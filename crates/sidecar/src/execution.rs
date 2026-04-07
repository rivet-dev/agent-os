//! Process execution, networking, and runtime event handling extracted from service.rs.

use crate::filesystem::{
    handle_python_vfs_rpc_request as filesystem_handle_python_vfs_rpc_request,
    service_javascript_fs_sync_rpc,
};
use crate::protocol::{
    BoundUdpSnapshotResponse, CloseStdinRequest, EventFrame, EventPayload, ExecuteRequest,
    FindBoundUdpRequest, FindListenerRequest, GetSignalStateRequest, GetZombieTimerCountRequest,
    GuestRuntimeKind, JavascriptChildProcessSpawnRequest, JavascriptDgramBindRequest,
    JavascriptDgramCreateSocketRequest, JavascriptDgramSendRequest, JavascriptDnsLookupRequest,
    JavascriptDnsResolveRequest, JavascriptNetConnectRequest, JavascriptNetListenRequest,
    KillProcessRequest, ListenerSnapshotResponse, OwnershipScope, ProcessExitedEvent,
    ProcessKilledResponse, ProcessOutputEvent, ProcessStartedResponse, RequestFrame,
    ResponsePayload, SidecarRequestPayload, SignalDispositionAction, SignalHandlerRegistration,
    SignalStateResponse, SocketStateEntry, StdinClosedResponse, StdinWrittenResponse,
    StreamChannel, WasmPermissionTier, WriteStdinRequest, ZombieTimerCountResponse,
};
use crate::service::{
    audit_fields, dirname, emit_security_audit_event, emit_structured_event, javascript_error,
    kernel_error, normalize_host_path, normalize_path, path_is_within_root, python_error,
    wasm_error,
};
use crate::state::{
    ActiveCipherSession, ActiveDhSession, ActiveDiffieHellmanSession, ActiveEcdhSession,
    ActiveExecution, ActiveExecutionEvent, ActiveHttp2Server, ActiveHttp2Session,
    ActiveHttp2Stream, ActiveHttpServer, ActiveProcess, ActiveSqliteDatabase,
    ActiveSqliteStatement, ActiveTcpListener, ActiveTcpSocket, ActiveTlsState, ActiveTlsStream,
    ActiveUdpSocket, ActiveUnixListener, ActiveUnixSocket, BridgeError,
    DEFAULT_JAVASCRIPT_NET_BACKLOG, DnsResolutionSource, EXECUTION_DRIVER_NAME,
    EXECUTION_SANDBOX_ROOT_ENV, Http2BridgeEvent, Http2RuntimeSnapshot, Http2SessionCommand,
    Http2SessionSnapshot, Http2SocketSnapshot, Http2StreamDirection, JAVASCRIPT_COMMAND,
    JavascriptSocketFamily, JavascriptSocketPathContext, JavascriptTcpListenerEvent,
    JavascriptTcpSocketEvent, JavascriptTlsBridgeOptions, JavascriptTlsClientHello,
    JavascriptTlsDataValue, JavascriptTlsMaterial, JavascriptUdpFamily, JavascriptUdpSocketEvent,
    JavascriptUnixListenerEvent, LOOPBACK_EXEMPT_PORTS_ENV, NetworkResourceCounts, PYTHON_COMMAND,
    PendingTcpSocket, PendingUnixSocket, ProcNetEntry, ProcessEventEnvelope,
    ResolvedChildProcessExecution, ResolvedTcpConnectAddr, SharedBridge, SidecarKernel,
    SocketQueryKind, TOOL_DRIVER_NAME, ToolExecution, VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY,
    VM_LISTEN_PORT_MAX_METADATA_KEY, VM_LISTEN_PORT_MIN_METADATA_KEY, VmDnsConfig, VmListenPolicy,
    VmState, WASM_COMMAND,
};
use crate::tools::{ToolCommandResolution, format_tool_failure_output, resolve_tool_command};
use crate::{DispatchResult, NativeSidecar, NativeSidecarBridge, SidecarError};

use agent_os_bridge::LifecycleState;
use agent_os_execution::wasm::{
    WASM_MAX_FUEL_ENV, WASM_MAX_MEMORY_BYTES_ENV, WASM_MAX_STACK_BYTES_ENV,
};
use agent_os_execution::{
    CreateJavascriptContextRequest, CreatePythonContextRequest, CreateWasmContextRequest,
    JavascriptExecutionEvent, JavascriptSyncRpcRequest, NodeSignalDispositionAction,
    NodeSignalHandlerRegistration, PythonExecutionEvent, PythonVfsRpcRequest,
    PythonVfsRpcResponsePayload, StartJavascriptExecutionRequest, StartPythonExecutionRequest,
    StartWasmExecutionRequest, WasmExecutionEvent,
    WasmPermissionTier as ExecutionWasmPermissionTier,
};
use agent_os_kernel::kernel::{KernelProcessHandle, SpawnOptions, VirtualProcessOptions};
use agent_os_kernel::permissions::NetworkOperation;
use agent_os_kernel::process_table::{SIGKILL, SIGTERM};
use agent_os_kernel::pty::LineDisciplineConfig;
use agent_os_kernel::resource_accounting::ResourceLimits;
use base64::Engine;
use bytes::Bytes;
use h2::{Reason, client, server};
use hickory_resolver::TokioResolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hmac::{Hmac, Mac};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, Uri};
use md5::Md5;
use nix::libc;
use nix::sys::signal::{Signal, kill as send_signal};
use nix::sys::wait::{Id as WaitId, WaitPidFlag, WaitStatus, waitid as wait_on_child};
use nix::unistd::Pid;
use openssl::bn::{BigNum, BigNumContext};
use openssl::derive::Deriver;
use openssl::dh::Dh;
use openssl::ec::{EcGroup, EcKey, EcPoint, PointConversionForm};
use openssl::hash::MessageDigest;
use openssl::md::Md;
use openssl::nid::Nid;
use openssl::pkcs5;
use openssl::pkey::{Id as PKeyId, PKey, Params, Private, Public};
use openssl::rand::rand_bytes;
use openssl::rsa::{Padding, Rsa};
use openssl::sign::{Signer, Verifier};
use openssl::symm::{Cipher, Crypter, Mode};
use pbkdf2::pbkdf2_hmac;
use rusqlite::types::ValueRef as SqliteValueRef;
use rusqlite::{
    Connection as SqliteConnection, OpenFlags as SqliteOpenFlags, Statement as SqliteStatement,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, ServerConfig,
    ServerConnection, SignatureScheme,
};
use scrypt::{Params as ScryptParams, scrypt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha1::Sha1;
use sha2::{Sha256, Sha512, digest::Digest};
use socket2::{SockRef, TcpKeepalive};
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs,
    UdpSocket,
};
use std::os::unix::net::{SocketAddr as UnixSocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use url::Url;

const DEFAULT_KERNEL_STDIN_READ_MAX_BYTES: usize = 64 * 1024;
const DEFAULT_KERNEL_STDIN_READ_TIMEOUT_MS: u64 = 100;
const JAVASCRIPT_NET_TIMEOUT_SENTINEL: &str = "__secure_exec_net_timeout__";
const TCP_SOCKET_POLL_TIMEOUT: Duration = Duration::from_millis(100);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HTTP_LOOPBACK_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_SCRYPT_COST: u64 = 16_384;
const DEFAULT_SCRYPT_BLOCK_SIZE: u32 = 8;
const DEFAULT_SCRYPT_PARALLELIZATION: u32 = 1;
const SQLITE_JS_SAFE_INTEGER_MAX: i64 = 9_007_199_254_740_991;
const DEFAULT_ALLOWED_NODE_BUILTINS: &[&str] = &[
    "assert",
    "buffer",
    "console",
    "child_process",
    "crypto",
    "dns",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "os",
    "path",
    "querystring",
    "sqlite",
    "stream",
    "string_decoder",
    "timers",
    "tls",
    "url",
    "util",
    "zlib",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JavascriptCryptoDigestAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavascriptScryptOptions {
    #[serde(alias = "N")]
    cost: Option<u64>,
    #[serde(alias = "r")]
    block_size: Option<u32>,
    #[serde(alias = "p")]
    parallelization: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JavascriptHttpListenRequest {
    server_id: u64,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavascriptHttpRequestOptions {
    method: Option<String>,
    headers: BTreeMap<String, Value>,
    body: Option<String>,
    reject_unauthorized: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavascriptHttp2ServerListenRequest {
    server_id: u64,
    secure: bool,
    port: Option<u16>,
    host: Option<String>,
    backlog: Option<u32>,
    timeout: Option<u64>,
    settings: BTreeMap<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavascriptHttp2SessionConnectRequest {
    authority: Option<String>,
    protocol: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    settings: BTreeMap<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavascriptHttp2RequestOptions {
    end_stream: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavascriptHttp2FileResponseOptions {
    offset: Option<u64>,
    length: Option<i64>,
}

#[derive(Debug, Clone)]
struct HttpHeaderCollection {
    normalized: BTreeMap<String, Vec<String>>,
    raw_pairs: Vec<(String, String)>,
}

#[derive(Debug)]
struct InsecureTlsVerifier {
    supported_schemes: Vec<SignatureScheme>,
}

impl ServerCertVerifier for InsecureTlsVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_schemes.clone()
    }
}

impl ActiveProcess {
    pub(crate) fn new(
        kernel_pid: u32,
        kernel_handle: KernelProcessHandle,
        runtime: GuestRuntimeKind,
        execution: ActiveExecution,
    ) -> Self {
        Self {
            kernel_pid,
            kernel_handle,
            kernel_stdin_writer_fd: None,
            runtime,
            execution,
            host_cwd: PathBuf::from("/"),
            child_processes: BTreeMap::new(),
            next_child_process_id: 0,
            http_servers: BTreeMap::new(),
            pending_http_requests: BTreeMap::new(),
            http2: Default::default(),
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
            cipher_sessions: BTreeMap::new(),
            next_cipher_session_id: 0,
            diffie_hellman_sessions: BTreeMap::new(),
            next_diffie_hellman_session_id: 0,
            sqlite_databases: BTreeMap::new(),
            next_sqlite_database_id: 0,
            sqlite_statements: BTreeMap::new(),
            next_sqlite_statement_id: 0,
        }
    }

    pub(crate) fn with_host_cwd(mut self, host_cwd: PathBuf) -> Self {
        self.host_cwd = host_cwd;
        self
    }

    pub(crate) fn with_kernel_stdin_writer_fd(mut self, fd: u32) -> Self {
        self.kernel_stdin_writer_fd = Some(fd);
        self
    }

    pub(crate) fn allocate_child_process_id(&mut self) -> String {
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

    pub(crate) fn network_resource_counts(&self) -> NetworkResourceCounts {
        let mut counts = NetworkResourceCounts {
            sockets: self.http_servers.len()
                + self.tcp_listeners.len()
                + self.tcp_sockets.len()
                + self.unix_listeners.len()
                + self.unix_sockets.len()
                + self.udp_sockets.len(),
            connections: self.tcp_sockets.len() + self.unix_sockets.len(),
        };
        if let Ok(http2) = self.http2.shared.lock() {
            counts.sockets += http2.servers.len() + http2.sessions.len();
            counts.connections += http2.sessions.len();
        }

        for child in self.child_processes.values() {
            let child_counts = child.network_resource_counts();
            counts.sockets += child_counts.sockets;
            counts.connections += child_counts.connections;
        }

        counts
    }
}

// TCP types moved to crate::state

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
        read_stream
            .set_read_timeout(Some(TCP_SOCKET_POLL_TIMEOUT))
            .map_err(sidecar_net_error)?;
        let stream = Arc::new(Mutex::new(stream));
        let pending_read_stream = Arc::new(Mutex::new(Some(read_stream)));
        let (sender, events) = mpsc::channel();
        let tls_mode = Arc::new(AtomicBool::new(false));
        let tls_stream = Arc::new(Mutex::new(None));
        let tls_state = Arc::new(Mutex::new(None));
        let saw_local_shutdown = Arc::new(AtomicBool::new(false));
        let saw_remote_end = Arc::new(AtomicBool::new(false));
        let close_notified = Arc::new(AtomicBool::new(false));

        Ok(Self {
            stream,
            pending_read_stream,
            events,
            event_sender: sender,
            guest_local_addr,
            guest_remote_addr,
            listener_id,
            tls_mode,
            tls_stream,
            tls_state,
            saw_local_shutdown,
            saw_remote_end,
            close_notified,
        })
    }

    fn poll(&mut self, wait: Duration) -> Result<Option<JavascriptTcpSocketEvent>, SidecarError> {
        self.ensure_tcp_reader()?;
        match self.events.recv_timeout(wait) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    fn ensure_tcp_reader(&self) -> Result<(), SidecarError> {
        if self.tls_mode.load(Ordering::SeqCst) {
            return Ok(());
        }
        let read_stream = self
            .pending_read_stream
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("TCP socket reader lock poisoned"))
            })?
            .take();
        if let Some(read_stream) = read_stream {
            spawn_tcp_socket_reader(
                read_stream,
                self.event_sender.clone(),
                Arc::clone(&self.tls_mode),
                Arc::clone(&self.saw_local_shutdown),
                Arc::clone(&self.saw_remote_end),
                Arc::clone(&self.close_notified),
            );
        }
        Ok(())
    }

    fn socket_info(&self) -> Value {
        json!({
            "localAddress": self.guest_local_addr.ip().to_string(),
            "localPort": self.guest_local_addr.port(),
            "localFamily": socket_addr_family(&self.guest_local_addr),
            "remoteAddress": self.guest_remote_addr.ip().to_string(),
            "remotePort": self.guest_remote_addr.port(),
            "remoteFamily": socket_addr_family(&self.guest_remote_addr),
        })
    }

    fn set_no_delay(&self, enable: bool) -> Result<(), SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        stream.set_nodelay(enable).map_err(sidecar_net_error)
    }

    fn set_keep_alive(
        &self,
        enable: bool,
        initial_delay_secs: Option<u64>,
    ) -> Result<(), SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        let socket = SockRef::from(&*stream);
        socket.set_keepalive(enable).map_err(sidecar_net_error)?;
        if enable {
            if let Some(delay_secs) = initial_delay_secs {
                socket
                    .set_tcp_keepalive(
                        &TcpKeepalive::new().with_time(Duration::from_secs(delay_secs)),
                    )
                    .map_err(sidecar_net_error)?;
            }
        }
        Ok(())
    }

    fn upgrade_tls(&self, options: JavascriptTlsBridgeOptions) -> Result<(), SidecarError> {
        if self.tls_mode.load(Ordering::SeqCst) {
            return Ok(());
        }

        let client_hello = if options.is_server {
            self.peek_tls_client_hello()?
        } else {
            None
        };

        self.pending_read_stream
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("TCP socket reader lock poisoned"))
            })?
            .take();

        let tls_stream = {
            let stream = self.stream.lock().map_err(|_| {
                SidecarError::InvalidState(String::from("TCP socket lock poisoned"))
            })?;
            let cloned = stream.try_clone().map_err(sidecar_net_error)?;
            drop(stream);

            if options.is_server {
                ActiveTlsStream::Server(build_server_tls_stream(cloned, &options)?)
            } else {
                ActiveTlsStream::Client(build_client_tls_stream(cloned, &options)?)
            }
        };

        let tls_state = ActiveTlsState {
            authorized: true,
            authorization_error: None,
            client_hello,
            local_certificates: tls_local_certificates(&options)?,
            session_reused: false,
        };

        self.tls_mode.store(true, Ordering::SeqCst);
        {
            let mut state = self
                .tls_state
                .lock()
                .map_err(|_| SidecarError::InvalidState(String::from("TLS state lock poisoned")))?;
            *state = Some(tls_state);
        }
        {
            let mut stream = self.tls_stream.lock().map_err(|_| {
                SidecarError::InvalidState(String::from("TLS stream lock poisoned"))
            })?;
            *stream = Some(tls_stream);
        }

        spawn_tls_socket_reader(
            Arc::clone(&self.tls_stream),
            self.event_sender.clone(),
            Arc::clone(&self.saw_local_shutdown),
            Arc::clone(&self.saw_remote_end),
            Arc::clone(&self.close_notified),
        );
        Ok(())
    }

    fn peek_tls_client_hello(&self) -> Result<Option<JavascriptTlsClientHello>, SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        let mut buffer = vec![0_u8; 16 * 1024];
        let bytes = match stream.peek(&mut buffer) {
            Ok(0) => return Ok(None),
            Ok(bytes) => bytes,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(sidecar_net_error(error)),
        };

        let mut acceptor = rustls::server::Acceptor::default();
        let mut cursor = Cursor::new(&buffer[..bytes]);
        acceptor.read_tls(&mut cursor).map_err(sidecar_net_error)?;
        let Some(accepted) = acceptor.accept().map_err(|(error, _)| {
            SidecarError::Execution(format!("failed to parse TLS client hello: {error}"))
        })?
        else {
            return Ok(None);
        };
        let client_hello = accepted.client_hello();
        let alpn_protocols = client_hello.alpn().map(|protocols| {
            protocols
                .filter_map(|protocol| String::from_utf8(protocol.to_vec()).ok())
                .collect::<Vec<_>>()
        });
        Ok(Some(JavascriptTlsClientHello {
            servername: client_hello.server_name().map(str::to_owned),
            alpn_protocols,
        }))
    }

    fn tls_client_hello_json(&self) -> Result<Value, SidecarError> {
        if let Some(client_hello) = self
            .tls_state
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TLS state lock poisoned")))?
            .as_ref()
            .and_then(|state| state.client_hello.clone())
        {
            return javascript_net_json_string(
                serde_json::to_value(client_hello).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "failed to serialize TLS client hello: {error}"
                    ))
                })?,
                "net.socket_get_tls_client_hello",
            );
        }

        javascript_net_json_string(
            serde_json::to_value(self.peek_tls_client_hello()?.unwrap_or_default()).map_err(
                |error| {
                    SidecarError::InvalidState(format!(
                        "failed to serialize TLS client hello: {error}"
                    ))
                },
            )?,
            "net.socket_get_tls_client_hello",
        )
    }

    fn tls_query(&self, query: &str, detailed: bool) -> Result<Value, SidecarError> {
        let state = self
            .tls_state
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TLS state lock poisoned")))?
            .clone();
        let mut tls_stream = self
            .tls_stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TLS stream lock poisoned")))?;
        let Some(stream) = tls_stream.as_mut() else {
            return javascript_net_json_string(
                tls_bridge_undefined_value(),
                "net.socket_tls_query",
            );
        };

        let payload = match query {
            "getSession" => tls_bridge_undefined_value(),
            "isSessionReused" => Value::Bool(
                state
                    .as_ref()
                    .is_some_and(|tls_state| tls_state.session_reused),
            ),
            "getPeerCertificate" => {
                let certificate = stream
                    .peer_certificates()
                    .and_then(|certificates| certificates.first())
                    .map(|certificate| {
                        tls_certificate_bridge_value(certificate.as_ref(), detailed)
                    });
                certificate.unwrap_or_else(tls_bridge_undefined_value)
            }
            "getCertificate" => state
                .as_ref()
                .and_then(|tls_state| tls_state.local_certificates.first())
                .map(|certificate| tls_certificate_bridge_value(certificate, detailed))
                .unwrap_or_else(tls_bridge_undefined_value),
            "getProtocol" => stream
                .protocol_version()
                .map(tls_protocol_name)
                .map(Value::String)
                .unwrap_or(Value::Null),
            "getCipher" => stream
                .negotiated_cipher_suite()
                .map(tls_cipher_bridge_value)
                .unwrap_or_else(tls_bridge_undefined_value),
            other => {
                return Err(SidecarError::InvalidState(format!(
                    "unsupported TLS query {other}"
                )));
            }
        };
        javascript_net_json_string(payload, "net.socket_tls_query")
    }

    fn write_all(&self, contents: &[u8]) -> Result<usize, SidecarError> {
        if self.tls_mode.load(Ordering::SeqCst) {
            let mut tls_stream = self.tls_stream.lock().map_err(|_| {
                SidecarError::InvalidState(String::from("TLS stream lock poisoned"))
            })?;
            let stream = tls_stream.as_mut().ok_or_else(|| {
                SidecarError::InvalidState(String::from("TLS stream missing for upgraded socket"))
            })?;
            stream.write_all(contents)?;
            return Ok(contents.len());
        }

        let mut stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        stream.write_all(contents).map_err(sidecar_net_error)?;
        Ok(contents.len())
    }

    fn shutdown_write(&self) -> Result<(), SidecarError> {
        if self.tls_mode.load(Ordering::SeqCst) {
            if let Some(stream) = self
                .tls_stream
                .lock()
                .map_err(|_| SidecarError::InvalidState(String::from("TLS stream lock poisoned")))?
                .as_mut()
            {
                let _ = stream.send_close_notify();
            }
        }
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
        if self.tls_mode.load(Ordering::SeqCst) {
            if let Some(stream) = self
                .tls_stream
                .lock()
                .map_err(|_| SidecarError::InvalidState(String::from("TLS stream lock poisoned")))?
                .as_mut()
            {
                let _ = stream.send_close_notify();
            }
        }
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        stream.shutdown(Shutdown::Both).map_err(sidecar_net_error)
    }
}

impl ActiveTlsStream {
    fn write_all(&mut self, contents: &[u8]) -> Result<(), SidecarError> {
        match self {
            Self::Client(stream) => {
                stream.write_all(contents).map_err(sidecar_net_error)?;
                stream.flush().map_err(sidecar_net_error)
            }
            Self::Server(stream) => {
                stream.write_all(contents).map_err(sidecar_net_error)?;
                stream.flush().map_err(sidecar_net_error)
            }
        }
    }

    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Client(stream) => stream.read(buffer),
            Self::Server(stream) => stream.read(buffer),
        }
    }

    fn send_close_notify(&mut self) -> Result<(), SidecarError> {
        match self {
            Self::Client(stream) => {
                stream.conn.send_close_notify();
                let _ = stream.conn.complete_io(&mut stream.sock);
            }
            Self::Server(stream) => {
                stream.conn.send_close_notify();
                let _ = stream.conn.complete_io(&mut stream.sock);
            }
        }
        Ok(())
    }

    fn peer_certificates(&self) -> Option<&[CertificateDer<'static>]> {
        match self {
            Self::Client(stream) => stream.conn.peer_certificates(),
            Self::Server(stream) => stream.conn.peer_certificates(),
        }
    }

    fn negotiated_cipher_suite(&self) -> Option<rustls::SupportedCipherSuite> {
        match self {
            Self::Client(stream) => stream.conn.negotiated_cipher_suite(),
            Self::Server(stream) => stream.conn.negotiated_cipher_suite(),
        }
    }

    fn protocol_version(&self) -> Option<rustls::ProtocolVersion> {
        match self {
            Self::Client(stream) => stream.conn.protocol_version(),
            Self::Server(stream) => stream.conn.protocol_version(),
        }
    }
}

// ActiveTcpListener moved to crate::state

// Unix socket types moved to crate::state

impl ActiveUnixSocket {
    fn connect(host_path: &Path, guest_path: &str) -> Result<Self, SidecarError> {
        let stream = UnixStream::connect(host_path).map_err(sidecar_net_error)?;
        Self::from_stream(stream, None, None, Some(guest_path.to_owned()))
    }

    fn from_stream(
        stream: UnixStream,
        listener_id: Option<String>,
        local_path: Option<String>,
        remote_path: Option<String>,
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
            local_path,
            remote_path,
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

    fn socket_info(&self) -> Value {
        json!({
            "localPath": self.local_path.clone(),
            "remotePath": self.remote_path.clone(),
        })
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

// ActiveUnixListener moved to crate::state

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

    pub(crate) fn local_addr(&self) -> SocketAddr {
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

// UDP types moved to crate::state

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

    fn socket(&self) -> Result<&UdpSocket, SidecarError> {
        self.socket
            .as_ref()
            .ok_or_else(|| SidecarError::Execution(String::from("EBADF: bad file descriptor")))
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
        let socket = self.socket()?;
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

    fn set_buffer_size(&self, which: &str, size: usize) -> Result<(), SidecarError> {
        let socket = self.socket()?;
        let socket = SockRef::from(socket);
        match which {
            "recv" => socket.set_recv_buffer_size(size).map_err(sidecar_net_error),
            "send" => socket.set_send_buffer_size(size).map_err(sidecar_net_error),
            other => Err(SidecarError::InvalidState(format!(
                "unsupported UDP buffer size kind {other}"
            ))),
        }
    }

    fn get_buffer_size(&self, which: &str) -> Result<usize, SidecarError> {
        let socket = self.socket()?;
        let socket = SockRef::from(socket);
        match which {
            "recv" => socket.recv_buffer_size().map_err(sidecar_net_error),
            "send" => socket.send_buffer_size().map_err(sidecar_net_error),
            other => Err(SidecarError::InvalidState(format!(
                "unsupported UDP buffer size kind {other}"
            ))),
        }
    }
}

// ActiveExecution, ActiveExecutionEvent, SocketQueryKind moved to crate::state

impl ActiveExecution {
    pub(crate) fn child_pid(&self) -> u32 {
        match self {
            Self::Javascript(execution) => execution.child_pid(),
            Self::Python(execution) => execution.child_pid(),
            Self::Wasm(execution) => execution.child_pid(),
            Self::Tool(_) => 0,
        }
    }

    pub(crate) fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), SidecarError> {
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
            Self::Tool(_) => Ok(()),
        }
    }

    pub(crate) fn close_stdin(&mut self) -> Result<(), SidecarError> {
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
            Self::Tool(_) => Ok(()),
        }
    }

    pub(crate) fn respond_python_vfs_rpc_success(
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

    pub(crate) fn respond_python_vfs_rpc_error(
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

    pub(crate) fn send_javascript_stream_event(
        &self,
        event_type: &str,
        payload: Value,
    ) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .send_stream_event(event_type, payload)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            _ => Err(SidecarError::InvalidState(String::from(
                "only JavaScript executions can receive JavaScript stream events",
            ))),
        }
    }

    pub(crate) fn respond_javascript_sync_rpc_success(
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

    pub(crate) fn respond_javascript_sync_rpc_error(
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

    pub(crate) async fn poll_event(
        &self,
        timeout: Duration,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .poll_event(timeout)
                .await
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
                .await
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
                .await
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
            Self::Tool(_) => {
                let _ = timeout;
                Ok(None)
            }
        }
    }

    pub(crate) fn poll_event_blocking(
        &self,
        timeout: Duration,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .poll_event_blocking(timeout)
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
                .poll_event_blocking(timeout)
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
                .poll_event_blocking(timeout)
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
            Self::Tool(_) => {
                let _ = timeout;
                Ok(None)
            }
        }
    }
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) async fn execute(
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

        if let Some(command) = payload.command.as_deref() {
            if let Some(tool_resolution) =
                resolve_tool_command(vm, command, &payload.args, payload.cwd.as_deref())?
            {
                let guest_cwd = payload
                    .cwd
                    .as_deref()
                    .map(normalize_path)
                    .unwrap_or_else(|| vm.guest_cwd.clone());
                let kernel_handle = vm
                    .kernel
                    .create_virtual_process(
                        EXECUTION_DRIVER_NAME,
                        TOOL_DRIVER_NAME,
                        command,
                        std::iter::once(command.to_owned())
                            .chain(payload.args.iter().cloned())
                            .collect(),
                        VirtualProcessOptions {
                            env: vm.guest_env.clone(),
                            cwd: Some(guest_cwd),
                            ..VirtualProcessOptions::default()
                        },
                    )
                    .map_err(kernel_error)?;
                let kernel_pid = kernel_handle.pid();
                let tool_execution = ToolExecution::default();
                let cancelled = tool_execution.cancelled.clone();
                vm.active_processes.insert(
                    payload.process_id.clone(),
                    ActiveProcess::new(
                        kernel_pid,
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Tool(tool_execution),
                    ),
                );
                self.bridge.emit_lifecycle(&vm_id, LifecycleState::Busy)?;

                let sender = self.process_event_sender.clone();
                let vm_id_for_thread = vm_id.clone();
                let process_id_for_thread = payload.process_id.clone();
                let connection_id_for_thread = connection_id.clone();
                let session_id_for_thread = session_id.clone();
                let sidecar_requests = self.sidecar_requests.clone();

                std::thread::spawn(move || match tool_resolution {
                    ToolCommandResolution::Immediate {
                        stdout,
                        stderr,
                        exit_code,
                    } => {
                        if cancelled.load(Ordering::Relaxed) {
                            return;
                        }
                        if !stdout.is_empty() {
                            let _ = sender.send(ProcessEventEnvelope {
                                connection_id: connection_id_for_thread.clone(),
                                session_id: session_id_for_thread.clone(),
                                vm_id: vm_id_for_thread.clone(),
                                process_id: process_id_for_thread.clone(),
                                event: ActiveExecutionEvent::Stdout(stdout),
                            });
                        }
                        if !stderr.is_empty() {
                            let _ = sender.send(ProcessEventEnvelope {
                                connection_id: connection_id_for_thread.clone(),
                                session_id: session_id_for_thread.clone(),
                                vm_id: vm_id_for_thread.clone(),
                                process_id: process_id_for_thread.clone(),
                                event: ActiveExecutionEvent::Stderr(stderr),
                            });
                        }
                        let _ = sender.send(ProcessEventEnvelope {
                            connection_id: connection_id_for_thread,
                            session_id: session_id_for_thread,
                            vm_id: vm_id_for_thread,
                            process_id: process_id_for_thread,
                            event: ActiveExecutionEvent::Exited(exit_code),
                        });
                    }
                    ToolCommandResolution::Invoke { request, timeout } => {
                        let response = sidecar_requests.invoke(
                            OwnershipScope::vm(
                                connection_id_for_thread.clone(),
                                session_id_for_thread.clone(),
                                vm_id_for_thread.clone(),
                            ),
                            SidecarRequestPayload::ToolInvocation(request.clone()),
                            timeout,
                        );
                        if cancelled.load(Ordering::Relaxed) {
                            return;
                        }

                        match response {
                            Ok(crate::protocol::SidecarResponsePayload::ToolInvocationResult(
                                result,
                            )) => {
                                if let Some(value) = result.result {
                                    let stdout = serde_json::to_vec(&json!({
                                        "ok": true,
                                        "result": value,
                                    }))
                                    .unwrap_or_else(|error| {
                                        format_tool_failure_output(&format!(
                                            "failed to serialize tool result: {error}"
                                        ))
                                    });
                                    let _ = sender.send(ProcessEventEnvelope {
                                        connection_id: connection_id_for_thread.clone(),
                                        session_id: session_id_for_thread.clone(),
                                        vm_id: vm_id_for_thread.clone(),
                                        process_id: process_id_for_thread.clone(),
                                        event: ActiveExecutionEvent::Stdout(stdout),
                                    });
                                    let _ = sender.send(ProcessEventEnvelope {
                                        connection_id: connection_id_for_thread,
                                        session_id: session_id_for_thread,
                                        vm_id: vm_id_for_thread,
                                        process_id: process_id_for_thread,
                                        event: ActiveExecutionEvent::Exited(0),
                                    });
                                } else {
                                    let message = result.error.unwrap_or_else(|| {
                                        String::from("tool invocation returned no result")
                                    });
                                    let _ = sender.send(ProcessEventEnvelope {
                                        connection_id: connection_id_for_thread.clone(),
                                        session_id: session_id_for_thread.clone(),
                                        vm_id: vm_id_for_thread.clone(),
                                        process_id: process_id_for_thread.clone(),
                                        event: ActiveExecutionEvent::Stderr(
                                            format_tool_failure_output(&message),
                                        ),
                                    });
                                    let _ = sender.send(ProcessEventEnvelope {
                                        connection_id: connection_id_for_thread,
                                        session_id: session_id_for_thread,
                                        vm_id: vm_id_for_thread,
                                        process_id: process_id_for_thread,
                                        event: ActiveExecutionEvent::Exited(1),
                                    });
                                }
                            }
                            Ok(_) => {
                                let _ = sender.send(ProcessEventEnvelope {
                                    connection_id: connection_id_for_thread.clone(),
                                    session_id: session_id_for_thread.clone(),
                                    vm_id: vm_id_for_thread.clone(),
                                    process_id: process_id_for_thread.clone(),
                                    event: ActiveExecutionEvent::Stderr(
                                        format_tool_failure_output(
                                            "unexpected sidecar tool response",
                                        ),
                                    ),
                                });
                                let _ = sender.send(ProcessEventEnvelope {
                                    connection_id: connection_id_for_thread,
                                    session_id: session_id_for_thread,
                                    vm_id: vm_id_for_thread,
                                    process_id: process_id_for_thread,
                                    event: ActiveExecutionEvent::Exited(1),
                                });
                            }
                            Err(error) => {
                                let _ = sender.send(ProcessEventEnvelope {
                                    connection_id: connection_id_for_thread.clone(),
                                    session_id: session_id_for_thread.clone(),
                                    vm_id: vm_id_for_thread.clone(),
                                    process_id: process_id_for_thread.clone(),
                                    event: ActiveExecutionEvent::Stderr(
                                        format_tool_failure_output(&error.to_string()),
                                    ),
                                });
                                let _ = sender.send(ProcessEventEnvelope {
                                    connection_id: connection_id_for_thread,
                                    session_id: session_id_for_thread,
                                    vm_id: vm_id_for_thread,
                                    process_id: process_id_for_thread,
                                    event: ActiveExecutionEvent::Exited(1),
                                });
                            }
                        }
                    }
                });

                return Ok(DispatchResult {
                    response: self.respond(
                        request,
                        ResponsePayload::ProcessStarted(ProcessStartedResponse {
                            process_id: payload.process_id,
                            pid: Some(kernel_pid),
                        }),
                    ),
                    events: Vec::new(),
                });
            }
        }

        let resolved = resolve_execute_request(vm, &payload)?;
        let mut env = resolved.env.clone();
        let sandbox_root = normalize_host_path(&vm.cwd);
        env.insert(
            String::from(EXECUTION_SANDBOX_ROOT_ENV),
            sandbox_root.to_string_lossy().into_owned(),
        );
        let argv = std::iter::once(resolved.entrypoint.clone())
            .chain(resolved.execution_args.iter().cloned())
            .collect::<Vec<_>>();
        let kernel_handle = vm
            .kernel
            .spawn_process(
                &resolved.command,
                argv,
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    cwd: Some(String::from("/")),
                    ..SpawnOptions::default()
                },
            )
            .map_err(kernel_error)?;

        let execution = match resolved.runtime {
            GuestRuntimeKind::JavaScript => {
                prepare_javascript_shadow(vm, &resolved)?;

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
                        argv: std::iter::once(resolved.entrypoint.clone())
                            .chain(resolved.execution_args.iter().cloned())
                            .collect(),
                        env: env.clone(),
                        cwd: resolved.host_cwd.clone(),
                        inline_code: None,
                    })
                    .map_err(javascript_error)?;
                ActiveExecution::Javascript(execution)
            }
            GuestRuntimeKind::Python => {
                let python_file_path = python_file_entrypoint(&resolved.entrypoint);
                let pyodide_dist_path = self
                    .python_engine
                    .bundled_pyodide_dist_path_for_vm(&vm_id)
                    .map_err(python_error)?;
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
                        code: resolved.entrypoint.clone(),
                        file_path: python_file_path,
                        env: env.clone(),
                        cwd: resolved.host_cwd.clone(),
                    })
                    .map_err(python_error)?;
                ActiveExecution::Python(execution)
            }
            GuestRuntimeKind::WebAssembly => {
                apply_wasm_limit_env(&mut env, vm.kernel.resource_limits());
                let wasm_permission_tier = resolved.wasm_permission_tier.unwrap_or_else(|| {
                    resolve_wasm_permission_tier(
                        vm,
                        Some(&resolved.command),
                        None,
                        &resolved.entrypoint,
                    )
                });
                let context = self.wasm_engine.create_context(CreateWasmContextRequest {
                    vm_id: vm_id.clone(),
                    module_path: Some(resolved.entrypoint.clone()),
                });
                let execution = self
                    .wasm_engine
                    .start_execution(StartWasmExecutionRequest {
                        vm_id: vm_id.clone(),
                        context_id: context.context_id,
                        argv: resolved.execution_args.clone(),
                        env,
                        cwd: resolved.host_cwd.clone(),
                        permission_tier: execution_wasm_permission_tier(wasm_permission_tier),
                    })
                    .map_err(wasm_error)?;
                ActiveExecution::Wasm(execution)
            }
        };
        let child_pid = execution.child_pid();
        let kernel_stdin_writer_fd =
            install_kernel_stdin_pipe(&mut vm.kernel, kernel_handle.pid())?;
        vm.active_processes.insert(
            payload.process_id.clone(),
            ActiveProcess::new(
                kernel_handle.pid(),
                kernel_handle,
                resolved.runtime,
                execution,
            )
            .with_kernel_stdin_writer_fd(kernel_stdin_writer_fd)
            .with_host_cwd(resolved.host_cwd.clone()),
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

    pub(crate) async fn write_stdin(
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
        write_kernel_process_stdin(&mut vm.kernel, process, payload.chunk.as_bytes())?;

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

    pub(crate) async fn close_stdin(
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
        close_kernel_process_stdin(&mut vm.kernel, process)?;

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

    pub(crate) async fn kill_process(
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

    pub(crate) async fn find_listener(
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

    pub(crate) async fn find_bound_udp(
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

    pub(crate) async fn get_signal_state(
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

    pub(crate) async fn get_zombie_timer_count(
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

    pub(crate) fn kill_process_internal(
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

        match &process.execution {
            ActiveExecution::Tool(execution) => {
                if signal != 0 {
                    execution.cancelled.store(true, Ordering::Relaxed);
                    let _ = self.process_event_sender.send(ProcessEventEnvelope {
                        connection_id: vm.connection_id.clone(),
                        session_id: vm.session_id.clone(),
                        vm_id: vm_id.to_owned(),
                        process_id: process_id.to_owned(),
                        event: ActiveExecutionEvent::Exited(128 + signal),
                    });
                }
            }
            ActiveExecution::Javascript(execution) if execution.child_pid() == 0 => {
                if signal != 0 {
                    execution
                        .terminate()
                        .map_err(|error| SidecarError::Execution(error.to_string()))?;
                }
            }
            _ => signal_runtime_process(process.execution.child_pid(), signal)?,
        }
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

    pub async fn pump_process_events(
        &mut self,
        ownership: &OwnershipScope,
    ) -> Result<bool, SidecarError> {
        let mut emitted_any = false;

        {
            let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
                SidecarError::InvalidState(String::from("process event receiver unavailable"))
            })?;
            loop {
                match receiver.try_recv() {
                    Ok(envelope) => {
                        self.pending_process_events.push_back(envelope);
                        emitted_any = true;
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
        }

        let vm_ids = self.vm_ids_for_scope(ownership)?;
        for vm_id in vm_ids {
            loop {
                let Some(vm) = self.vms.get(&vm_id) else {
                    break;
                };
                let connection_id = vm.connection_id.clone();
                let session_id = vm.session_id.clone();
                let process_ids = self
                    .vms
                    .get(&vm_id)
                    .map(|vm| vm.active_processes.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                let mut emitted_this_pass = false;

                for process_id in process_ids {
                    if self
                        .acp_terminal_owner_for_process(&vm_id, &process_id)
                        .is_some()
                    {
                        continue;
                    }
                    let event = {
                        let vm = self.vms.get_mut(&vm_id).expect("VM should still exist");
                        let process = vm
                            .active_processes
                            .get_mut(&process_id)
                            .expect("process should still exist");
                        // Treat a closed event channel as "no more events" rather than
                        // a hard error. The channel closes after the Exited event is
                        // sent, but the process isn't removed from active_processes
                        // until the envelope is dequeued and handled later.
                        match process.execution.poll_event(Duration::ZERO).await {
                            Ok(event) => event,
                            Err(SidecarError::Execution(_)) => None,
                            Err(other) => return Err(other),
                        }
                    };

                    let Some(event) = event else {
                        continue;
                    };

                    let _ = self.process_event_sender.send(ProcessEventEnvelope {
                        connection_id: connection_id.clone(),
                        session_id: session_id.clone(),
                        vm_id: vm_id.clone(),
                        process_id: process_id.clone(),
                        event,
                    });
                    emitted_any = true;
                    emitted_this_pass = true;
                }

                if !emitted_this_pass {
                    break;
                }
            }
        }

        Ok(emitted_any)
    }

    pub(crate) fn handle_execution_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
        event: ActiveExecutionEvent,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let Some(vm) = self.vms.get(vm_id) else {
            return Ok(None);
        };
        if !vm.active_processes.contains_key(process_id) {
            return Ok(None);
        }
        let (connection_id, session_id) = { (vm.connection_id.clone(), vm.session_id.clone()) };
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

    pub(crate) fn drain_process_events_blocking(
        &mut self,
        vm_id: &str,
        process_id: &str,
    ) -> Result<Vec<ActiveExecutionEvent>, SidecarError> {
        let mut events = Vec::new();

        loop {
            let event = {
                let Some(vm) = self.vms.get_mut(vm_id) else {
                    break;
                };
                let Some(process) = vm.active_processes.get_mut(process_id) else {
                    break;
                };
                match process.execution.poll_event_blocking(Duration::ZERO) {
                    Ok(event) => event,
                    Err(SidecarError::Execution(_)) => None,
                    Err(other) => return Err(other),
                }
            };

            let Some(event) = event else {
                break;
            };
            events.push(event);
        }

        Ok(events)
    }

    pub(crate) fn handle_python_vfs_rpc_request(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: PythonVfsRpcRequest,
    ) -> Result<(), SidecarError> {
        filesystem_handle_python_vfs_rpc_request(self, vm_id, process_id, request)
    }

    fn resolve_javascript_child_process_execution(
        &self,
        vm: &VmState,
        parent_host_cwd: &Path,
        request: &JavascriptChildProcessSpawnRequest,
    ) -> Result<ResolvedChildProcessExecution, SidecarError> {
        let mut runtime_env = vm.guest_env.clone();
        runtime_env.extend(request.options.internal_bootstrap_env.clone());
        let (guest_cwd, host_cwd_override) = request
            .options
            .cwd
            .as_deref()
            .map(|cwd| {
                let normalized_parent_host_cwd = normalize_host_path(parent_host_cwd);
                let requested_host_cwd = normalize_host_path(Path::new(cwd));
                if path_is_within_root(&requested_host_cwd, &normalized_parent_host_cwd) {
                    let relative = requested_host_cwd
                        .strip_prefix(&normalized_parent_host_cwd)
                        .unwrap_or_else(|_| Path::new(""));
                    let relative = relative.to_string_lossy().replace('\\', "/");
                    let guest_cwd = if relative.is_empty() {
                        String::from("/")
                    } else {
                        normalize_path(&format!("/{relative}"))
                    };
                    (guest_cwd, Some(requested_host_cwd))
                } else {
                    (normalize_path(cwd), None)
                }
            })
            .unwrap_or_else(|| (String::from("/"), None));
        let host_cwd = host_cwd_override
            .or_else(|| {
                host_runtime_path_for_guest_path_with_env(
                    vm,
                    &runtime_env,
                    &guest_cwd,
                    parent_host_cwd,
                )
            })
            .unwrap_or_else(|| {
                let candidate = PathBuf::from(&guest_cwd);
                if candidate.is_absolute() {
                    shadow_path_for_guest(vm, &guest_cwd)
                } else {
                    vm.host_cwd.clone()
                }
            });
        let mut env = vm.guest_env.clone();
        env.extend(request.options.env.clone());

        let (command, process_args) = if request.options.shell {
            if !command_requires_shell(&request.command) {
                let tokens = tokenize_shell_free_command(&request.command);
                let Some((command, args)) = tokens.split_first() else {
                    return Err(SidecarError::InvalidState(String::from(
                        "child_process shell command must not be empty",
                    )));
                };
                (command.clone(), args.to_vec())
            } else if vm.command_guest_paths.contains_key("sh") {
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
        if is_path_like_specifier(&command)
            && matches!(
                Path::new(&command).extension().and_then(|ext| ext.to_str()),
                Some("js" | "mjs" | "cjs" | "ts" | "mts" | "cts")
            )
        {
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
                host_runtime_path_for_guest_path_with_env(
                    vm,
                    &runtime_env,
                    &guest_entrypoint,
                    parent_host_cwd,
                )
                .unwrap_or_else(|| {
                    let candidate = PathBuf::from(&guest_entrypoint);
                    if candidate.is_absolute() {
                        candidate
                    } else {
                        host_cwd.join(&guest_entrypoint)
                    }
                })
            };
            env.entry(String::from("AGENT_OS_GUEST_ENTRYPOINT"))
                .or_insert(guest_entrypoint);

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
                    host_runtime_path_for_guest_path_with_env(
                        vm,
                        &runtime_env,
                        &guest_entrypoint,
                        parent_host_cwd,
                    )
                    .unwrap_or_else(|| {
                        let candidate = PathBuf::from(&guest_entrypoint);
                        if candidate.is_absolute() {
                            candidate
                        } else {
                            host_cwd.join(&guest_entrypoint)
                        }
                    })
                };
                env.entry(String::from("AGENT_OS_GUEST_ENTRYPOINT"))
                    .or_insert(guest_entrypoint);
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

        let guest_entrypoint = vm
            .command_guest_paths
            .get(&command)
            .ok_or_else(|| SidecarError::InvalidState(format!("command not found: {command}")))?;
        let host_entrypoint = host_runtime_path_for_guest_path_with_env(
            vm,
            &runtime_env,
            guest_entrypoint,
            parent_host_cwd,
        )
        .unwrap_or_else(|| {
            let candidate = PathBuf::from(guest_entrypoint);
            if candidate.is_absolute() {
                candidate
            } else {
                host_cwd.join(guest_entrypoint)
            }
        });
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

    pub(crate) fn spawn_javascript_child_process(
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
                execution_env.insert(String::from("AGENT_OS_KEEP_STDIN_OPEN"), String::from("1"));
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
                let inline_code = load_javascript_entrypoint_source(
                    vm,
                    &resolved.host_cwd,
                    &resolved.entrypoint,
                    &execution_env,
                );

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
                        inline_code,
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
        let kernel_stdin_writer_fd = install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid)?;

        vm.active_processes
            .get_mut(process_id)
            .expect("process should still exist")
            .child_processes
            .insert(
                child_process_id.clone(),
                ActiveProcess::new(kernel_pid, kernel_handle, resolved.runtime, execution)
                    .with_kernel_stdin_writer_fd(kernel_stdin_writer_fd)
                    .with_host_cwd(resolved.host_cwd.clone()),
            );

        Ok(json!({
            "childId": child_process_id,
            "pid": kernel_pid,
            "command": resolved.command,
            "args": resolved.process_args,
        }))
    }

    pub(crate) fn spawn_javascript_child_process_sync(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: JavascriptChildProcessSpawnRequest,
        max_buffer: Option<usize>,
    ) -> Result<Value, SidecarError> {
        let spawned = self.spawn_javascript_child_process(vm_id, process_id, request)?;
        let child_process_id = spawned
            .get("childId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "child_process.spawn_sync response is missing childId",
                ))
            })?
            .to_owned();

        let max_buffer = max_buffer.unwrap_or(1024 * 1024);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code = 0_i32;
        let mut max_buffer_exceeded = false;
        let mut kill_sent = false;

        loop {
            let event =
                self.poll_javascript_child_process(vm_id, process_id, &child_process_id, 50)?;
            if event.is_null() {
                continue;
            }

            match event.get("type").and_then(Value::as_str) {
                Some("stdout") => {
                    let chunk = javascript_sync_rpc_bytes_arg(
                        &[event.get("data").cloned().unwrap_or(Value::Null)],
                        0,
                        "child_process.spawn_sync stdout",
                    )?;
                    stdout.extend_from_slice(&chunk);
                    if stdout.len() > max_buffer && !kill_sent {
                        max_buffer_exceeded = true;
                        self.kill_javascript_child_process(
                            vm_id,
                            process_id,
                            &child_process_id,
                            "SIGTERM",
                        )?;
                        kill_sent = true;
                    }
                }
                Some("stderr") => {
                    let chunk = javascript_sync_rpc_bytes_arg(
                        &[event.get("data").cloned().unwrap_or(Value::Null)],
                        0,
                        "child_process.spawn_sync stderr",
                    )?;
                    stderr.extend_from_slice(&chunk);
                    if stderr.len() > max_buffer && !kill_sent {
                        max_buffer_exceeded = true;
                        self.kill_javascript_child_process(
                            vm_id,
                            process_id,
                            &child_process_id,
                            "SIGTERM",
                        )?;
                        kill_sent = true;
                    }
                }
                Some("exit") => {
                    exit_code = event
                        .get("exitCode")
                        .and_then(Value::as_i64)
                        .map(|value| value as i32)
                        .unwrap_or(1);
                    break;
                }
                _ => {}
            }
        }

        Ok(json!({
            "stdout": String::from_utf8_lossy(&stdout),
            "stderr": String::from_utf8_lossy(&stderr),
            "code": exit_code,
            "maxBufferExceeded": max_buffer_exceeded,
        }))
    }

    pub(crate) fn poll_javascript_child_process(
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
                            "unknown child process {child_process_id} during poll"
                        ))
                    });
                let Ok(child) = child else {
                    return Ok(json!({
                        "type": "exit",
                        "exitCode": 1,
                    }));
                };
                child
                    .execution
                    .poll_event_blocking(Duration::from_millis(wait_ms))?
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
                    let should_signal_parent = vm
                        .signal_states
                        .get(process_id)
                        .and_then(|handlers| handlers.get(&(libc::SIGCHLD as u32)))
                        .is_some_and(|registration| {
                            registration.action != SignalDispositionAction::Default
                        });
                    let child = vm
                        .active_processes
                        .get_mut(process_id)
                        .expect("process should still exist")
                        .child_processes
                        .remove(child_process_id)
                        .expect("child process should still exist");
                    child.kernel_handle.finish(exit_code);
                    let _ = vm.kernel.wait_and_reap(child.kernel_pid);
                    if should_signal_parent {
                        signal_runtime_process(parent_runtime_pid, libc::SIGCHLD)?;
                    }
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

    pub(crate) fn write_javascript_child_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
        chunk: &[u8],
    ) -> Result<(), SidecarError> {
        let vm = self.vms.get_mut(vm_id).expect("VM should exist");
        let Some(child) = vm
            .active_processes
            .get_mut(process_id)
            .expect("process should still exist")
            .child_processes
            .get_mut(child_process_id)
        else {
            return Ok(());
        };
        child.execution.write_stdin(chunk)?;
        write_kernel_process_stdin(&mut vm.kernel, child, chunk)
    }

    pub(crate) fn close_javascript_child_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let vm = self.vms.get_mut(vm_id).expect("VM should exist");
        let Some(child) = vm
            .active_processes
            .get_mut(process_id)
            .expect("process should still exist")
            .child_processes
            .get_mut(child_process_id)
        else {
            return Ok(());
        };
        child.execution.close_stdin()?;
        close_kernel_process_stdin(&mut vm.kernel, child)
    }

    pub(crate) fn kill_javascript_child_process(
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
                SidecarError::InvalidState(format!(
                    "unknown child process {child_process_id} during kill"
                ))
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

// bridge_permissions moved to crate::bridge

// reconcile_mounts, resolve_cwd moved to crate::vm

fn resolve_execute_request(
    vm: &VmState,
    payload: &ExecuteRequest,
) -> Result<ResolvedChildProcessExecution, SidecarError> {
    if let Some(command) = payload.command.as_deref() {
        return resolve_command_execution(
            vm,
            command,
            &payload.args,
            &payload.env,
            payload.cwd.as_deref(),
            payload.wasm_permission_tier,
        );
    }

    let runtime = payload.runtime.clone().ok_or_else(|| {
        SidecarError::InvalidState(String::from("execute requires either command or runtime"))
    })?;
    let entrypoint = payload.entrypoint.clone().ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "execute requires either command or entrypoint",
        ))
    })?;
    let (guest_cwd, host_cwd, allow_host_path_overrides) =
        resolve_execution_cwds(vm, payload.cwd.as_deref());
    let mut env = vm.guest_env.clone();
    env.extend(payload.env.clone());

    let requested_host_entrypoint = resolve_host_entrypoint_within_vm_host_cwd(vm, &entrypoint);
    if requested_host_entrypoint.is_some() && !allow_host_path_overrides {
        let requested_cwd = payload.cwd.as_deref().unwrap_or(guest_cwd.as_str());
        return Err(SidecarError::InvalidState(format!(
            "execution cwd {requested_cwd} is outside sandbox root {}",
            vm.host_cwd.to_string_lossy()
        )));
    }
    let host_entrypoint_override = allow_host_path_overrides
        .then(|| resolve_host_entrypoint_within_vm_host_cwd(vm, &entrypoint))
        .flatten();

    if runtime == GuestRuntimeKind::JavaScript {
        let guest_entrypoint = host_entrypoint_override
            .as_ref()
            .map(|(guest_entrypoint, _)| guest_entrypoint.clone())
            .or_else(|| guest_entrypoint_for_specifier(&guest_cwd, &entrypoint));
        prepare_javascript_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;
    }

    Ok(ResolvedChildProcessExecution {
        command: match runtime {
            GuestRuntimeKind::JavaScript => String::from(JAVASCRIPT_COMMAND),
            GuestRuntimeKind::Python => String::from(PYTHON_COMMAND),
            GuestRuntimeKind::WebAssembly => String::from(WASM_COMMAND),
        },
        process_args: std::iter::once(entrypoint.clone())
            .chain(payload.args.iter().cloned())
            .collect(),
        runtime,
        entrypoint: host_entrypoint_override
            .map(|(_, host_entrypoint)| host_entrypoint)
            .unwrap_or(entrypoint),
        execution_args: payload.args.clone(),
        env,
        guest_cwd,
        host_cwd,
        wasm_permission_tier: payload.wasm_permission_tier,
    })
}

fn resolve_command_execution(
    vm: &VmState,
    command: &str,
    args: &[String],
    extra_env: &BTreeMap<String, String>,
    cwd: Option<&str>,
    explicit_wasm_permission_tier: Option<WasmPermissionTier>,
) -> Result<ResolvedChildProcessExecution, SidecarError> {
    let (guest_cwd, host_cwd, allow_host_path_overrides) = resolve_execution_cwds(vm, cwd);
    let mut env = vm.guest_env.clone();
    env.extend(extra_env.clone());

    if matches!(command, "node" | "npm" | "npx") {
        let Some(entrypoint_specifier) = args.first() else {
            return Err(SidecarError::InvalidState(format!(
                "{command} execution requires an entrypoint"
            )));
        };

        let (entrypoint, execution_args, guest_entrypoint) =
            if matches!(entrypoint_specifier.as_str(), "-e" | "--eval") {
                env.insert(
                    String::from("AGENT_OS_NODE_EVAL"),
                    args.get(1).cloned().unwrap_or_default(),
                );
                (
                    entrypoint_specifier.clone(),
                    args.iter().skip(2).cloned().collect(),
                    None,
                )
            } else {
                let requested_host_entrypoint =
                    resolve_host_entrypoint_within_vm_host_cwd(vm, entrypoint_specifier);
                if requested_host_entrypoint.is_some() && !allow_host_path_overrides {
                    let requested_cwd = cwd.unwrap_or(guest_cwd.as_str());
                    return Err(SidecarError::InvalidState(format!(
                        "execution cwd {requested_cwd} is outside sandbox root {}",
                        vm.host_cwd.to_string_lossy()
                    )));
                }
                let host_entrypoint_override = allow_host_path_overrides
                    .then(|| resolve_host_entrypoint_within_vm_host_cwd(vm, entrypoint_specifier))
                    .flatten();
                let guest_entrypoint = host_entrypoint_override
                    .as_ref()
                    .map(|(guest_entrypoint, _)| guest_entrypoint.clone())
                    .or_else(|| guest_entrypoint_for_specifier(&guest_cwd, entrypoint_specifier));
                let entrypoint = host_entrypoint_override.map_or_else(
                    || {
                        guest_entrypoint.as_ref().map_or_else(
                            || entrypoint_specifier.clone(),
                            |guest_entrypoint| {
                                resolve_vm_guest_path_to_host(vm, guest_entrypoint)
                                    .to_string_lossy()
                                    .into_owned()
                            },
                        )
                    },
                    |(_, host_entrypoint)| host_entrypoint,
                );
                (
                    entrypoint,
                    args.iter().skip(1).cloned().collect(),
                    guest_entrypoint,
                )
            };

        prepare_javascript_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;

        return Ok(ResolvedChildProcessExecution {
            command: String::from(JAVASCRIPT_COMMAND),
            process_args: std::iter::once(command.to_owned())
                .chain(args.iter().cloned())
                .collect(),
            runtime: GuestRuntimeKind::JavaScript,
            entrypoint,
            execution_args,
            env,
            guest_cwd,
            host_cwd,
            wasm_permission_tier: None,
        });
    }

    if command.ends_with(".js") || command.ends_with(".mjs") || command.ends_with(".cjs") {
        let requested_host_entrypoint = resolve_host_entrypoint_within_vm_host_cwd(vm, command);
        if requested_host_entrypoint.is_some() && !allow_host_path_overrides {
            let requested_cwd = cwd.unwrap_or(guest_cwd.as_str());
            return Err(SidecarError::InvalidState(format!(
                "execution cwd {requested_cwd} is outside sandbox root {}",
                vm.host_cwd.to_string_lossy()
            )));
        }
        let host_entrypoint_override = allow_host_path_overrides
            .then(|| resolve_host_entrypoint_within_vm_host_cwd(vm, command))
            .flatten();
        let guest_entrypoint = host_entrypoint_override
            .as_ref()
            .map(|(guest_entrypoint, _)| guest_entrypoint.clone())
            .or_else(|| guest_entrypoint_for_specifier(&guest_cwd, command));
        let entrypoint = host_entrypoint_override.map_or_else(
            || {
                guest_entrypoint.as_ref().map_or_else(
                    || command.to_owned(),
                    |guest_entrypoint| {
                        resolve_vm_guest_path_to_host(vm, guest_entrypoint)
                            .to_string_lossy()
                            .into_owned()
                    },
                )
            },
            |(_, host_entrypoint)| host_entrypoint,
        );
        prepare_javascript_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;

        return Ok(ResolvedChildProcessExecution {
            command: String::from(JAVASCRIPT_COMMAND),
            process_args: std::iter::once(command.to_owned())
                .chain(args.iter().cloned())
                .collect(),
            runtime: GuestRuntimeKind::JavaScript,
            entrypoint,
            execution_args: args.to_vec(),
            env,
            guest_cwd,
            host_cwd,
            wasm_permission_tier: None,
        });
    }

    let guest_entrypoint = if is_path_like_specifier(command) {
        Some(resolve_path_like_guest_specifier(&guest_cwd, command))
    } else {
        vm.command_guest_paths.get(command).cloned()
    }
    .ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "command not found on native sidecar path: {command}"
        ))
    })?;
    let wasm_permission_tier = explicit_wasm_permission_tier
        .or_else(|| vm.command_permissions.get(command).copied())
        .or_else(|| {
            Path::new(&guest_entrypoint)
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| vm.command_permissions.get(name).copied())
        });

    let host_entrypoint = resolve_vm_guest_path_to_host(vm, &guest_entrypoint);

    Ok(ResolvedChildProcessExecution {
        command: String::from(WASM_COMMAND),
        process_args: std::iter::once(command.to_owned())
            .chain(args.iter().cloned())
            .collect(),
        runtime: GuestRuntimeKind::WebAssembly,
        entrypoint: host_entrypoint.to_string_lossy().into_owned(),
        execution_args: args.to_vec(),
        env,
        guest_cwd,
        host_cwd,
        wasm_permission_tier,
    })
}

fn resolve_guest_execution_cwd(vm: &VmState, value: Option<&str>) -> String {
    value
        .map(normalize_path)
        .unwrap_or_else(|| vm.guest_cwd.clone())
}

fn resolve_execution_cwds(vm: &VmState, value: Option<&str>) -> (String, PathBuf, bool) {
    if let Some(raw_cwd) = value {
        let normalized_vm_host_cwd = normalize_host_path(&vm.host_cwd);
        let requested_host_cwd = normalize_host_path(Path::new(raw_cwd));
        if path_is_within_root(&requested_host_cwd, &normalized_vm_host_cwd) {
            let relative = requested_host_cwd
                .strip_prefix(&normalized_vm_host_cwd)
                .unwrap_or_else(|_| Path::new(""));
            let relative = relative.to_string_lossy().replace('\\', "/");
            let guest_cwd = if relative.is_empty() {
                String::from("/")
            } else {
                normalize_path(&format!("/{relative}"))
            };
            return (guest_cwd, requested_host_cwd, true);
        }
    }

    let guest_cwd = resolve_guest_execution_cwd(vm, value);
    let host_cwd = if value.is_none() {
        vm.host_cwd.clone()
    } else {
        resolve_vm_guest_path_to_host(vm, &guest_cwd)
    };
    (guest_cwd, host_cwd, value.is_none())
}

fn resolve_vm_guest_path_to_host(vm: &VmState, guest_path: &str) -> PathBuf {
    host_mount_path_for_guest_path(vm, guest_path)
        .unwrap_or_else(|| shadow_path_for_guest(vm, guest_path))
}

fn shadow_path_for_guest(vm: &VmState, guest_path: &str) -> PathBuf {
    let normalized = normalize_path(guest_path);
    let relative = normalized.trim_start_matches('/');
    if relative.is_empty() {
        return vm.cwd.clone();
    }
    vm.cwd.join(relative)
}

fn resolve_path_like_guest_specifier(cwd: &str, specifier: &str) -> String {
    if specifier.starts_with("file://") {
        normalize_path(specifier.trim_start_matches("file://"))
    } else if specifier.starts_with("file:") {
        normalize_path(specifier.trim_start_matches("file:"))
    } else if specifier.starts_with('/') {
        normalize_path(specifier)
    } else {
        normalize_path(&format!("{cwd}/{specifier}"))
    }
}

fn guest_entrypoint_for_specifier(cwd: &str, specifier: &str) -> Option<String> {
    is_path_like_specifier(specifier).then(|| resolve_path_like_guest_specifier(cwd, specifier))
}

fn resolve_host_entrypoint_within_vm_host_cwd(
    vm: &VmState,
    specifier: &str,
) -> Option<(String, String)> {
    let candidate = Path::new(specifier);
    if !candidate.is_absolute() {
        return None;
    }

    let normalized_entrypoint = normalize_host_path(candidate);
    let normalized_host_cwd = normalize_host_path(&vm.host_cwd);
    if !path_is_within_root(&normalized_entrypoint, &normalized_host_cwd) {
        return None;
    }

    let relative = normalized_entrypoint
        .strip_prefix(&normalized_host_cwd)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let guest_entrypoint = if relative.is_empty() {
        String::from("/")
    } else {
        normalize_path(&format!("/{relative}"))
    };
    Some((
        guest_entrypoint,
        normalized_entrypoint.to_string_lossy().into_owned(),
    ))
}

fn prepare_javascript_runtime_env(
    vm: &VmState,
    env: &mut BTreeMap<String, String>,
    guest_cwd: &str,
    host_cwd: &Path,
    guest_entrypoint: Option<String>,
) -> Result<(), SidecarError> {
    let path_mappings = runtime_guest_path_mappings(vm);
    let read_paths = expand_host_access_paths(
        std::iter::once(vm.cwd.clone())
            .chain(
                path_mappings
                    .iter()
                    .map(|mapping| PathBuf::from(&mapping.host_path)),
            )
            .chain(std::iter::once(host_cwd.to_path_buf()))
            .collect::<Vec<_>>()
            .as_slice(),
    );
    let write_paths = dedupe_host_paths(&[vm.cwd.clone(), host_cwd.to_path_buf()]);
    let allowed_node_builtins = configured_allowed_node_builtins(vm);
    let loopback_exempt_ports = configured_loopback_exempt_ports(vm);

    env.insert(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        serde_json::to_string(&path_mappings).map_err(|error| {
            SidecarError::InvalidState(format!("failed to encode guest path mappings: {error}"))
        })?,
    );
    env.insert(
        String::from("AGENT_OS_EXTRA_FS_READ_PATHS"),
        serde_json::to_string(
            &read_paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| {
            SidecarError::InvalidState(format!("failed to encode read paths: {error}"))
        })?,
    );
    env.insert(
        String::from("AGENT_OS_EXTRA_FS_WRITE_PATHS"),
        serde_json::to_string(
            &write_paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| {
            SidecarError::InvalidState(format!("failed to encode write paths: {error}"))
        })?,
    );
    env.insert(
        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
        serde_json::to_string(&allowed_node_builtins).map_err(|error| {
            SidecarError::InvalidState(format!("failed to encode allowed builtins: {error}"))
        })?,
    );
    env.insert(String::from("HOME"), guest_cwd.to_owned());
    if !loopback_exempt_ports.is_empty() {
        env.insert(
            String::from(LOOPBACK_EXEMPT_PORTS_ENV),
            serde_json::to_string(&loopback_exempt_ports).map_err(|error| {
                SidecarError::InvalidState(format!("failed to encode loopback exemptions: {error}"))
            })?,
        );
    }
    if let Some(guest_entrypoint) = guest_entrypoint {
        env.insert(String::from("AGENT_OS_GUEST_ENTRYPOINT"), guest_entrypoint);
    }
    Ok(())
}

fn configured_allowed_node_builtins(vm: &VmState) -> Vec<String> {
    let configured = if vm.configuration.allowed_node_builtins.is_empty() {
        DEFAULT_ALLOWED_NODE_BUILTINS
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
    } else {
        vm.configuration.allowed_node_builtins.clone()
    };
    dedupe_strings(&configured)
}

fn configured_loopback_exempt_ports(vm: &VmState) -> Vec<String> {
    if !vm.configuration.loopback_exempt_ports.is_empty() {
        return vm
            .configuration
            .loopback_exempt_ports
            .iter()
            .map(ToString::to_string)
            .collect();
    }

    vm.metadata
        .get(&format!("env.{LOOPBACK_EXEMPT_PORTS_ENV}"))
        .and_then(|value| serde_json::from_str::<Vec<Value>>(value).ok())
        .into_iter()
        .flatten()
        .filter_map(|value| match value {
            Value::String(text) => Some(text),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .collect()
}

fn runtime_guest_path_mappings(vm: &VmState) -> Vec<RuntimeGuestPathMapping> {
    let mut mappings = vm
        .configuration
        .mounts
        .iter()
        .filter_map(|mount| {
            ((mount.plugin.id == "host_dir") || (mount.plugin.id == "module_access"))
                .then(|| {
                    mount
                        .plugin
                        .config
                        .get("hostPath")
                        .and_then(Value::as_str)
                        .map(|host_path| RuntimeGuestPathMapping {
                            guest_path: normalize_path(&mount.guest_path),
                            host_path: host_path.to_owned(),
                        })
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    let mut extra_node_modules_roots = mappings
        .iter()
        .filter(|mapping| mapping.guest_path.starts_with("/root/node_modules/"))
        .filter_map(|mapping| {
            host_node_modules_root(Path::new(&mapping.host_path)).map(|host_root| {
                RuntimeGuestPathMapping {
                    guest_path: String::from("/root/node_modules"),
                    host_path: host_root.to_string_lossy().into_owned(),
                }
            })
        })
        .collect::<Vec<_>>();
    mappings.append(&mut extra_node_modules_roots);
    mappings.push(RuntimeGuestPathMapping {
        guest_path: String::from("/"),
        host_path: vm.cwd.to_string_lossy().into_owned(),
    });
    mappings.sort_by(|left, right| right.guest_path.len().cmp(&left.guest_path.len()));
    mappings.dedup_by(|left, right| {
        left.guest_path == right.guest_path && left.host_path == right.host_path
    });
    mappings
}

fn host_node_modules_root(path: &Path) -> Option<PathBuf> {
    let canonical = fs::canonicalize(path).ok()?;
    canonical
        .ancestors()
        .filter(|candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("node_modules")
        })
        .last()
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod runtime_guest_path_mapping_tests {
    use super::host_node_modules_root;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn host_node_modules_root_prefers_workspace_root_over_pnpm_package_node_modules() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("agent-os-sidecar-node-modules-{unique}"));
        let workspace_node_modules = temp.join("node_modules");
        let package_root = workspace_node_modules
            .join(".pnpm")
            .join("example@1.0.0")
            .join("node_modules")
            .join("@scope")
            .join("pkg");
        fs::create_dir_all(&package_root).expect("package root should be created");

        let resolved =
            host_node_modules_root(&package_root).expect("node_modules root should resolve");

        assert_eq!(resolved, workspace_node_modules);

        fs::remove_dir_all(&temp).expect("temp tree should be removed");
    }
}

fn dedupe_strings(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            deduped.push(value.clone());
        }
    }
    deduped
}

fn dedupe_host_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for path in paths {
        let normalized = normalize_host_path(path);
        let key = normalized.to_string_lossy().into_owned();
        if seen.insert(key) {
            deduped.push(normalized);
        }
    }
    deduped
}

fn expand_host_access_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut expanded = Vec::new();
    let mut seen = BTreeSet::new();

    let mut add_path = |candidate: PathBuf| {
        let normalized = normalize_host_path(&candidate);
        let key = normalized.to_string_lossy().into_owned();
        if seen.insert(key) {
            expanded.push(normalized);
        }
    };

    for host_path in paths {
        add_path(host_path.clone());
        if let Ok(realpath) = fs::canonicalize(host_path) {
            add_path(realpath);
        }

        if host_path.file_name().and_then(|name| name.to_str()) != Some("node_modules") {
            continue;
        }

        let mut current = host_path.parent();
        while let Some(parent) = current {
            let candidate = parent.join("node_modules");
            if candidate.exists() {
                add_path(candidate.clone());
                if let Ok(realpath) = fs::canonicalize(&candidate) {
                    add_path(realpath);
                }
            }
            current = parent.parent();
        }
    }

    expanded
}

fn prepare_javascript_shadow(
    vm: &mut VmState,
    resolved: &ResolvedChildProcessExecution,
) -> Result<(), SidecarError> {
    let guest_entrypoint = resolved
        .env
        .get("AGENT_OS_GUEST_ENTRYPOINT")
        .cloned()
        .or_else(|| {
            resolved
                .entrypoint
                .starts_with('/')
                .then(|| normalize_path(&resolved.entrypoint))
        });
    let Some(guest_entrypoint) = guest_entrypoint else {
        return Ok(());
    };
    if host_mount_path_for_guest_path(vm, &guest_entrypoint).is_some() {
        return Ok(());
    }
    materialize_guest_path_to_shadow(vm, &guest_entrypoint)
}

fn materialize_guest_path_to_shadow(
    vm: &mut VmState,
    guest_path: &str,
) -> Result<(), SidecarError> {
    let stat = vm.kernel.lstat(guest_path).map_err(kernel_error)?;
    let shadow_path = shadow_path_for_guest(vm, guest_path);

    if stat.is_symbolic_link {
        if let Some(parent) = shadow_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                SidecarError::Io(format!("failed to create shadow symlink parent: {error}"))
            })?;
        }
        let _ = fs::remove_file(&shadow_path);
        let _ = fs::remove_dir_all(&shadow_path);
        let target = vm.kernel.read_link(guest_path).map_err(kernel_error)?;
        std::os::unix::fs::symlink(&target, &shadow_path)
            .map_err(|error| SidecarError::Io(format!("failed to mirror symlink: {error}")))?;
        return Ok(());
    }

    if stat.is_directory {
        fs::create_dir_all(&shadow_path).map_err(|error| {
            SidecarError::Io(format!("failed to create shadow directory: {error}"))
        })?;
        return Ok(());
    }

    if let Some(parent) = shadow_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!("failed to create shadow parent: {error}"))
        })?;
    }
    let bytes = vm.kernel.read_file(guest_path).map_err(kernel_error)?;
    fs::write(&shadow_path, bytes).map_err(|error| {
        SidecarError::Io(format!(
            "failed to mirror guest file into shadow root: {error}"
        ))
    })?;
    Ok(())
}

fn load_javascript_entrypoint_source(
    vm: &mut VmState,
    host_cwd: &Path,
    entrypoint: &str,
    env: &BTreeMap<String, String>,
) -> Option<String> {
    let mut read_guest_file = |path: &str| {
        vm.kernel
            .read_file(path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };

    if let Some(source) = env
        .get("AGENT_OS_GUEST_ENTRYPOINT")
        .filter(|path| path.starts_with('/'))
        .and_then(|path| read_guest_file(path))
    {
        return Some(source);
    }

    if entrypoint.starts_with('/') {
        if let Some(source) = read_guest_file(entrypoint) {
            return Some(source);
        }
    }

    let host_entrypoint = if Path::new(entrypoint).is_absolute() {
        PathBuf::from(entrypoint)
    } else {
        host_cwd.join(entrypoint)
    };
    let normalized_entrypoint = normalize_host_path(&host_entrypoint);
    let sandbox_root = normalize_host_path(&vm.cwd);
    let host_cwd = normalize_host_path(&vm.host_cwd);
    if !path_is_within_root(&normalized_entrypoint, &sandbox_root)
        && !path_is_within_root(&normalized_entrypoint, &host_cwd)
    {
        return None;
    }

    fs::read_to_string(&normalized_entrypoint).ok()
}

// extract_guest_env moved to crate::vm

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

// parse_resource_limits, parse_resource_limit, parse_resource_limit_u64,
// parse_vm_dns_config moved to crate::vm

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

// parse_vm_dns_nameserver, normalize_dns_hostname moved to crate::vm

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

// build_root_filesystem, convert_root_lower_descriptor, convert_root_filesystem_entry,
// root_snapshot_entry moved to crate::bootstrap

// apply_root_filesystem_entry, ensure_parent_directories moved to crate::bootstrap

// ProcNetEntry moved to crate::state

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

pub(crate) fn vm_network_resource_counts(vm: &VmState) -> NetworkResourceCounts {
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

pub(crate) fn build_javascript_socket_path_context(
    vm: &VmState,
) -> Result<JavascriptSocketPathContext, SidecarError> {
    let internal_env = crate::vm::extract_guest_env(&vm.metadata);
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

fn python_file_entrypoint(entrypoint: &str) -> Option<PathBuf> {
    let path = Path::new(entrypoint);
    (path.extension().and_then(|extension| extension.to_str()) == Some("py"))
        .then(|| path.to_path_buf())
}

// discover_command_guest_paths moved to crate::bootstrap

fn is_path_like_specifier(specifier: &str) -> bool {
    specifier.starts_with('/')
        || specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.starts_with("file:")
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

fn command_requires_shell(command: &str) -> bool {
    command.chars().any(|ch| {
        matches!(
            ch,
            '|' | '&'
                | ';'
                | '<'
                | '>'
                | '('
                | ')'
                | '$'
                | '`'
                | '*'
                | '?'
                | '['
                | ']'
                | '{'
                | '}'
                | '~'
                | '\''
                | '"'
                | '\\'
                | '\n'
        )
    })
}

fn host_mount_path_for_guest_path(vm: &VmState, guest_path: &str) -> Option<PathBuf> {
    let normalized = normalize_path(guest_path);

    let mut mounts = vm
        .configuration
        .mounts
        .iter()
        .filter_map(|mount| {
            ((mount.plugin.id == "host_dir") || (mount.plugin.id == "module_access"))
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

fn host_runtime_path_for_guest_path_with_env(
    vm: &VmState,
    runtime_env: &BTreeMap<String, String>,
    guest_path: &str,
    default_host_cwd: &Path,
) -> Option<PathBuf> {
    if let Some(path) = host_mount_path_for_guest_path(vm, guest_path) {
        return Some(path);
    }
    if let Some(path) = host_path_from_runtime_guest_mappings(runtime_env, guest_path) {
        return Some(path);
    }

    let normalized = normalize_path(guest_path);
    let virtual_home = runtime_env
        .get("AGENT_OS_VIRTUAL_OS_HOMEDIR")
        .or_else(|| vm.guest_env.get("AGENT_OS_VIRTUAL_OS_HOMEDIR"))
        .filter(|value| value.starts_with('/'))
        .cloned()
        .unwrap_or_else(|| String::from("/root"));

    if normalized == virtual_home || normalized.starts_with(&format!("{virtual_home}/")) {
        let suffix = normalized
            .strip_prefix(&virtual_home)
            .unwrap_or_default()
            .trim_start_matches('/');
        let mut host_path = default_host_cwd.to_path_buf();
        if !suffix.is_empty() {
            host_path.push(suffix);
        }
        return Some(host_path);
    }

    None
}

#[derive(Deserialize, Serialize)]
struct RuntimeGuestPathMapping {
    #[serde(rename = "guestPath")]
    guest_path: String,
    #[serde(rename = "hostPath")]
    host_path: String,
}

fn host_path_from_runtime_guest_mappings(
    runtime_env: &BTreeMap<String, String>,
    guest_path: &str,
) -> Option<PathBuf> {
    let mappings = runtime_env
        .get("AGENT_OS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<RuntimeGuestPathMapping>>(value).ok())?;
    let normalized = normalize_path(guest_path);

    let mut sorted_mappings = mappings
        .into_iter()
        .filter_map(|mapping| {
            (!mapping.guest_path.is_empty() && !mapping.host_path.is_empty()).then_some((
                normalize_path(&mapping.guest_path),
                PathBuf::from(mapping.host_path),
            ))
        })
        .collect::<Vec<_>>();
    sorted_mappings.sort_by(|left, right| right.0.len().cmp(&left.0.len()));

    for (guest_root, mut host_root) in sorted_mappings {
        if guest_root != "/"
            && normalized != guest_root
            && !normalized.starts_with(&format!("{guest_root}/"))
        {
            continue;
        }
        if guest_root == "/" && !normalized.starts_with('/') {
            continue;
        }

        if host_root.is_relative() {
            host_root = std::env::current_dir().ok()?.join(host_root);
        }

        let suffix = if guest_root == "/" {
            normalized.trim_start_matches('/')
        } else {
            normalized
                .strip_prefix(&guest_root)
                .unwrap_or_default()
                .trim_start_matches('/')
        };
        if !suffix.is_empty() {
            host_root.push(suffix);
        }
        return Some(host_root);
    }

    None
}

fn guest_runtime_path_for_host_path(
    runtime_env: &BTreeMap<String, String>,
    cwd: &Path,
    host_path: &str,
) -> Option<String> {
    let resolved = if host_path.starts_with("file://") {
        PathBuf::from(host_path.trim_start_matches("file://"))
    } else if host_path.starts_with("file:") {
        PathBuf::from(host_path.trim_start_matches("file:"))
    } else {
        let candidate = PathBuf::from(host_path);
        if candidate.is_absolute() {
            candidate
        } else if host_path.starts_with("./") || host_path.starts_with("../") {
            cwd.join(candidate)
        } else {
            return None;
        }
    };
    let normalized = normalize_host_path(&resolved);

    if let Some(path) = guest_path_from_runtime_host_mappings(runtime_env, &normalized) {
        return Some(path);
    }

    let normalized_cwd = normalize_host_path(cwd);
    if !path_is_within_root(&normalized, &normalized_cwd) {
        return None;
    }

    let virtual_home = runtime_env
        .get("AGENT_OS_VIRTUAL_OS_HOMEDIR")
        .filter(|value| value.starts_with('/'))
        .cloned()
        .unwrap_or_else(|| String::from("/root"));
    let suffix = normalized
        .strip_prefix(&normalized_cwd)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_owned();

    Some(if suffix.is_empty() {
        virtual_home
    } else {
        normalize_path(&format!("{virtual_home}/{suffix}"))
    })
}

fn guest_path_from_runtime_host_mappings(
    runtime_env: &BTreeMap<String, String>,
    host_path: &Path,
) -> Option<String> {
    let mappings = runtime_env
        .get("AGENT_OS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<RuntimeGuestPathMapping>>(value).ok())?;
    let normalized = normalize_host_path(host_path);

    let mut sorted_mappings = mappings
        .into_iter()
        .filter_map(|mapping| {
            (!mapping.guest_path.is_empty() && !mapping.host_path.is_empty()).then_some((
                normalize_path(&mapping.guest_path),
                normalize_host_path(Path::new(&mapping.host_path)),
            ))
        })
        .collect::<Vec<_>>();
    sorted_mappings.sort_by(|left, right| right.1.as_os_str().len().cmp(&left.1.as_os_str().len()));

    for (guest_root, host_root) in sorted_mappings {
        if !path_is_within_root(&normalized, &host_root) {
            continue;
        }
        let suffix = normalized
            .strip_prefix(&host_root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_owned();

        return Some(if suffix.is_empty() {
            guest_root
        } else if guest_root == "/" {
            normalize_path(&format!("/{suffix}"))
        } else {
            normalize_path(&format!("{guest_root}/{suffix}"))
        });
    }

    None
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

// JavascriptChildProcessSpawnOptions, JavascriptChildProcessSpawnRequest moved to crate::protocol
// ResolvedChildProcessExecution moved to crate::state

pub(crate) fn sanitize_javascript_child_process_internal_bootstrap_env(
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

// Network request types moved to crate::protocol

// VmDnsConfig, DnsResolutionSource moved to crate::state

fn resolve_tcp_bind_addr(host: &str, port: u16) -> Result<SocketAddr, SidecarError> {
    (host, port)
        .to_socket_addrs()
        .map_err(sidecar_net_error)?
        .next()
        .ok_or_else(|| {
            SidecarError::Execution(format!("failed to resolve TCP bind address {host}:{port}"))
        })
}

pub(crate) fn format_dns_resource(hostname: &str) -> String {
    format!("dns://{hostname}")
}

pub(crate) fn format_tcp_resource(host: &str, port: u16) -> String {
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

    let normalized_hostname = crate::vm::normalize_dns_hostname(hostname)?;
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

fn javascript_net_timeout_value() -> Value {
    Value::String(String::from(JAVASCRIPT_NET_TIMEOUT_SENTINEL))
}

fn javascript_net_json_string(value: Value, label: &str) -> Result<Value, SidecarError> {
    serde_json::to_string(&value)
        .map(Value::String)
        .map_err(|error| {
            SidecarError::InvalidState(format!("failed to serialize {label} payload: {error}"))
        })
}

fn javascript_net_read_value(
    event: Option<JavascriptTcpSocketEvent>,
) -> Result<Value, SidecarError> {
    match event {
        Some(JavascriptTcpSocketEvent::Data(chunk)) => Ok(Value::String(
            base64::engine::general_purpose::STANDARD.encode(chunk),
        )),
        Some(JavascriptTcpSocketEvent::End | JavascriptTcpSocketEvent::Close { .. }) => {
            Ok(Value::Null)
        }
        Some(JavascriptTcpSocketEvent::Error { code, message }) => {
            let detail = code.unwrap_or_else(|| String::from("socket read"));
            Err(SidecarError::Execution(format!("{detail}: {message}")))
        }
        None => Ok(javascript_net_timeout_value()),
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

fn tls_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(aws_lc_rs::default_provider())
}

fn tls_local_certificates(
    options: &JavascriptTlsBridgeOptions,
) -> Result<Vec<Vec<u8>>, SidecarError> {
    let Some(certificates) = options.cert.as_ref() else {
        return Ok(Vec::new());
    };
    tls_material_entries(certificates)
}

fn tls_material_entries(material: &JavascriptTlsMaterial) -> Result<Vec<Vec<u8>>, SidecarError> {
    match material {
        JavascriptTlsMaterial::Single(entry) => tls_data_value(entry).map(|value| vec![value]),
        JavascriptTlsMaterial::Many(entries) => entries.iter().map(tls_data_value).collect(),
    }
}

fn tls_data_value(value: &JavascriptTlsDataValue) -> Result<Vec<u8>, SidecarError> {
    match value {
        JavascriptTlsDataValue::Buffer { data } => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| {
                SidecarError::InvalidState(format!("TLS material contains invalid base64: {error}"))
            }),
        JavascriptTlsDataValue::String { data } => Ok(data.as_bytes().to_vec()),
    }
}

fn tls_certificates_from_material(
    material: &JavascriptTlsMaterial,
) -> Result<Vec<CertificateDer<'static>>, SidecarError> {
    let mut certificates = Vec::new();
    for entry in tls_material_entries(material)? {
        let mut reader = std::io::BufReader::new(Cursor::new(entry.clone()));
        let parsed = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(sidecar_net_error)?;
        if parsed.is_empty() {
            certificates.push(CertificateDer::from(entry));
        } else {
            certificates.extend(parsed);
        }
    }
    if certificates.is_empty() {
        return Err(SidecarError::InvalidState(String::from(
            "TLS certificate material did not contain any certificates",
        )));
    }
    Ok(certificates)
}

fn tls_private_key_from_material(
    material: &JavascriptTlsMaterial,
) -> Result<PrivateKeyDer<'static>, SidecarError> {
    for entry in tls_material_entries(material)? {
        let mut reader = std::io::BufReader::new(Cursor::new(entry));
        if let Some(key) = rustls_pemfile::private_key(&mut reader).map_err(sidecar_net_error)? {
            return Ok(key);
        }
    }
    Err(SidecarError::InvalidState(String::from(
        "TLS private key material did not contain a supported key",
    )))
}

fn tls_root_store(options: &JavascriptTlsBridgeOptions) -> Result<RootCertStore, SidecarError> {
    let mut roots = RootCertStore::empty();
    if let Some(ca) = options.ca.as_ref() {
        for certificate in tls_certificates_from_material(ca)? {
            roots.add(certificate).map_err(|error| {
                SidecarError::InvalidState(format!("failed to add TLS CA certificate: {error}"))
            })?;
        }
        return Ok(roots);
    }

    for certificate in rustls_native_certs::load_native_certs().certs {
        roots.add(certificate).map_err(|error| {
            SidecarError::InvalidState(format!(
                "failed to add native TLS certificate to root store: {error}"
            ))
        })?;
    }
    Ok(roots)
}

fn build_client_tls_stream(
    stream: TcpStream,
    options: &JavascriptTlsBridgeOptions,
) -> Result<rustls::StreamOwned<ClientConnection, TcpStream>, SidecarError> {
    let provider = tls_provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|error| {
            SidecarError::InvalidState(format!("invalid TLS protocol config: {error}"))
        })?;

    let mut config = if options.reject_unauthorized == Some(false) {
        let verifier = Arc::new(InsecureTlsVerifier {
            supported_schemes: provider
                .signature_verification_algorithms
                .supported_schemes(),
        });
        builder
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth()
    } else {
        builder
            .with_root_certificates(tls_root_store(options)?)
            .with_no_client_auth()
    };

    if let Some(protocols) = options.alpn_protocols.as_ref() {
        config.alpn_protocols = protocols
            .iter()
            .map(|protocol| protocol.as_bytes().to_vec())
            .collect();
    }

    let server_name = options
        .servername
        .clone()
        .unwrap_or_else(|| String::from("localhost"));
    let server_name = ServerName::try_from(server_name)
        .map_err(|_| SidecarError::InvalidState(String::from("invalid TLS servername")))?;
    stream
        .set_read_timeout(Some(TLS_HANDSHAKE_TIMEOUT))
        .map_err(sidecar_net_error)?;
    stream
        .set_write_timeout(Some(TLS_HANDSHAKE_TIMEOUT))
        .map_err(sidecar_net_error)?;
    let mut tls_stream = rustls::StreamOwned::new(
        ClientConnection::new(Arc::new(config), server_name).map_err(|error| {
            SidecarError::Execution(format!("failed to start TLS client: {error}"))
        })?,
        stream,
    );
    while tls_stream.conn.is_handshaking() {
        tls_stream
            .conn
            .complete_io(&mut tls_stream.sock)
            .map_err(sidecar_net_error)?;
    }
    tls_stream
        .sock
        .set_read_timeout(Some(TCP_SOCKET_POLL_TIMEOUT))
        .map_err(sidecar_net_error)?;
    tls_stream
        .sock
        .set_write_timeout(None)
        .map_err(sidecar_net_error)?;
    Ok(tls_stream)
}

fn build_server_tls_stream(
    stream: TcpStream,
    options: &JavascriptTlsBridgeOptions,
) -> Result<rustls::StreamOwned<ServerConnection, TcpStream>, SidecarError> {
    let certificates = tls_certificates_from_material(options.cert.as_ref().ok_or_else(|| {
        SidecarError::InvalidState(String::from("TLS server upgrade requires a certificate"))
    })?)?;
    let key = tls_private_key_from_material(options.key.as_ref().ok_or_else(|| {
        SidecarError::InvalidState(String::from("TLS server upgrade requires a private key"))
    })?)?;

    let mut config = ServerConfig::builder_with_provider(tls_provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| {
            SidecarError::InvalidState(format!("invalid TLS protocol config: {error}"))
        })?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|error| {
            SidecarError::InvalidState(format!("invalid TLS server config: {error}"))
        })?;

    if let Some(protocols) = options.alpn_protocols.as_ref() {
        config.alpn_protocols = protocols
            .iter()
            .map(|protocol| protocol.as_bytes().to_vec())
            .collect();
    }

    stream
        .set_read_timeout(Some(TLS_HANDSHAKE_TIMEOUT))
        .map_err(sidecar_net_error)?;
    stream
        .set_write_timeout(Some(TLS_HANDSHAKE_TIMEOUT))
        .map_err(sidecar_net_error)?;
    let mut tls_stream = rustls::StreamOwned::new(
        ServerConnection::new(Arc::new(config)).map_err(|error| {
            SidecarError::Execution(format!("failed to start TLS server: {error}"))
        })?,
        stream,
    );
    while tls_stream.conn.is_handshaking() {
        tls_stream
            .conn
            .complete_io(&mut tls_stream.sock)
            .map_err(sidecar_net_error)?;
    }
    tls_stream
        .sock
        .set_read_timeout(Some(TCP_SOCKET_POLL_TIMEOUT))
        .map_err(sidecar_net_error)?;
    tls_stream
        .sock
        .set_write_timeout(None)
        .map_err(sidecar_net_error)?;
    Ok(tls_stream)
}

fn tls_protocol_name(version: rustls::ProtocolVersion) -> String {
    match version {
        rustls::ProtocolVersion::TLSv1_2 => String::from("TLSv1.2"),
        rustls::ProtocolVersion::TLSv1_3 => String::from("TLSv1.3"),
        other => other
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{other:?}")),
    }
}

fn tls_cipher_bridge_value(suite: rustls::SupportedCipherSuite) -> Value {
    tls_bridge_object(vec![
        (
            "name",
            suite
                .suite()
                .as_str()
                .map(|value| Value::String(value.to_owned()))
                .unwrap_or(Value::Null),
        ),
        (
            "standardName",
            suite
                .suite()
                .as_str()
                .map(|value| Value::String(value.to_owned()))
                .unwrap_or(Value::Null),
        ),
        (
            "version",
            Value::String(if suite.tls13().is_some() {
                String::from("TLSv1.3")
            } else {
                String::from("TLSv1.2")
            }),
        ),
    ])
}

fn tls_certificate_bridge_value(certificate: &[u8], detailed: bool) -> Value {
    let mut fields = vec![("raw", tls_bridge_buffer_value(certificate))];
    if detailed {
        fields.push(("issuerCertificate", tls_bridge_undefined_value()));
    }
    tls_bridge_object(fields)
}

fn tls_bridge_buffer_value(bytes: &[u8]) -> Value {
    json!({
        "type": "buffer",
        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn tls_bridge_object(entries: Vec<(&str, Value)>) -> Value {
    let value = entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect::<serde_json::Map<String, Value>>();
    json!({
        "type": "object",
        "id": 1,
        "value": value,
    })
}

fn tls_bridge_undefined_value() -> Value {
    json!({
        "type": "undefined",
    })
}

fn spawn_tcp_socket_reader(
    stream: TcpStream,
    sender: Sender<JavascriptTcpSocketEvent>,
    tls_mode: Arc<AtomicBool>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut stream = stream;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            if tls_mode.load(Ordering::SeqCst) {
                break;
            }
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
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
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

fn spawn_tls_socket_reader(
    tls_stream: Arc<Mutex<Option<ActiveTlsStream>>>,
    sender: Sender<JavascriptTcpSocketEvent>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read_result = {
                let mut guard = match tls_stream.lock() {
                    Ok(guard) => guard,
                    Err(_) => return,
                };
                let Some(stream) = guard.as_mut() else {
                    return;
                };
                stream.read(&mut buffer)
            };

            match read_result {
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
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
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
    let sqlite_database_ids = process.sqlite_databases.keys().copied().collect::<Vec<_>>();
    for database_id in sqlite_database_ids {
        let _ = close_sqlite_database(kernel, process, database_id);
    }
    process.sqlite_statements.clear();
    process.http_servers.clear();
    process.pending_http_requests.clear();
    if let Ok(mut http2) = process.http2.shared.lock() {
        let sessions = http2.sessions.values().cloned().collect::<Vec<_>>();
        http2.server_events.clear();
        http2.session_events.clear();
        http2.streams.clear();
        http2.servers.clear();
        http2.sessions.clear();
        drop(http2);
        for session in sessions {
            let (respond_to, _rx) = mpsc::channel();
            let _ = session.command_tx.send(Http2SessionCommand::Close {
                abrupt: true,
                respond_to,
            });
        }
    }

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

fn service_javascript_sqlite_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    match request.method.as_str() {
        "sqlite.constants" => Ok(json!({})),
        "sqlite.open" => sqlite_open_database(kernel, process, request),
        "sqlite.close" => {
            let database_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.close database id")?;
            close_sqlite_database(kernel, process, database_id)?;
            Ok(Value::Null)
        }
        "sqlite.exec" => sqlite_exec_database(kernel, process, request),
        "sqlite.query" => sqlite_query_database(process, request),
        "sqlite.prepare" => sqlite_prepare_statement(process, request),
        "sqlite.location" => {
            let database_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.location database id")?;
            let database = sqlite_database(process, database_id)?;
            Ok(database
                .vm_path
                .as_ref()
                .map(|path| Value::String(path.clone()))
                .unwrap_or(Value::Null))
        }
        "sqlite.checkpoint" => {
            let database_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.checkpoint database id")?;
            let kernel_pid = process.kernel_pid;
            let database = sqlite_database_mut(process, database_id)?;
            sqlite_sync_database(kernel, kernel_pid, database)?;
            Ok(Value::Null)
        }
        "sqlite.statement.run" => sqlite_run_statement(kernel, process, request),
        "sqlite.statement.get" => sqlite_get_statement(process, request),
        "sqlite.statement.all" | "sqlite.statement.iterate" => {
            sqlite_all_statement(process, request)
        }
        "sqlite.statement.columns" => sqlite_statement_columns(process, request),
        "sqlite.statement.setReturnArrays" => {
            let statement_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sqlite.statement.setReturnArrays statement id",
            )?;
            let enabled = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "sqlite.statement.setReturnArrays enabled",
            )?;
            sqlite_statement_mut(process, statement_id)?.return_arrays = enabled;
            Ok(Value::Null)
        }
        "sqlite.statement.setReadBigInts" => {
            let statement_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sqlite.statement.setReadBigInts statement id",
            )?;
            let enabled = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "sqlite.statement.setReadBigInts enabled",
            )?;
            sqlite_statement_mut(process, statement_id)?.read_bigints = enabled;
            Ok(Value::Null)
        }
        "sqlite.statement.setAllowBareNamedParameters" => {
            let statement_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sqlite.statement.setAllowBareNamedParameters statement id",
            )?;
            let enabled = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "sqlite.statement.setAllowBareNamedParameters enabled",
            )?;
            sqlite_statement_mut(process, statement_id)?.allow_bare_named_parameters = enabled;
            Ok(Value::Null)
        }
        "sqlite.statement.setAllowUnknownNamedParameters" => {
            let statement_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sqlite.statement.setAllowUnknownNamedParameters statement id",
            )?;
            let enabled = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "sqlite.statement.setAllowUnknownNamedParameters enabled",
            )?;
            sqlite_statement_mut(process, statement_id)?.allow_unknown_named_parameters = enabled;
            Ok(Value::Null)
        }
        "sqlite.statement.finalize" => {
            let statement_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sqlite.statement.finalize statement id",
            )?;
            process
                .sqlite_statements
                .remove(&statement_id)
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "sqlite statement handle not found: {statement_id}"
                    ))
                })?;
            Ok(Value::Null)
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript sqlite sync RPC method {other}"
        ))),
    }
}

fn sqlite_open_database(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let path = request.args.first().and_then(Value::as_str);
    let vm_path = path.filter(|value| !value.is_empty() && *value != ":memory:");
    let options = request.args.get(1);
    let read_only = sqlite_option_bool(options, "readOnly").unwrap_or(false);
    let create = sqlite_option_bool(options, "create").unwrap_or(!read_only);
    let timeout_ms = sqlite_option_u64(options, "timeout");

    process.next_sqlite_database_id += 1;
    let database_id = process.next_sqlite_database_id;

    let host_path = if vm_path.is_some() {
        Some(
            std::env::temp_dir()
                .join(format!(
                    "agent-os-sidecar-sqlite-{}-{database_id}",
                    process.kernel_pid
                ))
                .join("database.sqlite"),
        )
    } else {
        None
    };

    if let Some(host_path) = host_path.as_ref() {
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to prepare sqlite temp directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
    }

    if let (Some(vm_path), Some(host_path)) = (vm_path, host_path.as_ref()) {
        if kernel
            .exists_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, vm_path)
            .map_err(kernel_error)?
        {
            let contents = kernel
                .read_file_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, vm_path)
                .map_err(kernel_error)?;
            fs::write(host_path, contents).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to materialize sqlite database {}: {error}",
                    host_path.display()
                ))
            })?;
        } else if read_only && !create {
            return Err(SidecarError::InvalidState(format!(
                "sqlite database does not exist: {vm_path}"
            )));
        }
    }

    let target = host_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from(":memory:"));
    let mut flags = if read_only {
        SqliteOpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        SqliteOpenFlags::SQLITE_OPEN_READ_WRITE
    };
    if create && !read_only {
        flags |= SqliteOpenFlags::SQLITE_OPEN_CREATE;
    }

    let connection = SqliteConnection::open_with_flags(&target, flags).map_err(|error| {
        SidecarError::InvalidState(format!(
            "sqlite database open failed for {}: {error}",
            vm_path.unwrap_or(":memory:")
        ))
    })?;
    if let Some(timeout_ms) = timeout_ms {
        connection
            .busy_timeout(Duration::from_millis(timeout_ms))
            .map_err(sqlite_error)?;
    }
    if host_path.is_some() && !read_only {
        let _ = connection.pragma_update(None, "journal_mode", "WAL");
    }

    process.sqlite_databases.insert(
        database_id,
        ActiveSqliteDatabase {
            connection,
            host_path,
            vm_path: vm_path.map(String::from),
            dirty: false,
            transaction_depth: 0,
            read_only,
        },
    );

    Ok(json!(database_id))
}

fn sqlite_exec_database(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let database_id = javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.exec database id")?;
    let sql = javascript_sync_rpc_arg_str(&request.args, 1, "sqlite.exec sql")?;
    let kernel_pid = process.kernel_pid;
    let database = sqlite_database_mut(process, database_id)?;
    let before = database.connection.total_changes();
    database
        .connection
        .execute_batch(sql)
        .map_err(sqlite_error)?;
    mark_sqlite_mutation(database, sql);
    sqlite_sync_database(kernel, kernel_pid, database)?;
    Ok(json!(
        database.connection.total_changes().saturating_sub(before)
    ))
}

fn sqlite_query_database(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let database_id = javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.query database id")?;
    let sql = javascript_sync_rpc_arg_str(&request.args, 1, "sqlite.query sql")?;
    let params = request.args.get(2);
    let options = request.args.get(3);
    let return_arrays = sqlite_option_bool(options, "returnArrays").unwrap_or(false);
    let read_bigints = sqlite_option_bool(options, "readBigInts").unwrap_or(false);
    let database = sqlite_database_mut(process, database_id)?;
    sqlite_query_rows(
        &mut database.connection,
        sql,
        params,
        return_arrays,
        read_bigints,
        true,
        false,
    )
}

fn sqlite_prepare_statement(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let database_id = javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.prepare database id")?;
    let sql = javascript_sync_rpc_arg_str(&request.args, 1, "sqlite.prepare sql")?;
    let _ = sqlite_database(process, database_id)?;
    process.next_sqlite_statement_id += 1;
    let statement_id = process.next_sqlite_statement_id;
    process.sqlite_statements.insert(
        statement_id,
        ActiveSqliteStatement {
            database_id,
            sql: sql.to_owned(),
            return_arrays: false,
            read_bigints: false,
            allow_bare_named_parameters: false,
            allow_unknown_named_parameters: false,
        },
    );
    Ok(json!(statement_id))
}

fn sqlite_run_statement(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let statement_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.statement.run statement id")?;
    let params = request.args.get(1);
    let statement_state = sqlite_statement(process, statement_id)?.clone();
    let kernel_pid = process.kernel_pid;
    let database = sqlite_database_mut(process, statement_state.database_id)?;
    let before = database.connection.total_changes();
    {
        let mut statement = database
            .connection
            .prepare(&statement_state.sql)
            .map_err(sqlite_error)?;
        bind_sqlite_parameters(
            &mut statement,
            params,
            statement_state.allow_bare_named_parameters,
            statement_state.allow_unknown_named_parameters,
        )?;
        statement.raw_execute().map_err(sqlite_error)?;
    }
    let changes = database.connection.total_changes().saturating_sub(before);
    let last_insert_rowid = database.connection.last_insert_rowid();
    mark_sqlite_mutation(database, &statement_state.sql);
    sqlite_sync_database(kernel, kernel_pid, database)?;
    let result = json!({
        "changes": changes,
        "lastInsertRowid": encode_sqlite_integer(last_insert_rowid, true),
    });
    Ok(result)
}

fn sqlite_get_statement(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let statement_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.statement.get statement id")?;
    let params = request.args.get(1);
    let statement_state = sqlite_statement(process, statement_id)?.clone();
    let database = sqlite_database_mut(process, statement_state.database_id)?;
    let rows = sqlite_query_rows(
        &mut database.connection,
        &statement_state.sql,
        params,
        statement_state.return_arrays,
        statement_state.read_bigints,
        statement_state.allow_bare_named_parameters,
        statement_state.allow_unknown_named_parameters,
    )?;
    Ok(rows
        .as_array()
        .and_then(|rows| rows.first().cloned())
        .unwrap_or(Value::Null))
}

fn sqlite_all_statement(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let statement_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.statement.all statement id")?;
    let params = request.args.get(1);
    let statement_state = sqlite_statement(process, statement_id)?.clone();
    let database = sqlite_database_mut(process, statement_state.database_id)?;
    sqlite_query_rows(
        &mut database.connection,
        &statement_state.sql,
        params,
        statement_state.return_arrays,
        statement_state.read_bigints,
        statement_state.allow_bare_named_parameters,
        statement_state.allow_unknown_named_parameters,
    )
}

fn sqlite_statement_columns(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let statement_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.statement.columns statement id")?;
    let statement_state = sqlite_statement(process, statement_id)?.clone();
    let database = sqlite_database_mut(process, statement_state.database_id)?;
    let statement = database
        .connection
        .prepare(&statement_state.sql)
        .map_err(sqlite_error)?;
    Ok(Value::Array(
        statement
            .column_names()
            .iter()
            .map(|name| json!({ "name": name }))
            .collect(),
    ))
}

fn sqlite_query_rows(
    connection: &mut SqliteConnection,
    sql: &str,
    params: Option<&Value>,
    return_arrays: bool,
    read_bigints: bool,
    allow_bare_named_parameters: bool,
    allow_unknown_named_parameters: bool,
) -> Result<Value, SidecarError> {
    let mut statement = connection.prepare(sql).map_err(sqlite_error)?;
    let column_names = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let column_count = statement.column_count();
    bind_sqlite_parameters(
        &mut statement,
        params,
        allow_bare_named_parameters,
        allow_unknown_named_parameters,
    )?;
    let mut rows = statement.raw_query();
    let mut encoded_rows = Vec::new();
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        encoded_rows.push(encode_sqlite_row(
            row,
            &column_names,
            column_count,
            return_arrays,
            read_bigints,
        )?);
    }
    Ok(Value::Array(encoded_rows))
}

fn encode_sqlite_row(
    row: &rusqlite::Row<'_>,
    column_names: &[String],
    column_count: usize,
    return_arrays: bool,
    read_bigints: bool,
) -> Result<Value, SidecarError> {
    if return_arrays {
        let mut values = Vec::with_capacity(column_count);
        for index in 0..column_count {
            values.push(encode_sqlite_value_ref(
                row.get_ref(index).map_err(sqlite_error)?,
                read_bigints,
            )?);
        }
        return Ok(Value::Array(values));
    }

    let mut object = Map::with_capacity(column_count);
    for (index, name) in column_names.iter().enumerate() {
        object.insert(
            name.clone(),
            encode_sqlite_value_ref(row.get_ref(index).map_err(sqlite_error)?, read_bigints)?,
        );
    }
    Ok(Value::Object(object))
}

fn encode_sqlite_value_ref(
    value: SqliteValueRef<'_>,
    read_bigints: bool,
) -> Result<Value, SidecarError> {
    Ok(match value {
        SqliteValueRef::Null => Value::Null,
        SqliteValueRef::Integer(number) => encode_sqlite_integer(number, read_bigints),
        SqliteValueRef::Real(number) => json!(number),
        SqliteValueRef::Text(text) => Value::String(String::from_utf8_lossy(text).into_owned()),
        SqliteValueRef::Blob(bytes) => json!({
            "__agentosSqliteType": "uint8array",
            "value": base64::engine::general_purpose::STANDARD.encode(bytes),
        }),
    })
}

fn encode_sqlite_integer(number: i64, read_bigints: bool) -> Value {
    if read_bigints || number.abs() > SQLITE_JS_SAFE_INTEGER_MAX {
        json!({
            "__agentosSqliteType": "bigint",
            "value": number.to_string(),
        })
    } else {
        json!(number)
    }
}

fn bind_sqlite_parameters(
    statement: &mut SqliteStatement<'_>,
    params: Option<&Value>,
    allow_bare_named_parameters: bool,
    allow_unknown_named_parameters: bool,
) -> Result<(), SidecarError> {
    let Some(params) = params else {
        return Ok(());
    };
    match params {
        Value::Null => Ok(()),
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                statement
                    .raw_bind_parameter(index + 1, decode_sqlite_parameter(value)?)
                    .map_err(sqlite_error)?;
            }
            Ok(())
        }
        Value::Object(map)
            if map
                .get("__agentosSqliteType")
                .and_then(Value::as_str)
                .is_none() =>
        {
            for (key, value) in map {
                let index =
                    resolve_sqlite_parameter_index(statement, key, allow_bare_named_parameters)?;
                let Some(index) = index else {
                    if allow_unknown_named_parameters {
                        continue;
                    }
                    return Err(SidecarError::InvalidState(format!(
                        "sqlite named parameter not found: {key}"
                    )));
                };
                statement
                    .raw_bind_parameter(index, decode_sqlite_parameter(value)?)
                    .map_err(sqlite_error)?;
            }
            Ok(())
        }
        other => statement
            .raw_bind_parameter(1, decode_sqlite_parameter(other)?)
            .map_err(sqlite_error),
    }
}

fn resolve_sqlite_parameter_index(
    statement: &mut SqliteStatement<'_>,
    key: &str,
    allow_bare_named_parameters: bool,
) -> Result<Option<usize>, SidecarError> {
    let mut candidates = vec![key.to_owned()];
    if allow_bare_named_parameters
        && !key.starts_with(':')
        && !key.starts_with('@')
        && !key.starts_with('$')
    {
        candidates.push(format!(":{key}"));
        candidates.push(format!("@{key}"));
        candidates.push(format!("${key}"));
    }
    for candidate in candidates {
        if let Some(index) = statement
            .parameter_index(&candidate)
            .map_err(sqlite_error)?
        {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn decode_sqlite_parameter(value: &Value) -> Result<rusqlite::types::Value, SidecarError> {
    Ok(match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Bool(value) => rusqlite::types::Value::Integer(i64::from(*value)),
        Value::Number(value) => match (value.as_i64(), value.as_f64()) {
            (Some(integer), _) => rusqlite::types::Value::Integer(integer),
            (_, Some(real)) => rusqlite::types::Value::Real(real),
            _ => {
                return Err(SidecarError::InvalidState(String::from(
                    "sqlite parameter number is not representable",
                )));
            }
        },
        Value::String(value) => rusqlite::types::Value::Text(value.clone()),
        Value::Array(_) => {
            return Err(SidecarError::InvalidState(String::from(
                "sqlite parameters do not support nested arrays",
            )));
        }
        Value::Object(map) => match map.get("__agentosSqliteType").and_then(Value::as_str) {
            Some("bigint") => rusqlite::types::Value::Integer(
                map.get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "sqlite bigint parameter missing string value",
                        ))
                    })?
                    .parse::<i64>()
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "sqlite bigint parameter is not a signed 64-bit integer: {error}"
                        ))
                    })?,
            ),
            Some("uint8array") => rusqlite::types::Value::Blob(
                base64::engine::general_purpose::STANDARD
                    .decode(map.get("value").and_then(Value::as_str).ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "sqlite blob parameter missing base64 value",
                        ))
                    })?)
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "sqlite blob parameter contains invalid base64: {error}"
                        ))
                    })?,
            ),
            Some(other) => {
                return Err(SidecarError::InvalidState(format!(
                    "unsupported sqlite tagged parameter type {other}"
                )));
            }
            None => {
                return Err(SidecarError::InvalidState(String::from(
                    "sqlite named parameter objects must be passed as the top-level params object",
                )));
            }
        },
    })
}

fn close_sqlite_database(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    database_id: u64,
) -> Result<(), SidecarError> {
    let mut database = process
        .sqlite_databases
        .remove(&database_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("sqlite database handle not found: {database_id}"))
        })?;
    process
        .sqlite_statements
        .retain(|_, statement| statement.database_id != database_id);
    sqlite_sync_database(kernel, process.kernel_pid, &mut database)?;
    let host_path = database.host_path.clone();
    drop(database);
    cleanup_sqlite_host_artifacts(host_path.as_deref())?;
    Ok(())
}

fn sqlite_sync_database(
    kernel: &mut SidecarKernel,
    kernel_pid: u32,
    database: &mut ActiveSqliteDatabase,
) -> Result<(), SidecarError> {
    if !database.dirty
        || database.transaction_depth > 0
        || database.read_only
        || database.host_path.is_none()
        || database.vm_path.is_none()
    {
        return Ok(());
    }

    let _ = database
        .connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
    let host_path = database.host_path.as_ref().expect("sqlite host path");
    if !host_path.exists() {
        return Ok(());
    }
    ensure_vm_parent_dir(
        kernel,
        kernel_pid,
        database.vm_path.as_deref().expect("sqlite vm path"),
    )?;
    let contents = fs::read(host_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to read sqlite temp database {}: {error}",
            host_path.display()
        ))
    })?;
    kernel
        .write_file_for_process(
            EXECUTION_DRIVER_NAME,
            kernel_pid,
            database.vm_path.as_deref().expect("sqlite vm path"),
            contents,
            None,
        )
        .map_err(kernel_error)?;
    database.dirty = false;
    Ok(())
}

fn cleanup_sqlite_host_artifacts(host_path: Option<&Path>) -> Result<(), SidecarError> {
    let Some(host_path) = host_path else {
        return Ok(());
    };
    let parent = host_path.parent().map(PathBuf::from);
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{}", host_path.display(), suffix));
        if path.exists() {
            fs::remove_file(&path).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to remove sqlite temp artifact {}: {error}",
                    path.display()
                ))
            })?;
        }
    }
    if let Some(parent) = parent {
        let _ = fs::remove_dir_all(parent);
    }
    Ok(())
}

fn ensure_vm_parent_dir(
    kernel: &mut SidecarKernel,
    kernel_pid: u32,
    path: &str,
) -> Result<(), SidecarError> {
    let parent = dirname(path);
    if parent == "/" || parent == "." {
        return Ok(());
    }
    let mut current = String::new();
    for segment in parent.split('/').filter(|segment| !segment.is_empty()) {
        current.push('/');
        current.push_str(segment);
        if !kernel
            .exists_for_process(EXECUTION_DRIVER_NAME, kernel_pid, &current)
            .map_err(kernel_error)?
        {
            kernel
                .mkdir_for_process(EXECUTION_DRIVER_NAME, kernel_pid, &current, false, None)
                .map_err(kernel_error)?;
        }
    }
    Ok(())
}

fn sqlite_database(
    process: &ActiveProcess,
    database_id: u64,
) -> Result<&ActiveSqliteDatabase, SidecarError> {
    process.sqlite_databases.get(&database_id).ok_or_else(|| {
        SidecarError::InvalidState(format!("sqlite database handle not found: {database_id}"))
    })
}

fn sqlite_database_mut(
    process: &mut ActiveProcess,
    database_id: u64,
) -> Result<&mut ActiveSqliteDatabase, SidecarError> {
    process
        .sqlite_databases
        .get_mut(&database_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("sqlite database handle not found: {database_id}"))
        })
}

fn sqlite_statement(
    process: &ActiveProcess,
    statement_id: u64,
) -> Result<&ActiveSqliteStatement, SidecarError> {
    process.sqlite_statements.get(&statement_id).ok_or_else(|| {
        SidecarError::InvalidState(format!("sqlite statement handle not found: {statement_id}"))
    })
}

fn sqlite_statement_mut(
    process: &mut ActiveProcess,
    statement_id: u64,
) -> Result<&mut ActiveSqliteStatement, SidecarError> {
    process
        .sqlite_statements
        .get_mut(&statement_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("sqlite statement handle not found: {statement_id}"))
        })
}

fn mark_sqlite_mutation(database: &mut ActiveSqliteDatabase, sql: &str) {
    let normalized = sql.trim_start().to_ascii_lowercase();
    if normalized.starts_with("begin") || normalized.starts_with("savepoint") {
        database.dirty = true;
        database.transaction_depth += 1;
        return;
    }
    if normalized.starts_with("commit") || normalized.starts_with("release savepoint") {
        database.dirty = true;
        database.transaction_depth = database.transaction_depth.saturating_sub(1);
        return;
    }
    if normalized.starts_with("rollback") && !normalized.starts_with("rollback to") {
        database.dirty = true;
        database.transaction_depth = database.transaction_depth.saturating_sub(1);
        return;
    }
    if normalized.starts_with("insert")
        || normalized.starts_with("update")
        || normalized.starts_with("delete")
        || normalized.starts_with("replace")
        || normalized.starts_with("create")
        || normalized.starts_with("alter")
        || normalized.starts_with("drop")
        || normalized.starts_with("vacuum")
        || normalized.starts_with("reindex")
        || normalized.starts_with("analyze")
        || normalized.starts_with("attach")
        || normalized.starts_with("detach")
        || normalized.starts_with("pragma")
    {
        database.dirty = true;
    }
}

fn sqlite_option_bool(options: Option<&Value>, key: &str) -> Option<bool> {
    options
        .and_then(|value| value.get(key))
        .and_then(Value::as_bool)
}

fn sqlite_option_u64(options: Option<&Value>, key: &str) -> Option<u64> {
    options
        .and_then(|value| value.get(key))
        .and_then(Value::as_u64)
}

fn sqlite_error(error: rusqlite::Error) -> SidecarError {
    SidecarError::InvalidState(format!("sqlite error: {error}"))
}

pub(crate) fn javascript_sync_rpc_arg_str<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a str, SidecarError> {
    args.get(index)
        .and_then(Value::as_str)
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} must be a string argument")))
}

pub(crate) fn javascript_sync_rpc_arg_bool(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<bool, SidecarError> {
    args.get(index)
        .and_then(Value::as_bool)
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} must be a boolean argument")))
}

pub(crate) fn javascript_sync_rpc_encoding(args: &[Value]) -> Option<String> {
    args.get(1).and_then(|value| {
        value.as_str().map(str::to_owned).or_else(|| {
            value
                .get("encoding")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    })
}

pub(crate) fn javascript_sync_rpc_option_bool(
    args: &[Value],
    index: usize,
    key: &str,
) -> Option<bool> {
    args.get(index)
        .and_then(|value| value.get(key))
        .and_then(Value::as_bool)
}

pub(crate) fn javascript_sync_rpc_option_u32(
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

pub(crate) fn javascript_sync_rpc_arg_u32(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<u32, SidecarError> {
    let value = javascript_sync_rpc_arg_u64(args, index, label)?;
    u32::try_from(value)
        .map_err(|_| SidecarError::InvalidState(format!("{label} must fit within u32")))
}

pub(crate) fn javascript_sync_rpc_arg_u32_optional(
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

pub(crate) fn javascript_sync_rpc_arg_u64(
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

pub(crate) fn javascript_sync_rpc_arg_u64_optional(
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

pub(crate) fn javascript_sync_rpc_bytes_arg(
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

pub(crate) fn javascript_sync_rpc_bytes_value(bytes: &[u8]) -> Value {
    json!({
        "__agentOsType": "bytes",
        "base64": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn javascript_sync_rpc_base64_arg(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Vec<u8>, SidecarError> {
    let value = javascript_sync_rpc_arg_str(args, index, label)?;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| {
            SidecarError::InvalidState(format!("{label} contains invalid base64: {error}"))
        })
}

pub(crate) fn service_javascript_sync_rpc<B>(
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
        "__kernel_stdin_read" => service_javascript_kernel_stdin_sync_rpc(kernel, process, request),
        "__pty_set_raw_mode" => {
            service_javascript_pty_set_raw_mode_sync_rpc(kernel, process, request)
        }
        "crypto.hashDigest"
        | "crypto.hmacDigest"
        | "crypto.pbkdf2"
        | "crypto.scrypt"
        | "crypto.cipheriv"
        | "crypto.decipheriv"
        | "crypto.cipherivCreate"
        | "crypto.cipherivUpdate"
        | "crypto.cipherivFinal"
        | "crypto.sign"
        | "crypto.verify"
        | "crypto.asymmetricOp"
        | "crypto.createKeyObject"
        | "crypto.generateKeyPairSync"
        | "crypto.generateKeySync"
        | "crypto.generatePrimeSync"
        | "crypto.diffieHellman"
        | "crypto.diffieHellmanGroup"
        | "crypto.diffieHellmanSessionCreate"
        | "crypto.diffieHellmanSessionCall"
        | "crypto.subtle" => service_javascript_crypto_sync_rpc(process, request),
        "dns.lookup" | "dns.resolve" | "dns.resolve4" | "dns.resolve6" => {
            service_javascript_dns_sync_rpc(bridge, vm_id, dns, request)
        }
        "net.fetch" => service_javascript_fetch_sync_rpc(bridge, vm_id, request),
        "net.http_request" | "net.http_listen" | "net.http_close" | "net.http_wait"
        | "net.http_respond" => service_javascript_net_sync_rpc(
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
        "net.http2_server_listen"
        | "net.http2_server_poll"
        | "net.http2_server_close"
        | "net.http2_server_respond"
        | "net.http2_server_wait"
        | "net.http2_session_connect"
        | "net.http2_session_request"
        | "net.http2_session_settings"
        | "net.http2_session_set_local_window_size"
        | "net.http2_session_goaway"
        | "net.http2_session_close"
        | "net.http2_session_destroy"
        | "net.http2_session_poll"
        | "net.http2_session_wait"
        | "net.http2_stream_respond"
        | "net.http2_stream_push_stream"
        | "net.http2_stream_write"
        | "net.http2_stream_end"
        | "net.http2_stream_close"
        | "net.http2_stream_pause"
        | "net.http2_stream_resume"
        | "net.http2_stream_respond_with_file" => service_javascript_http2_sync_rpc(
            bridge,
            vm_id,
            dns,
            socket_paths,
            process,
            request,
            resource_limits,
            network_counts,
        ),
        "net.connect"
        | "net.listen"
        | "net.poll"
        | "net.socket_wait_connect"
        | "net.socket_read"
        | "net.socket_set_no_delay"
        | "net.socket_set_keep_alive"
        | "net.socket_upgrade_tls"
        | "net.socket_get_tls_client_hello"
        | "net.socket_tls_query"
        | "net.server_poll"
        | "net.server_accept"
        | "net.server_connections"
        | "net.upgrade_socket_write"
        | "net.upgrade_socket_end"
        | "net.upgrade_socket_destroy"
        | "net.write"
        | "net.shutdown"
        | "net.destroy"
        | "net.server_close"
        | "tls.get_ciphers" => service_javascript_net_sync_rpc(
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
        "dgram.createSocket"
        | "dgram.bind"
        | "dgram.send"
        | "dgram.poll"
        | "dgram.close"
        | "dgram.address"
        | "dgram.setBufferSize"
        | "dgram.getBufferSize" => service_javascript_dgram_sync_rpc(
            bridge,
            vm_id,
            dns,
            socket_paths,
            process,
            request,
            resource_limits,
            network_counts,
        ),
        "sqlite.constants"
        | "sqlite.open"
        | "sqlite.close"
        | "sqlite.exec"
        | "sqlite.query"
        | "sqlite.prepare"
        | "sqlite.location"
        | "sqlite.checkpoint"
        | "sqlite.statement.run"
        | "sqlite.statement.get"
        | "sqlite.statement.all"
        | "sqlite.statement.iterate"
        | "sqlite.statement.columns"
        | "sqlite.statement.setReturnArrays"
        | "sqlite.statement.setReadBigInts"
        | "sqlite.statement.setAllowBareNamedParameters"
        | "sqlite.statement.setAllowUnknownNamedParameters"
        | "sqlite.statement.finalize" => {
            service_javascript_sqlite_sync_rpc(kernel, process, request)
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

pub(crate) fn service_javascript_crypto_sync_rpc(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    match request.method.as_str() {
        "crypto.hashDigest" => {
            let algorithm = javascript_crypto_digest_algorithm(
                &request.args,
                0,
                "crypto.hashDigest algorithm",
            )?;
            let data = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.hashDigest data")?;
            Ok(Value::String(
                base64::engine::general_purpose::STANDARD.encode(algorithm.digest(&data)),
            ))
        }
        "crypto.hmacDigest" => {
            let algorithm = javascript_crypto_digest_algorithm(
                &request.args,
                0,
                "crypto.hmacDigest algorithm",
            )?;
            let key = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.hmacDigest key")?;
            let data = javascript_sync_rpc_base64_arg(&request.args, 2, "crypto.hmacDigest data")?;
            Ok(Value::String(
                base64::engine::general_purpose::STANDARD.encode(algorithm.hmac(&key, &data)?),
            ))
        }
        "crypto.pbkdf2" => {
            let password =
                javascript_sync_rpc_base64_arg(&request.args, 0, "crypto.pbkdf2 password")?;
            let salt = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.pbkdf2 salt")?;
            let iterations =
                javascript_sync_rpc_arg_u32(&request.args, 2, "crypto.pbkdf2 iterations")?;
            if iterations == 0 {
                return Err(SidecarError::InvalidState(String::from(
                    "crypto.pbkdf2 iterations must be greater than zero",
                )));
            }
            let key_len = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                3,
                "crypto.pbkdf2 key length",
            )?)
            .map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "crypto.pbkdf2 key length must fit within usize",
                ))
            })?;
            let algorithm =
                javascript_crypto_digest_algorithm(&request.args, 4, "crypto.pbkdf2 digest")?;
            let mut output = vec![0u8; key_len];
            algorithm.pbkdf2(&password, &salt, iterations, &mut output);
            Ok(Value::String(
                base64::engine::general_purpose::STANDARD.encode(output),
            ))
        }
        "crypto.scrypt" => {
            let password =
                javascript_sync_rpc_base64_arg(&request.args, 0, "crypto.scrypt password")?;
            let salt = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.scrypt salt")?;
            let key_len = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                2,
                "crypto.scrypt key length",
            )?)
            .map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "crypto.scrypt key length must fit within usize",
                ))
            })?;
            let options_json =
                javascript_sync_rpc_arg_str(&request.args, 3, "crypto.scrypt options")?;
            let options: JavascriptScryptOptions =
                serde_json::from_str(options_json).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "crypto.scrypt options must be valid JSON: {error}"
                    ))
                })?;
            let cost = options.cost.unwrap_or(DEFAULT_SCRYPT_COST);
            if cost == 0 || !cost.is_power_of_two() {
                return Err(SidecarError::InvalidState(String::from(
                    "crypto.scrypt cost must be a positive power of two",
                )));
            }
            let log_n = u8::try_from(cost.ilog2()).map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "crypto.scrypt cost exceeds supported parameter range",
                ))
            })?;
            let params = ScryptParams::new(
                log_n,
                options.block_size.unwrap_or(DEFAULT_SCRYPT_BLOCK_SIZE),
                options
                    .parallelization
                    .unwrap_or(DEFAULT_SCRYPT_PARALLELIZATION),
                key_len,
            )
            .map_err(|error| {
                SidecarError::InvalidState(format!("crypto.scrypt options are invalid: {error}"))
            })?;
            let mut output = vec![0u8; key_len];
            scrypt(&password, &salt, &params, &mut output).map_err(|error| {
                SidecarError::Execution(format!("crypto.scrypt failed: {error}"))
            })?;
            Ok(Value::String(
                base64::engine::general_purpose::STANDARD.encode(output),
            ))
        }
        "crypto.cipheriv" => service_javascript_crypto_cipheriv_sync_rpc(request),
        "crypto.decipheriv" => service_javascript_crypto_decipheriv_sync_rpc(request),
        "crypto.cipherivCreate" => {
            service_javascript_crypto_cipheriv_create_sync_rpc(process, request)
        }
        "crypto.cipherivUpdate" => {
            service_javascript_crypto_cipheriv_update_sync_rpc(process, request)
        }
        "crypto.cipherivFinal" => {
            service_javascript_crypto_cipheriv_final_sync_rpc(process, request)
        }
        "crypto.sign" => service_javascript_crypto_sign_sync_rpc(request),
        "crypto.verify" => service_javascript_crypto_verify_sync_rpc(request),
        "crypto.asymmetricOp" => service_javascript_crypto_asymmetric_op_sync_rpc(request),
        "crypto.createKeyObject" => service_javascript_crypto_create_key_object_sync_rpc(request),
        "crypto.generateKeyPairSync" => {
            service_javascript_crypto_generate_key_pair_sync_rpc(request)
        }
        "crypto.generateKeySync" => service_javascript_crypto_generate_key_sync_rpc(request),
        "crypto.generatePrimeSync" => service_javascript_crypto_generate_prime_sync_rpc(request),
        "crypto.diffieHellman" => service_javascript_crypto_diffie_hellman_sync_rpc(request),
        "crypto.diffieHellmanGroup" => {
            service_javascript_crypto_diffie_hellman_group_sync_rpc(request)
        }
        "crypto.diffieHellmanSessionCreate" => {
            service_javascript_crypto_diffie_hellman_session_create_sync_rpc(process, request)
        }
        "crypto.diffieHellmanSessionCall" => {
            service_javascript_crypto_diffie_hellman_session_call_sync_rpc(process, request)
        }
        "crypto.subtle" => service_javascript_crypto_subtle_sync_rpc(request),
        _ => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript crypto sync RPC method {}",
            request.method
        ))),
    }
}

fn javascript_crypto_digest_algorithm(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<JavascriptCryptoDigestAlgorithm, SidecarError> {
    JavascriptCryptoDigestAlgorithm::parse(javascript_sync_rpc_arg_str(args, index, label)?)
}

impl JavascriptCryptoDigestAlgorithm {
    fn parse(value: &str) -> Result<Self, SidecarError> {
        match value.trim().to_ascii_lowercase().replace('-', "").as_str() {
            "md5" => Ok(Self::Md5),
            "sha1" => Ok(Self::Sha1),
            "sha256" => Ok(Self::Sha256),
            "sha512" => Ok(Self::Sha512),
            _ => Err(SidecarError::InvalidState(format!(
                "unsupported crypto digest algorithm {value}"
            ))),
        }
    }

    fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Md5 => Md5::digest(data).to_vec(),
            Self::Sha1 => Sha1::digest(data).to_vec(),
            Self::Sha256 => Sha256::digest(data).to_vec(),
            Self::Sha512 => Sha512::digest(data).to_vec(),
        }
    }

    fn hmac(self, key: &[u8], data: &[u8]) -> Result<Vec<u8>, SidecarError> {
        match self {
            Self::Md5 => {
                let mut mac = Hmac::<Md5>::new_from_slice(key).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid HMAC key: {error}"))
                })?;
                mac.update(data);
                Ok(mac.finalize().into_bytes().to_vec())
            }
            Self::Sha1 => {
                let mut mac = Hmac::<Sha1>::new_from_slice(key).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid HMAC key: {error}"))
                })?;
                mac.update(data);
                Ok(mac.finalize().into_bytes().to_vec())
            }
            Self::Sha256 => {
                let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid HMAC key: {error}"))
                })?;
                mac.update(data);
                Ok(mac.finalize().into_bytes().to_vec())
            }
            Self::Sha512 => {
                let mut mac = Hmac::<Sha512>::new_from_slice(key).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid HMAC key: {error}"))
                })?;
                mac.update(data);
                Ok(mac.finalize().into_bytes().to_vec())
            }
        }
    }

    fn pbkdf2(self, password: &[u8], salt: &[u8], iterations: u32, output: &mut [u8]) {
        match self {
            Self::Md5 => pbkdf2_hmac::<Md5>(password, salt, iterations, output),
            Self::Sha1 => pbkdf2_hmac::<Sha1>(password, salt, iterations, output),
            Self::Sha256 => pbkdf2_hmac::<Sha256>(password, salt, iterations, output),
            Self::Sha512 => pbkdf2_hmac::<Sha512>(password, salt, iterations, output),
        }
    }
}

#[derive(Debug, Clone)]
enum JavascriptCryptoKeyMaterial {
    Private(PKey<Private>),
    Public(PKey<Public>),
    Secret(Vec<u8>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct JavascriptSerializedSandboxKeyObject {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pem: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "asymmetricKeyType")]
    asymmetric_key_type: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "asymmetricKeyDetails"
    )]
    asymmetric_key_details: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    jwk: Option<Value>,
}

#[derive(Debug, Clone)]
struct JavascriptDirectKeyInput {
    key: JavascriptCryptoKeyMaterial,
    padding: Option<Padding>,
}

fn service_javascript_crypto_cipheriv_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    service_javascript_crypto_cipheriv_inner(request, false)
}

fn service_javascript_crypto_decipheriv_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    service_javascript_crypto_cipheriv_inner(request, true)
}

fn service_javascript_crypto_cipheriv_create_sync_rpc(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let mode = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.cipherivCreate mode")?;
    let decrypt = mode == "decipher";
    let algorithm =
        javascript_sync_rpc_arg_str(&request.args, 1, "crypto.cipherivCreate algorithm")?;
    let key = javascript_sync_rpc_base64_arg(&request.args, 2, "crypto.cipherivCreate key")?;
    let iv = javascript_sync_rpc_base64_arg_optional(&request.args, 3, "crypto.cipherivCreate iv")?;
    let options =
        javascript_sync_rpc_json_arg_optional(&request.args, 4, "crypto.cipherivCreate options")?;
    let auth_tag_len = javascript_crypto_requested_aead_tag_len(&algorithm, options.as_ref())?;
    let context = javascript_crypto_build_cipher_context(
        &algorithm,
        &key,
        iv.as_deref(),
        decrypt,
        options.as_ref(),
    )?;
    process.next_cipher_session_id += 1;
    let session_id = process.next_cipher_session_id;
    process.cipher_sessions.insert(
        session_id,
        ActiveCipherSession {
            algorithm: algorithm.to_string(),
            auth_tag_len,
            context,
        },
    );
    Ok(json!(session_id))
}

fn service_javascript_crypto_cipheriv_update_sync_rpc(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let session_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "crypto.cipherivUpdate session id")?;
    let data = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.cipherivUpdate data")?;
    let session = process
        .cipher_sessions
        .get_mut(&session_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("Cipher session {session_id} not found"))
        })?;
    let result = javascript_crypto_cipher_update(&mut session.context, &data)?;
    Ok(Value::String(
        base64::engine::general_purpose::STANDARD.encode(result),
    ))
}

fn service_javascript_crypto_cipheriv_final_sync_rpc(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let session_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "crypto.cipherivFinal session id")?;
    let mut session = process.cipher_sessions.remove(&session_id).ok_or_else(|| {
        SidecarError::InvalidState(format!("Cipher session {session_id} not found"))
    })?;
    let data = javascript_crypto_cipher_finalize(&mut session.context)?;
    let mut response = Map::new();
    response.insert(
        String::from("data"),
        Value::String(base64::engine::general_purpose::STANDARD.encode(data)),
    );
    if javascript_crypto_is_aead(&session.algorithm) {
        let mut auth_tag = vec![0_u8; session.auth_tag_len];
        session
            .context
            .get_tag(&mut auth_tag)
            .map_err(javascript_crypto_openssl_error)?;
        response.insert(
            String::from("authTag"),
            Value::String(base64::engine::general_purpose::STANDARD.encode(auth_tag)),
        );
    }
    Ok(Value::String(serde_json::to_string(&response).map_err(
        |error| SidecarError::InvalidState(format!("serialize cipher final response: {error}")),
    )?))
}

fn service_javascript_crypto_sign_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let algorithm = request.args.first().and_then(Value::as_str);
    let data = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.sign data")?;
    let key_json = javascript_sync_rpc_arg_str(&request.args, 2, "crypto.sign key")?;
    let key_input =
        javascript_crypto_parse_direct_key_input(key_json, Some("private"), "crypto.sign key")?;
    let private_key = javascript_crypto_expect_private_key(key_input.key, "crypto.sign key")?;
    let mut signer = javascript_crypto_new_signer(algorithm, &private_key)?;
    if let Some(padding) = key_input.padding {
        signer
            .set_rsa_padding(padding)
            .map_err(javascript_crypto_openssl_error)?;
    }
    signer
        .update(&data)
        .map_err(javascript_crypto_openssl_error)?;
    Ok(Value::String(
        base64::engine::general_purpose::STANDARD.encode(
            signer
                .sign_to_vec()
                .map_err(javascript_crypto_openssl_error)?,
        ),
    ))
}

fn service_javascript_crypto_verify_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let algorithm = request.args.first().and_then(Value::as_str);
    let data = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.verify data")?;
    let key_json = javascript_sync_rpc_arg_str(&request.args, 2, "crypto.verify key")?;
    let signature = javascript_sync_rpc_base64_arg(&request.args, 3, "crypto.verify signature")?;
    let key_input =
        javascript_crypto_parse_direct_key_input(key_json, Some("public"), "crypto.verify key")?;
    let public_key = javascript_crypto_expect_public_key(key_input.key, "crypto.verify key")?;
    let mut verifier = javascript_crypto_new_verifier(algorithm, &public_key)?;
    if let Some(padding) = key_input.padding {
        verifier
            .set_rsa_padding(padding)
            .map_err(javascript_crypto_openssl_error)?;
    }
    verifier
        .update(&data)
        .map_err(javascript_crypto_openssl_error)?;
    Ok(json!(
        verifier
            .verify(&signature)
            .map_err(javascript_crypto_openssl_error)?
    ))
}

fn service_javascript_crypto_asymmetric_op_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let operation = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.asymmetricOp operation")?;
    let key_json = javascript_sync_rpc_arg_str(&request.args, 1, "crypto.asymmetricOp key")?;
    let data = javascript_sync_rpc_base64_arg(&request.args, 2, "crypto.asymmetricOp data")?;
    let expect_kind = match operation {
        "publicEncrypt" | "publicDecrypt" => Some("public"),
        "privateEncrypt" | "privateDecrypt" => Some("private"),
        other => {
            return Err(SidecarError::InvalidState(format!(
                "Unsupported asymmetric crypto operation: {other}"
            )));
        }
    };
    let key_input =
        javascript_crypto_parse_direct_key_input(key_json, expect_kind, "crypto.asymmetricOp key")?;
    let padding = key_input.padding.unwrap_or(Padding::PKCS1);
    let mut output = vec![0_u8; javascript_crypto_rsa_output_size(&key_input.key)?];
    let written = match (operation, key_input.key) {
        ("publicEncrypt", JavascriptCryptoKeyMaterial::Public(key))
        | ("publicDecrypt", JavascriptCryptoKeyMaterial::Public(key)) => {
            let rsa = key.rsa().map_err(javascript_crypto_openssl_error)?;
            if operation == "publicEncrypt" {
                rsa.public_encrypt(&data, &mut output, padding)
                    .map_err(javascript_crypto_openssl_error)?
            } else {
                rsa.public_decrypt(&data, &mut output, padding)
                    .map_err(javascript_crypto_openssl_error)?
            }
        }
        ("privateEncrypt", JavascriptCryptoKeyMaterial::Private(key))
        | ("privateDecrypt", JavascriptCryptoKeyMaterial::Private(key)) => {
            let rsa = key.rsa().map_err(javascript_crypto_openssl_error)?;
            if operation == "privateEncrypt" {
                rsa.private_encrypt(&data, &mut output, padding)
                    .map_err(javascript_crypto_openssl_error)?
            } else {
                rsa.private_decrypt(&data, &mut output, padding)
                    .map_err(javascript_crypto_openssl_error)?
            }
        }
        _ => {
            return Err(SidecarError::InvalidState(format!(
                "{operation} requires an RSA {} key",
                expect_kind.unwrap_or("asymmetric")
            )));
        }
    };
    output.truncate(written);
    Ok(Value::String(
        base64::engine::general_purpose::STANDARD.encode(output),
    ))
}

fn service_javascript_crypto_create_key_object_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let operation =
        javascript_sync_rpc_arg_str(&request.args, 0, "crypto.createKeyObject operation")?;
    let key_json = javascript_sync_rpc_arg_str(&request.args, 1, "crypto.createKeyObject key")?;
    let expected = match operation {
        "createPrivateKey" => Some("private"),
        "createPublicKey" => Some("public"),
        other => {
            return Err(SidecarError::InvalidState(format!(
                "Unsupported key creation operation: {other}"
            )));
        }
    };
    let key_input =
        javascript_crypto_parse_direct_key_input(key_json, expected, "crypto.createKeyObject key")?;
    Ok(Value::String(
        serde_json::to_string(&javascript_crypto_serialize_sandbox_key_object(
            &key_input.key,
        )?)
        .map_err(|error| {
            SidecarError::InvalidState(format!("serialize crypto key object: {error}"))
        })?,
    ))
}

fn service_javascript_crypto_generate_key_pair_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let key_type =
        javascript_sync_rpc_arg_str(&request.args, 0, "crypto.generateKeyPairSync type")?;
    let options = javascript_crypto_parse_serialized_options_arg(
        &request.args,
        1,
        "crypto.generateKeyPairSync options",
    )?
    .unwrap_or(Value::Object(Map::new()));
    let public_encoding = options.get("publicKeyEncoding").cloned();
    let private_encoding = options.get("privateKeyEncoding").cloned();

    let private_key = match key_type {
        "rsa" => {
            let bits = options
                .get("modulusLength")
                .and_then(Value::as_u64)
                .unwrap_or(2048) as u32;
            let exponent = options
                .get("publicExponent")
                .map(|value| javascript_crypto_u32_from_bridge_value(value, "rsa publicExponent"))
                .transpose()?
                .unwrap_or(65_537);
            let exponent = BigNum::from_u32(exponent).map_err(javascript_crypto_openssl_error)?;
            let rsa =
                Rsa::generate_with_e(bits, &exponent).map_err(javascript_crypto_openssl_error)?;
            PKey::from_rsa(rsa).map_err(javascript_crypto_openssl_error)?
        }
        "ec" => {
            let named_curve = options
                .get("namedCurve")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "crypto.generateKeyPairSync ec requires namedCurve",
                    ))
                })?;
            let group = EcGroup::from_curve_name(javascript_crypto_curve_nid(named_curve)?)
                .map_err(javascript_crypto_openssl_error)?;
            let key = EcKey::generate(&group).map_err(javascript_crypto_openssl_error)?;
            PKey::from_ec_key(key).map_err(javascript_crypto_openssl_error)?
        }
        "ed25519" => PKey::generate_ed25519().map_err(javascript_crypto_openssl_error)?,
        "x25519" => PKey::generate_x25519().map_err(javascript_crypto_openssl_error)?,
        other => {
            return Err(SidecarError::InvalidState(format!(
                "unsupported crypto key pair type {other}"
            )));
        }
    };
    let public_key = PKey::public_key_from_pem(
        &private_key
            .public_key_to_pem()
            .map_err(javascript_crypto_openssl_error)?,
    )
    .map_err(javascript_crypto_openssl_error)?;
    let response = if public_encoding.is_some() || private_encoding.is_some() {
        json!({
            "publicKey": javascript_crypto_serialize_encoded_key_value_public(&public_key, public_encoding.as_ref())?,
            "privateKey": javascript_crypto_serialize_encoded_key_value_private(&private_key, private_encoding.as_ref())?,
        })
    } else {
        json!({
            "publicKey": javascript_crypto_serialize_sandbox_key_object(&JavascriptCryptoKeyMaterial::Public(public_key))?,
            "privateKey": javascript_crypto_serialize_sandbox_key_object(&JavascriptCryptoKeyMaterial::Private(private_key))?,
        })
    };
    Ok(Value::String(serde_json::to_string(&response).map_err(
        |error| SidecarError::InvalidState(format!("serialize generated key pair: {error}")),
    )?))
}

fn service_javascript_crypto_generate_key_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let key_type = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.generateKeySync type")?;
    let options = javascript_crypto_parse_serialized_options_arg(
        &request.args,
        1,
        "crypto.generateKeySync options",
    )?
    .unwrap_or(Value::Object(Map::new()));
    let bit_length = options
        .get("length")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "crypto.generateKeySync options.length is required",
            ))
        })? as usize;
    let mut raw = vec![0_u8; bit_length.div_ceil(8)];
    rand_bytes(&mut raw).map_err(javascript_crypto_openssl_error)?;
    let serialized = match key_type {
        "hmac" => javascript_crypto_serialize_sandbox_key_object(
            &JavascriptCryptoKeyMaterial::Secret(raw),
        )?,
        "aes" => javascript_crypto_serialize_sandbox_key_object(
            &JavascriptCryptoKeyMaterial::Secret(raw),
        )?,
        other => {
            return Err(SidecarError::InvalidState(format!(
                "unsupported crypto.generateKeySync type {other}"
            )));
        }
    };
    Ok(Value::String(serde_json::to_string(&serialized).map_err(
        |error| SidecarError::InvalidState(format!("serialize generated key: {error}")),
    )?))
}

fn service_javascript_crypto_generate_prime_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let bits =
        javascript_sync_rpc_arg_u64(&request.args, 0, "crypto.generatePrimeSync size")? as i32;
    let options = javascript_crypto_parse_serialized_options_arg(
        &request.args,
        1,
        "crypto.generatePrimeSync options",
    )?
    .unwrap_or(Value::Object(Map::new()));
    let safe = options
        .get("safe")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let add = options
        .get("add")
        .map(|value| javascript_crypto_bignum_from_bridge_value(value, "prime add"))
        .transpose()?;
    let rem = options
        .get("rem")
        .map(|value| javascript_crypto_bignum_from_bridge_value(value, "prime rem"))
        .transpose()?;
    let mut prime = BigNum::new().map_err(javascript_crypto_openssl_error)?;
    prime
        .generate_prime(bits, safe, add.as_deref(), rem.as_deref())
        .map_err(javascript_crypto_openssl_error)?;
    let payload = if options
        .get("bigint")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        json!({
            "__type": "bigint",
            "value": prime.to_dec_str().map_err(javascript_crypto_openssl_error)?.to_string(),
        })
    } else {
        json!({
            "__type": "buffer",
            "value": base64::engine::general_purpose::STANDARD.encode(prime.to_vec()),
        })
    };
    Ok(Value::String(serde_json::to_string(&payload).map_err(
        |error| SidecarError::InvalidState(format!("serialize generated prime: {error}")),
    )?))
}

fn service_javascript_crypto_diffie_hellman_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let options = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.diffieHellman options")?;
    let parsed: Value = serde_json::from_str(options).map_err(|error| {
        SidecarError::InvalidState(format!(
            "crypto.diffieHellman options must be valid JSON: {error}"
        ))
    })?;
    let private_key = javascript_crypto_parse_key_material_value(
        parsed.get("privateKey").ok_or_else(|| {
            SidecarError::InvalidState(String::from("crypto.diffieHellman missing privateKey"))
        })?,
        Some("private"),
        "crypto.diffieHellman privateKey",
    )?;
    let public_key = javascript_crypto_parse_key_material_value(
        parsed.get("publicKey").ok_or_else(|| {
            SidecarError::InvalidState(String::from("crypto.diffieHellman missing publicKey"))
        })?,
        Some("public"),
        "crypto.diffieHellman publicKey",
    )?;
    let private_key =
        javascript_crypto_expect_private_key(private_key, "crypto.diffieHellman privateKey")?;
    let public_key =
        javascript_crypto_expect_public_key(public_key, "crypto.diffieHellman publicKey")?;
    let mut deriver = Deriver::new(&private_key).map_err(javascript_crypto_openssl_error)?;
    deriver
        .set_peer(&public_key)
        .map_err(javascript_crypto_openssl_error)?;
    let secret = deriver
        .derive_to_vec()
        .map_err(javascript_crypto_openssl_error)?;
    Ok(Value::String(
        serde_json::to_string(&json!({
            "__type": "buffer",
            "value": base64::engine::general_purpose::STANDARD.encode(secret),
        }))
        .map_err(|error| {
            SidecarError::InvalidState(format!("serialize derived secret: {error}"))
        })?,
    ))
}

fn service_javascript_crypto_diffie_hellman_group_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let name = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.diffieHellmanGroup name")?;
    let params = javascript_crypto_named_dh_group(name)?;
    let response = json!({
        "prime": {
            "__type": "buffer",
            "value": base64::engine::general_purpose::STANDARD.encode(params.prime_p().to_vec()),
        },
        "generator": {
            "__type": "buffer",
            "value": base64::engine::general_purpose::STANDARD.encode(params.generator().to_vec()),
        },
    });
    Ok(Value::String(serde_json::to_string(&response).map_err(
        |error| {
            SidecarError::InvalidState(format!("serialize diffieHellmanGroup response: {error}"))
        },
    )?))
}

fn service_javascript_crypto_diffie_hellman_session_create_sync_rpc(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let raw = javascript_sync_rpc_arg_str(
        &request.args,
        0,
        "crypto.diffieHellmanSessionCreate request",
    )?;
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        SidecarError::InvalidState(format!(
            "crypto.diffieHellmanSessionCreate request must be valid JSON: {error}"
        ))
    })?;
    let session = match parsed.get("type").and_then(Value::as_str) {
        Some("group") => {
            let name = parsed.get("name").and_then(Value::as_str).ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "crypto.diffieHellmanSessionCreate group requires name",
                ))
            })?;
            ActiveDiffieHellmanSession::Dh(ActiveDhSession {
                params: javascript_crypto_named_dh_group(name)?,
                key_pair: None,
            })
        }
        Some("dh") => {
            let args = parsed
                .get("args")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "crypto.diffieHellmanSessionCreate dh requires args",
                    ))
                })?;
            let params = javascript_crypto_build_dh_params(args)?;
            ActiveDiffieHellmanSession::Dh(ActiveDhSession {
                params,
                key_pair: None,
            })
        }
        Some("ecdh") => {
            let curve = parsed.get("name").and_then(Value::as_str).ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "crypto.diffieHellmanSessionCreate ecdh requires name",
                ))
            })?;
            ActiveDiffieHellmanSession::Ecdh(ActiveEcdhSession {
                curve: curve.to_string(),
                key_pair: None,
            })
        }
        other => {
            return Err(SidecarError::InvalidState(format!(
                "Unsupported Diffie-Hellman session type: {}",
                other.unwrap_or("<missing>")
            )));
        }
    };
    process.next_diffie_hellman_session_id += 1;
    let session_id = process.next_diffie_hellman_session_id;
    process.diffie_hellman_sessions.insert(session_id, session);
    Ok(json!(session_id))
}

fn service_javascript_crypto_diffie_hellman_session_call_sync_rpc(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let session_id = javascript_sync_rpc_arg_u64(
        &request.args,
        0,
        "crypto.diffieHellmanSessionCall session id",
    )?;
    let raw =
        javascript_sync_rpc_arg_str(&request.args, 1, "crypto.diffieHellmanSessionCall request")?;
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        SidecarError::InvalidState(format!(
            "crypto.diffieHellmanSessionCall request must be valid JSON: {error}"
        ))
    })?;
    let method = parsed
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "crypto.diffieHellmanSessionCall request missing method",
            ))
        })?;
    let args = parsed
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let session = process
        .diffie_hellman_sessions
        .get_mut(&session_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("Diffie-Hellman session {session_id} not found"))
        })?;
    let (result, has_result) = match session {
        ActiveDiffieHellmanSession::Dh(session) => {
            javascript_crypto_call_dh_session(session, method, &args)?
        }
        ActiveDiffieHellmanSession::Ecdh(session) => {
            javascript_crypto_call_ecdh_session(session, method, &args)?
        }
    };
    Ok(Value::String(
        serde_json::to_string(&json!({
            "result": result,
            "hasResult": has_result,
        }))
        .map_err(|error| {
            SidecarError::InvalidState(format!("serialize diffie session result: {error}"))
        })?,
    ))
}

fn service_javascript_crypto_subtle_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let raw = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.subtle request")?;
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        SidecarError::InvalidState(format!("crypto.subtle request must be valid JSON: {error}"))
    })?;
    let op = parsed.get("op").and_then(Value::as_str).ok_or_else(|| {
        SidecarError::InvalidState(String::from("crypto.subtle request missing op"))
    })?;
    match op {
        "digest" => {
            let algorithm = parsed
                .get("algorithm")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "crypto.subtle.digest missing algorithm",
                    ))
                })?;
            let data = parsed.get("data").and_then(Value::as_str).ok_or_else(|| {
                SidecarError::InvalidState(String::from("crypto.subtle.digest missing data"))
            })?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|error| {
                    SidecarError::InvalidState(format!("crypto.subtle.digest data base64: {error}"))
                })?;
            let digest = JavascriptCryptoDigestAlgorithm::parse(algorithm)?.digest(&bytes);
            Ok(Value::String(
                serde_json::to_string(&json!({
                    "data": base64::engine::general_purpose::STANDARD.encode(digest),
                }))
                .map_err(|error| {
                    SidecarError::InvalidState(format!("serialize crypto.subtle digest: {error}"))
                })?,
            ))
        }
        _ => Err(SidecarError::InvalidState(format!(
            "Unsupported subtle operation: {op}"
        ))),
    }
}

fn service_javascript_crypto_cipheriv_inner(
    request: &JavascriptSyncRpcRequest,
    decrypt: bool,
) -> Result<Value, SidecarError> {
    let label = if decrypt {
        "crypto.decipheriv"
    } else {
        "crypto.cipheriv"
    };
    let algorithm = javascript_sync_rpc_arg_str(&request.args, 0, &format!("{label} algorithm"))?;
    let key = javascript_sync_rpc_base64_arg(&request.args, 1, &format!("{label} key"))?;
    let iv = javascript_sync_rpc_base64_arg_optional(&request.args, 2, &format!("{label} iv"))?;
    let data = javascript_sync_rpc_base64_arg(&request.args, 3, &format!("{label} data"))?;
    let options =
        javascript_sync_rpc_json_arg_optional(&request.args, 4, &format!("{label} options"))?;
    let auth_tag_len = javascript_crypto_requested_aead_tag_len(&algorithm, options.as_ref())?;
    let mut context = javascript_crypto_build_cipher_context(
        &algorithm,
        &key,
        iv.as_deref(),
        decrypt,
        options.as_ref(),
    )?;
    let payload = javascript_crypto_cipher_update(&mut context, &data)?;
    let final_bytes = javascript_crypto_cipher_finalize(&mut context)?;
    if decrypt {
        let mut output = payload;
        output.extend(final_bytes);
        return Ok(Value::String(
            base64::engine::general_purpose::STANDARD.encode(output),
        ));
    }

    let mut response = Map::new();
    let mut encrypted = payload;
    encrypted.extend(final_bytes);
    response.insert(
        String::from("data"),
        Value::String(base64::engine::general_purpose::STANDARD.encode(encrypted)),
    );
    if javascript_crypto_is_aead(&algorithm) {
        let mut auth_tag = vec![0_u8; auth_tag_len];
        context
            .get_tag(&mut auth_tag)
            .map_err(javascript_crypto_openssl_error)?;
        response.insert(
            String::from("authTag"),
            Value::String(base64::engine::general_purpose::STANDARD.encode(auth_tag)),
        );
    }
    Ok(Value::String(serde_json::to_string(&response).map_err(
        |error| SidecarError::InvalidState(format!("serialize {label} response: {error}")),
    )?))
}

fn javascript_sync_rpc_base64_arg_optional(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<Vec<u8>>, SidecarError> {
    if args.get(index).is_none() || args[index].is_null() {
        return Ok(None);
    }
    javascript_sync_rpc_base64_arg(args, index, label).map(Some)
}

fn javascript_sync_rpc_json_arg_optional(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<Value>, SidecarError> {
    if args.get(index).is_none() || args[index].is_null() {
        return Ok(None);
    }
    let raw = javascript_sync_rpc_arg_str(args, index, label)?;
    serde_json::from_str(raw)
        .map(Some)
        .map_err(|error| SidecarError::InvalidState(format!("{label} must be valid JSON: {error}")))
}

fn javascript_crypto_parse_direct_key_input(
    raw: &str,
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptDirectKeyInput, SidecarError> {
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        SidecarError::InvalidState(format!("{label} must be valid JSON: {error}"))
    })?;
    let padding = match parsed.as_object().and_then(|value| value.get("padding")) {
        Some(value) => javascript_crypto_padding_from_value(value)?,
        None => None,
    };
    Ok(JavascriptDirectKeyInput {
        key: javascript_crypto_parse_key_material_value(&parsed, expected, label)?,
        padding,
    })
}

fn javascript_crypto_parse_key_material_value(
    value: &Value,
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptCryptoKeyMaterial, SidecarError> {
    if let Some(object) = value.as_object() {
        if object.get("__type").and_then(Value::as_str) == Some("keyObject") {
            let serialized = object.get("value").ok_or_else(|| {
                SidecarError::InvalidState(format!("{label} keyObject is missing a value"))
            })?;
            return javascript_crypto_parse_serialized_key_object(serialized, expected, label);
        }
        if object.contains_key("type") && (object.contains_key("pem") || object.contains_key("raw"))
        {
            return javascript_crypto_parse_serialized_key_object(value, expected, label);
        }
        if let Some(source) = object.get("key") {
            return javascript_crypto_parse_key_source(
                source,
                object.get("format").and_then(Value::as_str),
                object.get("type").and_then(Value::as_str),
                expected,
                label,
            );
        }
    }
    javascript_crypto_parse_key_source(value, None, None, expected, label)
}

fn javascript_crypto_parse_key_source(
    source: &Value,
    format: Option<&str>,
    kind: Option<&str>,
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptCryptoKeyMaterial, SidecarError> {
    match source {
        Value::String(pem) => javascript_crypto_parse_key_from_pem(pem.as_bytes(), expected, label),
        Value::Object(object) if object.get("__type").and_then(Value::as_str) == Some("buffer") => {
            let data = javascript_crypto_decode_bridge_buffer(source, label)?;
            javascript_crypto_parse_key_from_bytes(&data, format, kind, expected, label)
        }
        Value::Object(_) => {
            if format == Some("jwk") {
                return Err(SidecarError::InvalidState(format!(
                    "{label} jwk inputs are not supported yet"
                )));
            }
            Err(SidecarError::InvalidState(format!(
                "{label} has an unsupported key shape"
            )))
        }
        _ => Err(SidecarError::InvalidState(format!(
            "{label} has an unsupported key value"
        ))),
    }
}

fn javascript_crypto_parse_key_from_pem(
    pem: &[u8],
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptCryptoKeyMaterial, SidecarError> {
    match expected {
        Some("private") => PKey::private_key_from_pem(pem)
            .map(JavascriptCryptoKeyMaterial::Private)
            .map_err(|error| {
                SidecarError::InvalidState(format!("{label} private key is invalid: {error}"))
            }),
        Some("public") => PKey::public_key_from_pem(pem)
            .map(JavascriptCryptoKeyMaterial::Public)
            .map_err(|error| {
                SidecarError::InvalidState(format!("{label} public key is invalid: {error}"))
            }),
        _ => PKey::private_key_from_pem(pem)
            .map(JavascriptCryptoKeyMaterial::Private)
            .or_else(|_| PKey::public_key_from_pem(pem).map(JavascriptCryptoKeyMaterial::Public))
            .map_err(|error| {
                SidecarError::InvalidState(format!("{label} PEM key is invalid: {error}"))
            }),
    }
}

fn javascript_crypto_parse_key_from_bytes(
    der: &[u8],
    format: Option<&str>,
    kind: Option<&str>,
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptCryptoKeyMaterial, SidecarError> {
    match (format.unwrap_or("der"), kind.or(expected)) {
        ("der", Some("pkcs8")) | ("der", Some("private")) => PKey::private_key_from_der(der)
            .map(JavascriptCryptoKeyMaterial::Private)
            .map_err(|error| {
                SidecarError::InvalidState(format!("{label} private key DER is invalid: {error}"))
            }),
        ("der", Some("spki")) | ("der", Some("public")) => PKey::public_key_from_der(der)
            .map(JavascriptCryptoKeyMaterial::Public)
            .map_err(|error| {
                SidecarError::InvalidState(format!("{label} public key DER is invalid: {error}"))
            }),
        _ => Err(SidecarError::InvalidState(format!(
            "{label} unsupported key bytes format"
        ))),
    }
}

fn javascript_crypto_parse_serialized_key_object(
    value: &Value,
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptCryptoKeyMaterial, SidecarError> {
    let serialized: JavascriptSerializedSandboxKeyObject = serde_json::from_value(value.clone())
        .map_err(|error| {
            SidecarError::InvalidState(format!("{label} keyObject is invalid: {error}"))
        })?;
    match serialized.kind.as_str() {
        "secret" => {
            if expected == Some("public") || expected == Some("private") {
                return Err(SidecarError::InvalidState(format!(
                    "{label} expected an asymmetric key"
                )));
            }
            Ok(JavascriptCryptoKeyMaterial::Secret(
                base64::engine::general_purpose::STANDARD
                    .decode(serialized.raw.unwrap_or_default())
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "{label} secret key contains invalid base64: {error}"
                        ))
                    })?,
            ))
        }
        "private" => {
            let pem = serialized.pem.ok_or_else(|| {
                SidecarError::InvalidState(format!("{label} private keyObject is missing pem"))
            })?;
            javascript_crypto_parse_key_from_pem(pem.as_bytes(), Some("private"), label)
        }
        "public" => {
            let pem = serialized.pem.ok_or_else(|| {
                SidecarError::InvalidState(format!("{label} public keyObject is missing pem"))
            })?;
            javascript_crypto_parse_key_from_pem(pem.as_bytes(), Some("public"), label)
        }
        other => Err(SidecarError::InvalidState(format!(
            "{label} has unsupported keyObject type {other}"
        ))),
    }
}

fn javascript_crypto_expect_private_key(
    key: JavascriptCryptoKeyMaterial,
    label: &str,
) -> Result<PKey<Private>, SidecarError> {
    match key {
        JavascriptCryptoKeyMaterial::Private(key) => Ok(key),
        _ => Err(SidecarError::InvalidState(format!(
            "{label} requires a private key"
        ))),
    }
}

fn javascript_crypto_expect_public_key(
    key: JavascriptCryptoKeyMaterial,
    label: &str,
) -> Result<PKey<Public>, SidecarError> {
    match key {
        JavascriptCryptoKeyMaterial::Public(key) => Ok(key),
        JavascriptCryptoKeyMaterial::Private(key) => {
            let pem = key
                .public_key_to_pem()
                .map_err(javascript_crypto_openssl_error)?;
            PKey::public_key_from_pem(&pem).map_err(javascript_crypto_openssl_error)
        }
        _ => Err(SidecarError::InvalidState(format!(
            "{label} requires a public key"
        ))),
    }
}

fn javascript_crypto_new_signer<'a>(
    algorithm: Option<&'a str>,
    key: &'a PKey<Private>,
) -> Result<Signer<'a>, SidecarError> {
    if matches!(key.id(), PKeyId::ED25519 | PKeyId::ED448) || algorithm.is_none() {
        return Signer::new_without_digest(key).map_err(javascript_crypto_openssl_error);
    }
    Signer::new(
        javascript_crypto_message_digest_from_name(algorithm.ok_or_else(|| {
            SidecarError::InvalidState(String::from("crypto.sign requires a digest algorithm"))
        })?)?,
        key,
    )
    .map_err(javascript_crypto_openssl_error)
}

fn javascript_crypto_new_verifier<'a>(
    algorithm: Option<&'a str>,
    key: &'a PKey<Public>,
) -> Result<Verifier<'a>, SidecarError> {
    if matches!(key.id(), PKeyId::ED25519 | PKeyId::ED448) || algorithm.is_none() {
        return Verifier::new_without_digest(key).map_err(javascript_crypto_openssl_error);
    }
    Verifier::new(
        javascript_crypto_message_digest_from_name(algorithm.ok_or_else(|| {
            SidecarError::InvalidState(String::from("crypto.verify requires a digest algorithm"))
        })?)?,
        key,
    )
    .map_err(javascript_crypto_openssl_error)
}

fn javascript_crypto_message_digest_from_name(name: &str) -> Result<MessageDigest, SidecarError> {
    match name.trim().to_ascii_lowercase().replace('-', "").as_str() {
        "md5" => Ok(MessageDigest::md5()),
        "sha1" => Ok(MessageDigest::sha1()),
        "sha256" => Ok(MessageDigest::sha256()),
        "sha384" => Ok(MessageDigest::sha384()),
        "sha512" => Ok(MessageDigest::sha512()),
        other => Err(SidecarError::InvalidState(format!(
            "unsupported crypto digest algorithm {other}"
        ))),
    }
}

fn javascript_crypto_padding_from_value(value: &Value) -> Result<Option<Padding>, SidecarError> {
    let Some(number) = value.as_i64() else {
        return Ok(None);
    };
    let padding = match number {
        1 => Padding::PKCS1,
        3 => Padding::NONE,
        4 => Padding::PKCS1_OAEP,
        6 => Padding::PKCS1_PSS,
        other => {
            return Err(SidecarError::InvalidState(format!(
                "unsupported RSA padding constant {other}"
            )));
        }
    };
    Ok(Some(padding))
}

fn javascript_crypto_decode_bridge_buffer(
    value: &Value,
    label: &str,
) -> Result<Vec<u8>, SidecarError> {
    let base64_value = value
        .as_object()
        .filter(|object| object.get("__type").and_then(Value::as_str) == Some("buffer"))
        .and_then(|object| object.get("value"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("{label} must be a serialized bridge buffer"))
        })?;
    base64::engine::general_purpose::STANDARD
        .decode(base64_value)
        .map_err(|error| {
            SidecarError::InvalidState(format!("{label} contains invalid base64: {error}"))
        })
}

fn javascript_crypto_serialize_sandbox_key_object(
    key: &JavascriptCryptoKeyMaterial,
) -> Result<Value, SidecarError> {
    let serialized = match key {
        JavascriptCryptoKeyMaterial::Private(key) => JavascriptSerializedSandboxKeyObject {
            kind: String::from("private"),
            pem: Some(
                String::from_utf8(
                    key.private_key_to_pem_pkcs8()
                        .map_err(javascript_crypto_openssl_error)?,
                )
                .map_err(|error| {
                    SidecarError::InvalidState(format!("private key PEM is not utf8: {error}"))
                })?,
            ),
            raw: None,
            asymmetric_key_type: javascript_crypto_pkey_type_name(key.id()),
            asymmetric_key_details: None,
            jwk: None,
        },
        JavascriptCryptoKeyMaterial::Public(key) => JavascriptSerializedSandboxKeyObject {
            kind: String::from("public"),
            pem: Some(
                String::from_utf8(
                    key.public_key_to_pem()
                        .map_err(javascript_crypto_openssl_error)?,
                )
                .map_err(|error| {
                    SidecarError::InvalidState(format!("public key PEM is not utf8: {error}"))
                })?,
            ),
            raw: None,
            asymmetric_key_type: javascript_crypto_pkey_type_name(key.id()),
            asymmetric_key_details: None,
            jwk: None,
        },
        JavascriptCryptoKeyMaterial::Secret(raw) => JavascriptSerializedSandboxKeyObject {
            kind: String::from("secret"),
            pem: None,
            raw: Some(base64::engine::general_purpose::STANDARD.encode(raw)),
            asymmetric_key_type: None,
            asymmetric_key_details: None,
            jwk: None,
        },
    };
    serde_json::to_value(serialized)
        .map_err(|error| SidecarError::InvalidState(format!("serialize key object: {error}")))
}

fn javascript_crypto_pkey_type_name(id: PKeyId) -> Option<String> {
    match id {
        PKeyId::RSA => Some(String::from("rsa")),
        PKeyId::EC => Some(String::from("ec")),
        PKeyId::ED25519 => Some(String::from("ed25519")),
        PKeyId::ED448 => Some(String::from("ed448")),
        PKeyId::X25519 => Some(String::from("x25519")),
        PKeyId::X448 => Some(String::from("x448")),
        PKeyId::DH => Some(String::from("dh")),
        _ => None,
    }
}

fn javascript_crypto_rsa_output_size(
    key: &JavascriptCryptoKeyMaterial,
) -> Result<usize, SidecarError> {
    match key {
        JavascriptCryptoKeyMaterial::Private(key) => key
            .rsa()
            .map(|rsa| rsa.size() as usize)
            .map_err(javascript_crypto_openssl_error),
        JavascriptCryptoKeyMaterial::Public(key) => key
            .rsa()
            .map(|rsa| rsa.size() as usize)
            .map_err(javascript_crypto_openssl_error),
        JavascriptCryptoKeyMaterial::Secret(_) => Err(SidecarError::InvalidState(String::from(
            "RSA operations require an asymmetric key",
        ))),
    }
}

fn javascript_crypto_parse_serialized_options_arg(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<Value>, SidecarError> {
    let Some(raw) = args.get(index).and_then(Value::as_str) else {
        return Ok(None);
    };
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        SidecarError::InvalidState(format!("{label} must be valid JSON: {error}"))
    })?;
    if parsed.get("hasOptions").and_then(Value::as_bool) == Some(true) {
        Ok(parsed.get("options").cloned())
    } else {
        Ok(None)
    }
}

fn javascript_crypto_u32_from_bridge_value(
    value: &Value,
    label: &str,
) -> Result<u32, SidecarError> {
    if let Some(number) = value.as_u64() {
        return u32::try_from(number)
            .map_err(|_| SidecarError::InvalidState(format!("{label} must fit within u32")));
    }
    let bytes = javascript_crypto_decode_bridge_buffer(value, label)?;
    if bytes.len() > 4 {
        return Err(SidecarError::InvalidState(format!(
            "{label} buffer is too large for u32"
        )));
    }
    Ok(bytes
        .into_iter()
        .fold(0_u32, |acc, byte| (acc << 8) | u32::from(byte)))
}

fn javascript_crypto_bignum_from_bridge_value(
    value: &Value,
    label: &str,
) -> Result<BigNum, SidecarError> {
    if let Some(object) = value.as_object() {
        if object.get("__type").and_then(Value::as_str) == Some("bigint") {
            let decimal = object.get("value").and_then(Value::as_str).ok_or_else(|| {
                SidecarError::InvalidState(format!("{label} bigint is missing a value"))
            })?;
            return BigNum::from_dec_str(decimal).map_err(javascript_crypto_openssl_error);
        }
    }
    let bytes = javascript_crypto_decode_bridge_buffer(value, label)?;
    BigNum::from_slice(&bytes).map_err(javascript_crypto_openssl_error)
}

fn javascript_crypto_curve_nid(name: &str) -> Result<Nid, SidecarError> {
    match name {
        "prime256v1" | "P-256" => Ok(Nid::X9_62_PRIME256V1),
        "secp384r1" | "P-384" => Ok(Nid::SECP384R1),
        "secp521r1" | "P-521" => Ok(Nid::SECP521R1),
        "secp256k1" => Ok(Nid::SECP256K1),
        other => Err(SidecarError::InvalidState(format!(
            "unsupported EC curve {other}"
        ))),
    }
}

fn javascript_crypto_named_dh_group(name: &str) -> Result<Dh<Params>, SidecarError> {
    match name {
        "modp2" => Dh::get_1024_160().map_err(javascript_crypto_openssl_error),
        "modp14" | "modp15" | "modp16" | "modp17" | "modp18" => {
            Dh::get_2048_256().map_err(javascript_crypto_openssl_error)
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported Diffie-Hellman group {other}"
        ))),
    }
}

fn javascript_crypto_clone_dh_params(params: &Dh<Params>) -> Result<Dh<Params>, SidecarError> {
    Dh::from_pqg(
        params
            .prime_p()
            .to_owned()
            .map_err(javascript_crypto_openssl_error)?,
        params
            .prime_q()
            .map(|value| value.to_owned().map_err(javascript_crypto_openssl_error))
            .transpose()?,
        params
            .generator()
            .to_owned()
            .map_err(javascript_crypto_openssl_error)?,
    )
    .map_err(javascript_crypto_openssl_error)
}

fn javascript_crypto_build_dh_params(args: &[Value]) -> Result<Dh<Params>, SidecarError> {
    let Some(first) = args.first() else {
        return Err(SidecarError::InvalidState(String::from(
            "Diffie-Hellman session args are required",
        )));
    };
    if let Some(bits) = first.as_u64() {
        let generator = args
            .get(1)
            .map(|value| javascript_crypto_u32_from_bridge_value(value, "Diffie-Hellman generator"))
            .transpose()?
            .unwrap_or(2);
        return Dh::generate_params(bits as u32, generator)
            .map_err(javascript_crypto_openssl_error);
    }
    let prime = javascript_crypto_bignum_from_bridge_value(first, "Diffie-Hellman prime")?;
    let generator = args
        .get(1)
        .map(|value| javascript_crypto_bignum_from_bridge_value(value, "Diffie-Hellman generator"))
        .transpose()?
        .unwrap_or(BigNum::from_u32(2).map_err(javascript_crypto_openssl_error)?);
    Dh::from_pqg(prime, None, generator).map_err(javascript_crypto_openssl_error)
}

fn javascript_crypto_call_dh_session(
    session: &mut ActiveDhSession,
    method: &str,
    args: &[Value],
) -> Result<(Value, bool), SidecarError> {
    match method {
        "verifyError" => Ok((Value::Null, false)),
        "generateKeys" => {
            if session.key_pair.is_none() {
                session.key_pair = Some(
                    javascript_crypto_clone_dh_params(&session.params)?
                        .generate_key()
                        .map_err(javascript_crypto_openssl_error)?,
                );
            }
            let public = session
                .key_pair
                .as_ref()
                .expect("dh key pair")
                .public_key()
                .to_vec();
            Ok((javascript_crypto_bridge_buffer_value(&public), true))
        }
        "computeSecret" => {
            if session.key_pair.is_none() {
                session.key_pair = Some(
                    javascript_crypto_clone_dh_params(&session.params)?
                        .generate_key()
                        .map_err(javascript_crypto_openssl_error)?,
                );
            }
            let peer = javascript_crypto_bignum_from_bridge_value(
                args.first().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "computeSecret requires peer public key",
                    ))
                })?,
                "Diffie-Hellman peer public key",
            )?;
            let secret = session
                .key_pair
                .as_ref()
                .expect("dh key pair")
                .compute_key(&peer)
                .map_err(javascript_crypto_openssl_error)?;
            Ok((javascript_crypto_bridge_buffer_value(&secret), true))
        }
        "getPrime" => Ok((
            javascript_crypto_bridge_buffer_value(&session.params.prime_p().to_vec()),
            true,
        )),
        "getGenerator" => Ok((
            javascript_crypto_bridge_buffer_value(&session.params.generator().to_vec()),
            true,
        )),
        "getPublicKey" => {
            if session.key_pair.is_none() {
                session.key_pair = Some(
                    javascript_crypto_clone_dh_params(&session.params)?
                        .generate_key()
                        .map_err(javascript_crypto_openssl_error)?,
                );
            }
            Ok((
                javascript_crypto_bridge_buffer_value(
                    &session
                        .key_pair
                        .as_ref()
                        .expect("dh key pair")
                        .public_key()
                        .to_vec(),
                ),
                true,
            ))
        }
        "getPrivateKey" => {
            if session.key_pair.is_none() {
                session.key_pair = Some(
                    javascript_crypto_clone_dh_params(&session.params)?
                        .generate_key()
                        .map_err(javascript_crypto_openssl_error)?,
                );
            }
            Ok((
                javascript_crypto_bridge_buffer_value(
                    &session
                        .key_pair
                        .as_ref()
                        .expect("dh key pair")
                        .private_key()
                        .to_vec(),
                ),
                true,
            ))
        }
        other => Err(SidecarError::InvalidState(format!(
            "Unsupported Diffie-Hellman method: {other}"
        ))),
    }
}

fn javascript_crypto_call_ecdh_session(
    session: &mut ActiveEcdhSession,
    method: &str,
    args: &[Value],
) -> Result<(Value, bool), SidecarError> {
    let nid = javascript_crypto_curve_nid(&session.curve)?;
    let group = EcGroup::from_curve_name(nid).map_err(javascript_crypto_openssl_error)?;
    match method {
        "verifyError" => Ok((Value::Null, false)),
        "generateKeys" => {
            if session.key_pair.is_none() {
                session.key_pair =
                    Some(EcKey::generate(&group).map_err(javascript_crypto_openssl_error)?);
            }
            let mut ctx = BigNumContext::new().map_err(javascript_crypto_openssl_error)?;
            let bytes = session
                .key_pair
                .as_ref()
                .expect("ecdh key pair")
                .public_key()
                .to_bytes(&group, PointConversionForm::UNCOMPRESSED, &mut ctx)
                .map_err(javascript_crypto_openssl_error)?;
            Ok((javascript_crypto_bridge_buffer_value(&bytes), true))
        }
        "computeSecret" => {
            if session.key_pair.is_none() {
                session.key_pair =
                    Some(EcKey::generate(&group).map_err(javascript_crypto_openssl_error)?);
            }
            let peer_bytes = javascript_crypto_decode_bridge_buffer(
                args.first().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "computeSecret requires peer public key",
                    ))
                })?,
                "ECDH peer public key",
            )?;
            let mut ctx = BigNumContext::new().map_err(javascript_crypto_openssl_error)?;
            let peer_point = EcPoint::from_bytes(&group, &peer_bytes, &mut ctx)
                .map_err(javascript_crypto_openssl_error)?;
            let peer_key = EcKey::from_public_key(&group, &peer_point)
                .map_err(javascript_crypto_openssl_error)?;
            let private =
                PKey::from_ec_key(session.key_pair.as_ref().expect("ecdh key pair").to_owned())
                    .map_err(javascript_crypto_openssl_error)?;
            let peer = PKey::from_ec_key(peer_key).map_err(javascript_crypto_openssl_error)?;
            let mut deriver = Deriver::new(&private).map_err(javascript_crypto_openssl_error)?;
            deriver
                .set_peer(&peer)
                .map_err(javascript_crypto_openssl_error)?;
            let secret = deriver
                .derive_to_vec()
                .map_err(javascript_crypto_openssl_error)?;
            Ok((javascript_crypto_bridge_buffer_value(&secret), true))
        }
        "getPublicKey" => {
            if session.key_pair.is_none() {
                session.key_pair =
                    Some(EcKey::generate(&group).map_err(javascript_crypto_openssl_error)?);
            }
            let mut ctx = BigNumContext::new().map_err(javascript_crypto_openssl_error)?;
            let bytes = session
                .key_pair
                .as_ref()
                .expect("ecdh key pair")
                .public_key()
                .to_bytes(&group, PointConversionForm::UNCOMPRESSED, &mut ctx)
                .map_err(javascript_crypto_openssl_error)?;
            Ok((javascript_crypto_bridge_buffer_value(&bytes), true))
        }
        "getPrivateKey" => {
            if session.key_pair.is_none() {
                session.key_pair =
                    Some(EcKey::generate(&group).map_err(javascript_crypto_openssl_error)?);
            }
            Ok((
                javascript_crypto_bridge_buffer_value(
                    &session
                        .key_pair
                        .as_ref()
                        .expect("ecdh key pair")
                        .private_key()
                        .to_vec(),
                ),
                true,
            ))
        }
        other => Err(SidecarError::InvalidState(format!(
            "Unsupported Diffie-Hellman method: {other}"
        ))),
    }
}

fn javascript_crypto_serialize_encoded_key_value_public(
    key: &PKey<Public>,
    encoding: Option<&Value>,
) -> Result<Value, SidecarError> {
    if let Some(encoding) = encoding {
        let format = encoding
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("pem");
        return Ok(match format {
            "der" => json!({
                "kind": "buffer",
                "value": base64::engine::general_purpose::STANDARD
                    .encode(key.public_key_to_der().map_err(javascript_crypto_openssl_error)?),
            }),
            _ => json!({
                "kind": "string",
                "value": String::from_utf8(
                    key.public_key_to_pem().map_err(javascript_crypto_openssl_error)?,
                )
                .map_err(|error| SidecarError::InvalidState(format!("public key PEM utf8: {error}")))?,
            }),
        });
    }
    javascript_crypto_serialize_sandbox_key_object(&JavascriptCryptoKeyMaterial::Public(
        key.to_owned(),
    ))
}

fn javascript_crypto_serialize_encoded_key_value_private(
    key: &PKey<Private>,
    encoding: Option<&Value>,
) -> Result<Value, SidecarError> {
    if let Some(encoding) = encoding {
        let format = encoding
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("pem");
        return Ok(match format {
            "der" => json!({
                "kind": "buffer",
                "value": base64::engine::general_purpose::STANDARD
                    .encode(key.private_key_to_der().map_err(javascript_crypto_openssl_error)?),
            }),
            _ => json!({
                "kind": "string",
                "value": String::from_utf8(
                    key.private_key_to_pem_pkcs8().map_err(javascript_crypto_openssl_error)?,
                )
                .map_err(|error| SidecarError::InvalidState(format!("private key PEM utf8: {error}")))?,
            }),
        });
    }
    javascript_crypto_serialize_sandbox_key_object(&JavascriptCryptoKeyMaterial::Private(
        key.to_owned(),
    ))
}

fn javascript_crypto_bridge_buffer_value(bytes: &[u8]) -> Value {
    json!({
        "__type": "buffer",
        "value": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn javascript_crypto_build_cipher_context(
    algorithm: &str,
    key: &[u8],
    iv: Option<&[u8]>,
    decrypt: bool,
    options: Option<&Value>,
) -> Result<Crypter, SidecarError> {
    let cipher = javascript_crypto_cipher_from_name(algorithm)?;
    let mode = if decrypt {
        Mode::Decrypt
    } else {
        Mode::Encrypt
    };
    let mut context =
        Crypter::new(cipher, mode, key, iv).map_err(javascript_crypto_openssl_error)?;
    if let Some(auto_padding) = options
        .and_then(|value| value.get("autoPadding"))
        .and_then(Value::as_bool)
    {
        context.pad(auto_padding);
    }
    if javascript_crypto_is_aead(algorithm) {
        if let Some(aad) = options
            .and_then(|value| value.get("aad"))
            .and_then(Value::as_str)
        {
            context
                .aad_update(
                    &base64::engine::general_purpose::STANDARD
                        .decode(aad)
                        .map_err(|error| {
                            SidecarError::InvalidState(format!(
                                "cipher aad contains invalid base64: {error}"
                            ))
                        })?,
                )
                .map_err(javascript_crypto_openssl_error)?;
        }
        if decrypt {
            if let Some(auth_tag) = options
                .and_then(|value| value.get("authTag"))
                .and_then(Value::as_str)
            {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(auth_tag)
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "cipher authTag contains invalid base64: {error}"
                        ))
                    })?;
                context
                    .set_tag(&decoded)
                    .map_err(javascript_crypto_openssl_error)?;
            }
        }
    }
    Ok(context)
}

fn javascript_crypto_requested_aead_tag_len(
    algorithm: &str,
    options: Option<&Value>,
) -> Result<usize, SidecarError> {
    if !javascript_crypto_is_aead(algorithm) {
        return Ok(0);
    }
    let requested = options
        .and_then(|value| value.get("authTagLength"))
        .and_then(Value::as_u64)
        .unwrap_or(javascript_crypto_aead_tag_len(algorithm) as u64);
    usize::try_from(requested).map_err(|_| {
        SidecarError::InvalidState(String::from("cipher authTagLength must fit within usize"))
    })
}

fn javascript_crypto_cipher_update(
    context: &mut Crypter,
    data: &[u8],
) -> Result<Vec<u8>, SidecarError> {
    let mut output = vec![0_u8; data.len() + 32];
    let written = context
        .update(data, &mut output)
        .map_err(javascript_crypto_openssl_error)?;
    output.truncate(written);
    Ok(output)
}

fn javascript_crypto_cipher_finalize(context: &mut Crypter) -> Result<Vec<u8>, SidecarError> {
    let mut output = vec![0_u8; 32];
    let written = context
        .finalize(&mut output)
        .map_err(javascript_crypto_openssl_error)?;
    output.truncate(written);
    Ok(output)
}

fn javascript_crypto_cipher_from_name(name: &str) -> Result<Cipher, SidecarError> {
    match name.to_ascii_lowercase().as_str() {
        "aes-128-cbc" => Ok(Cipher::aes_128_cbc()),
        "aes-192-cbc" => Ok(Cipher::aes_192_cbc()),
        "aes-256-cbc" => Ok(Cipher::aes_256_cbc()),
        "aes-128-ctr" => Ok(Cipher::aes_128_ctr()),
        "aes-192-ctr" => Ok(Cipher::aes_192_ctr()),
        "aes-256-ctr" => Ok(Cipher::aes_256_ctr()),
        "aes-128-gcm" => Ok(Cipher::aes_128_gcm()),
        "aes-192-gcm" => Ok(Cipher::aes_192_gcm()),
        "aes-256-gcm" => Ok(Cipher::aes_256_gcm()),
        other => Err(SidecarError::InvalidState(format!(
            "unsupported crypto cipher algorithm {other}"
        ))),
    }
}

fn javascript_crypto_is_aead(algorithm: &str) -> bool {
    algorithm.to_ascii_lowercase().ends_with("-gcm")
}

fn javascript_crypto_aead_tag_len(_algorithm: &str) -> usize {
    16
}

fn javascript_crypto_openssl_error(error: openssl::error::ErrorStack) -> SidecarError {
    SidecarError::Execution(format!("crypto operation failed: {error}"))
}

fn service_javascript_kernel_stdin_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let max_bytes =
        javascript_sync_rpc_arg_u64_optional(&request.args, 0, "__kernel_stdin_read max bytes")?
            .map(|value| value.clamp(1, DEFAULT_KERNEL_STDIN_READ_MAX_BYTES as u64) as usize)
            .unwrap_or(DEFAULT_KERNEL_STDIN_READ_MAX_BYTES);
    let timeout_ms =
        javascript_sync_rpc_arg_u64_optional(&request.args, 1, "__kernel_stdin_read timeout ms")?
            .unwrap_or(DEFAULT_KERNEL_STDIN_READ_TIMEOUT_MS);

    match kernel
        .fd_read_with_timeout_result(
            EXECUTION_DRIVER_NAME,
            process.kernel_pid,
            0,
            max_bytes,
            Some(Duration::from_millis(timeout_ms)),
        )
        .map_err(kernel_error)
    {
        Ok(Some(chunk)) if !chunk.is_empty() => Ok(json!({
            "dataBase64": base64::engine::general_purpose::STANDARD.encode(chunk),
        })),
        Ok(Some(_)) => Ok(Value::Null),
        Ok(None) => Ok(json!({
            "done": true,
        })),
        Err(SidecarError::Kernel(error)) if error.starts_with("EAGAIN:") => Ok(Value::Null),
        Err(error) => Err(error),
    }
}

fn service_javascript_pty_set_raw_mode_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let enabled = javascript_sync_rpc_arg_bool(&request.args, 0, "__pty_set_raw_mode enabled")?;
    kernel
        .pty_set_discipline(
            EXECUTION_DRIVER_NAME,
            process.kernel_pid,
            0,
            LineDisciplineConfig {
                canonical: Some(!enabled),
                echo: Some(!enabled),
                isig: Some(!enabled),
            },
        )
        .map_err(kernel_error)?;
    Ok(Value::Null)
}

fn install_kernel_stdin_pipe(kernel: &mut SidecarKernel, pid: u32) -> Result<u32, SidecarError> {
    let (read_fd, write_fd) = kernel
        .open_pipe(EXECUTION_DRIVER_NAME, pid)
        .map_err(kernel_error)?;
    kernel
        .fd_dup2(EXECUTION_DRIVER_NAME, pid, read_fd, 0)
        .map_err(kernel_error)?;
    kernel
        .fd_close(EXECUTION_DRIVER_NAME, pid, read_fd)
        .map_err(kernel_error)?;
    Ok(write_fd)
}

fn write_kernel_process_stdin(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    chunk: &[u8],
) -> Result<(), SidecarError> {
    let Some(writer_fd) = process.kernel_stdin_writer_fd else {
        return Ok(());
    };
    kernel
        .fd_write(EXECUTION_DRIVER_NAME, process.kernel_pid, writer_fd, chunk)
        .map(|_| ())
        .map_err(kernel_error)
}

fn close_kernel_process_stdin(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
) -> Result<(), SidecarError> {
    let Some(writer_fd) = process.kernel_stdin_writer_fd.take() else {
        return Ok(());
    };
    kernel
        .fd_close(EXECUTION_DRIVER_NAME, process.kernel_pid, writer_fd)
        .map_err(kernel_error)
}

fn service_javascript_fetch_sync_rpc<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let resource = javascript_sync_rpc_arg_str(&request.args, 0, "net.fetch resource")?;
    if let Some(response) = build_data_url_fetch_response(resource)? {
        return Ok(response);
    }

    Url::parse(resource)
        .map_err(|error| SidecarError::Execution(format!("ERR_INVALID_URL: {error}")))?;

    if let Err(error) = bridge.require_network_access(vm_id, NetworkOperation::Fetch, resource) {
        return Err(match error {
            SidecarError::Execution(_) => SidecarError::Execution(format!(
                "ERR_ACCESS_DENIED: blocked outbound network access to {resource}"
            )),
            other => other,
        });
    }

    Err(SidecarError::Execution(format!(
        "ERR_ACCESS_DENIED: blocked outbound network access to {resource}"
    )))
}

fn build_data_url_fetch_response(resource: &str) -> Result<Option<Value>, SidecarError> {
    let Some(payload) = resource.strip_prefix("data:") else {
        return Ok(None);
    };
    let (metadata, body) = payload.split_once(',').ok_or_else(|| {
        SidecarError::Execution(String::from(
            "ERR_INVALID_URL: malformed data URL missing comma separator",
        ))
    })?;
    let metadata = metadata.trim();
    let is_base64 = metadata
        .split(';')
        .any(|segment| segment.eq_ignore_ascii_case("base64"));
    let content_type = metadata
        .split(';')
        .find(|segment| !segment.is_empty() && !segment.eq_ignore_ascii_case("base64"))
        .unwrap_or("text/plain;charset=US-ASCII");

    let response = if is_base64 {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|error| {
                SidecarError::Execution(format!("ERR_INVALID_URL: invalid data URL body: {error}"))
            })?;
        json!({
            "ok": true,
            "status": 200,
            "statusText": "OK",
            "headers": {
                "content-type": content_type,
                "x-body-encoding": "base64",
            },
            "body": base64::engine::general_purpose::STANDARD.encode(bytes),
            "url": resource,
            "redirected": false,
        })
    } else {
        json!({
            "ok": true,
            "status": 200,
            "statusText": "OK",
            "headers": {
                "content-type": content_type,
            },
            "body": body,
            "url": resource,
            "redirected": false,
        })
    };

    serde_json::to_string(&response)
        .map(Value::String)
        .map(Some)
        .map_err(|error| SidecarError::Execution(format!("ERR_AGENT_OS_NODE_SYNC_RPC: {error}")))
}

fn parse_http_request_options(
    request: &JavascriptSyncRpcRequest,
) -> Result<(Url, JavascriptHttpRequestOptions, HttpHeaderCollection), SidecarError> {
    let resource = javascript_sync_rpc_arg_str(&request.args, 0, "net.http_request resource")?;
    let url = Url::parse(resource)
        .map_err(|error| SidecarError::Execution(format!("ERR_INVALID_URL: {error}")))?;
    let options_json =
        javascript_sync_rpc_arg_str(&request.args, 1, "net.http_request options payload")?;
    let options: JavascriptHttpRequestOptions =
        serde_json::from_str(options_json).map_err(|error| {
            SidecarError::InvalidState(format!(
                "net.http_request options must be valid JSON: {error}"
            ))
        })?;
    let headers = parse_http_header_collection(&options.headers, "net.http_request headers")?;
    Ok((url, options, headers))
}

fn parse_http_header_collection(
    headers: &BTreeMap<String, Value>,
    label: &str,
) -> Result<HttpHeaderCollection, SidecarError> {
    let mut normalized = BTreeMap::<String, Vec<String>>::new();
    let mut raw_pairs = Vec::new();

    for (raw_name, value) in headers {
        let normalized_name = raw_name.to_ascii_lowercase();
        let values = match value {
            Value::String(text) => vec![text.clone()],
            Value::Array(values) => values
                .iter()
                .map(|entry| {
                    entry.as_str().map(str::to_owned).ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "{label} header {raw_name} must contain only strings"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            other => {
                return Err(SidecarError::InvalidState(format!(
                    "{label} header {raw_name} must be a string or string array, received {other}"
                )));
            }
        };
        raw_pairs.extend(
            values
                .iter()
                .cloned()
                .map(|entry| (raw_name.clone(), entry)),
        );
        normalized
            .entry(normalized_name)
            .or_default()
            .extend(values.into_iter());
    }

    Ok(HttpHeaderCollection {
        normalized,
        raw_pairs,
    })
}

fn http_headers_json(headers: &HttpHeaderCollection) -> Value {
    let map = headers
        .normalized
        .iter()
        .map(|(name, values)| {
            let value = if values.len() == 1 {
                Value::String(values[0].clone())
            } else {
                Value::Array(values.iter().cloned().map(Value::String).collect())
            };
            (name.clone(), value)
        })
        .collect::<Map<String, Value>>();
    Value::Object(map)
}

fn http_raw_headers_json(headers: &HttpHeaderCollection) -> Value {
    Value::Array(
        headers
            .raw_pairs
            .iter()
            .flat_map(|(name, value)| [Value::String(name.clone()), Value::String(value.clone())])
            .collect(),
    )
}

fn is_loopback_request_host(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    matches!(bare, "localhost" | "127.0.0.1" | "::1")
}

fn serialize_http_loopback_request(
    url: &Url,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
) -> Result<String, SidecarError> {
    let body_base64 = options
        .body
        .as_ref()
        .map(|body| base64::engine::general_purpose::STANDARD.encode(body.as_bytes()));
    serde_json::to_string(&json!({
        "method": options.method.clone().unwrap_or_else(|| String::from("GET")),
        "url": http_request_target(url),
        "headers": http_headers_json(headers),
        "rawHeaders": http_raw_headers_json(headers),
        "bodyBase64": body_base64,
    }))
    .map_err(|error| SidecarError::Execution(format!("ERR_AGENT_OS_NODE_SYNC_RPC: {error}")))
}

fn http_request_target(url: &Url) -> String {
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    format!(
        "{path}{}",
        url.query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default()
    )
}

fn outbound_http_response_json(url: &Url, response: ureq::Response) -> Result<Value, SidecarError> {
    let status = response.status();
    let status_text = response.status_text().to_owned();
    let mut header_pairs = Vec::new();
    let mut raw_headers = Vec::new();
    for raw_name in response.headers_names() {
        for value in response.all(&raw_name) {
            header_pairs.push(json!([raw_name.to_ascii_lowercase(), value]));
            raw_headers.push(Value::String(raw_name.clone()));
            raw_headers.push(Value::String(value.to_owned()));
        }
    }
    let mut reader = response.into_reader();
    let mut body = Vec::new();
    reader.read_to_end(&mut body).map_err(|error| {
        SidecarError::Execution(format!("failed to read HTTP response: {error}"))
    })?;
    serde_json::to_string(&json!({
        "status": status,
        "statusText": status_text,
        "headers": header_pairs,
        "rawHeaders": raw_headers,
        "body": base64::engine::general_purpose::STANDARD.encode(body),
        "bodyEncoding": "base64",
        "url": url.as_str(),
    }))
    .map(Value::String)
    .map_err(|error| SidecarError::Execution(format!("ERR_AGENT_OS_NODE_SYNC_RPC: {error}")))
}

fn issue_outbound_http_request(
    url: &Url,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
) -> Result<Value, SidecarError> {
    let method = options.method.as_deref().unwrap_or("GET");
    let mut request = ureq::request(method, url.as_str());
    for (name, values) in &headers.normalized {
        let header_value = values.join(", ");
        request = request.set(name, &header_value);
    }
    let response = match options.body.as_deref() {
        Some(body) => request.send_string(body),
        None => request.call(),
    };

    match response {
        Ok(response) => outbound_http_response_json(url, response),
        Err(ureq::Error::Status(_, response)) => outbound_http_response_json(url, response),
        Err(ureq::Error::Transport(error)) => Err(SidecarError::Execution(format!(
            "ERR_HTTP_REQUEST_FAILED: {error}"
        ))),
    }
}

fn wait_for_loopback_http_response<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    socket_paths: &JavascriptSocketPathContext,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    resource_limits: &ResourceLimits,
    request_key: (u64, u64),
) -> Result<String, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let deadline = Instant::now() + HTTP_LOOPBACK_REQUEST_TIMEOUT;
    loop {
        if let Some(response) = process
            .pending_http_requests
            .get(&request_key)
            .and_then(|response| response.clone())
        {
            process.pending_http_requests.remove(&request_key);
            return Ok(response);
        }

        if Instant::now() >= deadline {
            process.pending_http_requests.remove(&request_key);
            return Err(SidecarError::Execution(String::from(
                "HTTP loopback request timed out waiting for net.http_respond",
            )));
        }

        let Some(event) = process
            .execution
            .poll_event_blocking(Duration::from_millis(10))
            .map_err(|error| SidecarError::Execution(error.to_string()))?
        else {
            continue;
        };

        match event {
            ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
                let network_counts = process.network_resource_counts();
                let response = service_javascript_sync_rpc(
                    bridge,
                    vm_id,
                    dns,
                    socket_paths,
                    kernel,
                    process,
                    &request,
                    resource_limits,
                    network_counts,
                );
                match response {
                    Ok(result) => process
                        .execution
                        .respond_javascript_sync_rpc_success(request.id, result)
                        .or_else(ignore_stale_javascript_sync_rpc_response)?,
                    Err(error) => process
                        .execution
                        .respond_javascript_sync_rpc_error(
                            request.id,
                            "ERR_AGENT_OS_NODE_SYNC_RPC",
                            error.to_string(),
                        )
                        .or_else(ignore_stale_javascript_sync_rpc_response)?,
                }
            }
            ActiveExecutionEvent::Exited(code) => {
                process.pending_http_requests.remove(&request_key);
                return Err(SidecarError::Execution(format!(
                    "HTTP loopback server exited before responding (exit code {code})"
                )));
            }
            ActiveExecutionEvent::Stdout(_)
            | ActiveExecutionEvent::Stderr(_)
            | ActiveExecutionEvent::PythonVfsRpcRequest(_)
            | ActiveExecutionEvent::SignalState { .. } => {}
        }
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
        "dgram.address" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "dgram.address socket id")?;
            let socket = process.udp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            let local_addr = socket.local_addr().ok_or_else(|| {
                SidecarError::Execution(String::from("EBADF: bad file descriptor"))
            })?;
            javascript_net_json_string(
                json!({
                    "address": local_addr.ip().to_string(),
                    "port": local_addr.port(),
                    "family": socket_addr_family(&local_addr),
                }),
                "dgram.address",
            )
        }
        "dgram.setBufferSize" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "dgram.setBufferSize socket id")?;
            let which =
                javascript_sync_rpc_arg_str(&request.args, 1, "dgram.setBufferSize buffer kind")?;
            let size = javascript_sync_rpc_arg_u64(&request.args, 2, "dgram.setBufferSize size")?;
            let size = usize::try_from(size).map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "dgram.setBufferSize size must fit within usize",
                ))
            })?;
            let socket = process.udp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            socket.set_buffer_size(which, size)?;
            Ok(Value::Null)
        }
        "dgram.getBufferSize" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "dgram.getBufferSize socket id")?;
            let which =
                javascript_sync_rpc_arg_str(&request.args, 1, "dgram.getBufferSize buffer kind")?;
            let socket = process.udp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            let size = socket.get_buffer_size(which)?;
            Ok(json!(size))
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript dgram sync RPC method {other}"
        ))),
    }
}

#[derive(Debug)]
struct ClientHttp2StreamState {
    send_stream: Option<h2::SendStream<Bytes>>,
}

#[derive(Debug)]
struct ServerHttp2StreamState {
    send_response: Option<ServerHttp2Responder>,
    send_stream: Option<h2::SendStream<Bytes>>,
}

#[derive(Debug)]
enum ServerHttp2Responder {
    Regular(server::SendResponse<Bytes>),
    Pushed(server::SendPushedResponse<Bytes>),
}

const HTTP2_DEFAULT_WINDOW_SIZE: u32 = 65_535;
const HTTP2_POLL_DELAY: Duration = Duration::from_millis(10);

fn http2_runtime_snapshot() -> Http2RuntimeSnapshot {
    Http2RuntimeSnapshot {
        effective_local_window_size: HTTP2_DEFAULT_WINDOW_SIZE,
        local_window_size: HTTP2_DEFAULT_WINDOW_SIZE,
        remote_window_size: HTTP2_DEFAULT_WINDOW_SIZE,
        next_stream_id: 1,
        outbound_queue_size: 1,
        deflate_dynamic_table_size: 0,
        inflate_dynamic_table_size: 0,
    }
}

fn http2_snapshot_json(snapshot: &Http2SessionSnapshot) -> Result<String, SidecarError> {
    serde_json::to_string(snapshot)
        .map_err(|error| SidecarError::Execution(format!("ERR_AGENT_OS_NODE_SYNC_RPC: {error}")))
}

fn http2_event_value(event: &Http2BridgeEvent) -> Result<Value, SidecarError> {
    serde_json::to_string(event)
        .map(Value::String)
        .map_err(|error| SidecarError::Execution(format!("ERR_AGENT_OS_NODE_SYNC_RPC: {error}")))
}

fn push_http2_server_event(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    server_id: u64,
    event: Http2BridgeEvent,
) {
    if let Ok(mut state) = shared.lock() {
        state
            .server_events
            .entry(server_id)
            .or_default()
            .push_back(event);
    }
}

fn push_http2_session_event(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    session_id: u64,
    event: Http2BridgeEvent,
) {
    if let Ok(mut state) = shared.lock() {
        state
            .session_events
            .entry(session_id)
            .or_default()
            .push_back(event);
    }
}

fn pop_http2_event(
    queue: &mut BTreeMap<u64, VecDeque<Http2BridgeEvent>>,
    id: u64,
) -> Option<Http2BridgeEvent> {
    queue.get_mut(&id).and_then(VecDeque::pop_front)
}

fn wait_for_http2_event(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    id: u64,
    is_server: bool,
    wait_ms: u64,
) -> Option<Http2BridgeEvent> {
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        if let Ok(mut state) = shared.lock() {
            let queue = if is_server {
                &mut state.server_events
            } else {
                &mut state.session_events
            };
            if let Some(event) = pop_http2_event(queue, id) {
                return Some(event);
            }
        }
        if wait_ms == 0 || Instant::now() >= deadline {
            return None;
        }
        thread::sleep(HTTP2_POLL_DELAY);
    }
}

fn next_http2_session_id(shared: &mut crate::state::Http2SharedState) -> u64 {
    shared.next_session_id += 1;
    shared.next_session_id
}

fn next_http2_stream_id(shared: &mut crate::state::Http2SharedState) -> u64 {
    shared.next_stream_id += 1;
    shared.next_stream_id
}

fn http2_reason(code: Option<u32>) -> Reason {
    code.unwrap_or(Reason::NO_ERROR.into()).into()
}

fn http2_error_payload(message: impl Into<String>) -> String {
    serde_json::to_string(&json!({
        "name": "Error",
        "code": "ERR_HTTP2_ERROR",
        "message": message.into(),
    }))
    .unwrap_or_else(|_| {
        String::from(
            "{\"name\":\"Error\",\"code\":\"ERR_HTTP2_ERROR\",\"message\":\"HTTP/2 bridge error\"}",
        )
    })
}

fn http2_socket_snapshot(local_addr: SocketAddr, remote_addr: SocketAddr) -> Http2SocketSnapshot {
    Http2SocketSnapshot {
        encrypted: false,
        allow_half_open: false,
        local_address: Some(local_addr.ip().to_string()),
        local_port: Some(local_addr.port()),
        local_family: Some(socket_addr_family(&local_addr).to_string()),
        remote_address: Some(remote_addr.ip().to_string()),
        remote_port: Some(remote_addr.port()),
        remote_family: Some(socket_addr_family(&remote_addr).to_string()),
        servername: None,
        alpn_protocol: Some(String::from("h2c")),
    }
}

fn http2_settings_from_value(settings: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    settings.clone()
}

fn parse_http2_headers_json(
    headers_json: &str,
    label: &str,
) -> Result<BTreeMap<String, Value>, SidecarError> {
    serde_json::from_str::<BTreeMap<String, Value>>(headers_json)
        .map_err(|error| SidecarError::InvalidState(format!("{label} must be valid JSON: {error}")))
}

fn apply_http2_header_values(
    header_map: &mut HeaderMap,
    name: &str,
    value: &Value,
) -> Result<(), SidecarError> {
    let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
        SidecarError::InvalidState(format!("invalid HTTP/2 header name {name:?}: {error}"))
    })?;
    match value {
        Value::Array(values) => {
            for value in values {
                apply_http2_header_values(header_map, name, value)?;
            }
        }
        Value::String(text) => {
            let value = HeaderValue::from_str(text).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid HTTP/2 header value for {name}: {error}"
                ))
            })?;
            header_map.append(header_name.clone(), value);
        }
        Value::Number(number) => {
            let value = HeaderValue::from_str(&number.to_string()).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid HTTP/2 numeric header value for {name}: {error}"
                ))
            })?;
            header_map.append(header_name.clone(), value);
        }
        Value::Bool(boolean) => {
            let value = HeaderValue::from_str(if *boolean { "true" } else { "false" }).map_err(
                |error| {
                    SidecarError::InvalidState(format!(
                        "invalid HTTP/2 boolean header value for {name}: {error}"
                    ))
                },
            )?;
            header_map.append(header_name.clone(), value);
        }
        Value::Null => {}
        Value::Object(_) => {
            return Err(SidecarError::InvalidState(format!(
                "unsupported HTTP/2 header object value for {name}"
            )));
        }
    }
    Ok(())
}

fn build_http2_request(headers_json: &str) -> Result<Request<()>, SidecarError> {
    let headers = parse_http2_headers_json(headers_json, "HTTP/2 request headers")?;
    let method = headers
        .get(":method")
        .and_then(Value::as_str)
        .unwrap_or("GET");
    let path = headers.get(":path").and_then(Value::as_str).unwrap_or("/");
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).map_err(|error| {
            SidecarError::InvalidState(format!("invalid HTTP/2 method {method:?}: {error}"))
        })?)
        .uri(path.parse::<Uri>().map_err(|error| {
            SidecarError::InvalidState(format!("invalid HTTP/2 path {path:?}: {error}"))
        })?);
    {
        let header_map = builder.headers_mut().expect("request header map");
        for (name, value) in &headers {
            if name.starts_with(':') {
                continue;
            }
            apply_http2_header_values(header_map, name, value)?;
        }
    }
    builder
        .body(())
        .map_err(|error| SidecarError::InvalidState(format!("invalid HTTP/2 request: {error}")))
}

fn build_http2_response(headers_json: &str) -> Result<Response<()>, SidecarError> {
    let headers = parse_http2_headers_json(headers_json, "HTTP/2 response headers")?;
    let status = headers
        .get(":status")
        .and_then(Value::as_u64)
        .or_else(|| {
            headers
                .get(":status")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<u16>().ok().map(u64::from))
        })
        .unwrap_or(200);
    let mut builder = Response::builder().status(status as u16);
    {
        let header_map = builder.headers_mut().expect("response header map");
        for (name, value) in &headers {
            if name.starts_with(':') {
                continue;
            }
            apply_http2_header_values(header_map, name, value)?;
        }
    }
    builder.body(()).map_err(|error| {
        SidecarError::InvalidState(format!("invalid HTTP/2 response headers: {error}"))
    })
}

fn serialize_http2_headers_map(
    pseudo: BTreeMap<String, Value>,
    headers: &HeaderMap,
) -> Result<String, SidecarError> {
    let mut serialized = pseudo;
    for (name, value) in headers {
        let name = name.as_str().to_string();
        let value = Value::String(
            value
                .to_str()
                .map_err(|error| {
                    SidecarError::Execution(format!("invalid HTTP/2 header value: {error}"))
                })?
                .to_owned(),
        );
        match serialized.get_mut(&name) {
            Some(Value::Array(values)) => values.push(value),
            Some(existing) => {
                let first = existing.clone();
                *existing = Value::Array(vec![first, value]);
            }
            None => {
                serialized.insert(name, value);
            }
        }
    }
    serde_json::to_string(&serialized)
        .map_err(|error| SidecarError::Execution(format!("ERR_AGENT_OS_NODE_SYNC_RPC: {error}")))
}

fn serialize_http2_request_headers(
    request: &Request<h2::RecvStream>,
) -> Result<String, SidecarError> {
    let mut pseudo = BTreeMap::new();
    pseudo.insert(
        String::from(":method"),
        Value::String(request.method().as_str().to_string()),
    );
    pseudo.insert(
        String::from(":path"),
        Value::String(
            request
                .uri()
                .path_and_query()
                .map(|value| value.as_str().to_string())
                .unwrap_or_else(|| String::from("/")),
        ),
    );
    serialize_http2_headers_map(pseudo, request.headers())
}

fn serialize_http2_response_headers(
    response: &Response<h2::RecvStream>,
) -> Result<String, SidecarError> {
    let mut pseudo = BTreeMap::new();
    pseudo.insert(
        String::from(":status"),
        Value::Number(serde_json::Number::from(response.status().as_u16())),
    );
    serialize_http2_headers_map(pseudo, response.headers())
}

fn remove_http2_session_resources(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    session_id: u64,
) {
    if let Ok(mut state) = shared.lock() {
        state.sessions.remove(&session_id);
        state.session_events.remove(&session_id);
        let stream_ids = state
            .streams
            .iter()
            .filter_map(|(stream_id, stream)| {
                (stream.session_id == session_id).then_some(*stream_id)
            })
            .collect::<Vec<_>>();
        for stream_id in stream_ids {
            state.streams.remove(&stream_id);
        }
    }
}

fn spawn_http2_client_session(
    shared: Arc<Mutex<crate::state::Http2SharedState>>,
    session_id: u64,
    remote_addr: SocketAddr,
    snapshot: Arc<Mutex<Http2SessionSnapshot>>,
    mut command_rx: UnboundedReceiver<Http2SessionCommand>,
) {
    thread::spawn(move || {
        let runtime = match TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                push_http2_session_event(
                    &shared,
                    session_id,
                    Http2BridgeEvent {
                        kind: String::from("sessionError"),
                        id: session_id,
                        data: Some(http2_error_payload(error.to_string())),
                        ..Http2BridgeEvent::default()
                    },
                );
                remove_http2_session_resources(&shared, session_id);
                return;
            }
        };

        runtime.block_on(async move {
            let stream = match tokio::net::TcpStream::connect(remote_addr).await {
                Ok(stream) => stream,
                Err(error) => {
                    push_http2_session_event(
                        &shared,
                        session_id,
                        Http2BridgeEvent {
                            kind: String::from("sessionError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };

            let local_addr = match stream.local_addr() {
                Ok(addr) => addr,
                Err(error) => {
                    push_http2_session_event(
                        &shared,
                        session_id,
                        Http2BridgeEvent {
                            kind: String::from("sessionError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };

            {
                let mut snapshot_guard = snapshot.lock().expect("http2 snapshot lock");
                snapshot_guard.socket = http2_socket_snapshot(local_addr, remote_addr);
                snapshot_guard.state = http2_runtime_snapshot();
            }
            if let Ok(snapshot_json) =
                http2_snapshot_json(&snapshot.lock().expect("http2 snapshot lock").clone())
            {
                push_http2_session_event(
                    &shared,
                    session_id,
                    Http2BridgeEvent {
                        kind: String::from("sessionConnect"),
                        id: session_id,
                        data: Some(snapshot_json),
                        ..Http2BridgeEvent::default()
                    },
                );
            }

            let (mut sender, connection) = match client::handshake(stream).await {
                Ok(parts) => parts,
                Err(error) => {
                    push_http2_session_event(
                        &shared,
                        session_id,
                        Http2BridgeEvent {
                            kind: String::from("sessionError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };

            let (status_tx, mut status_rx) = unbounded_channel::<Result<(), String>>();
            tokio::spawn(async move {
                let _ = status_tx.send(connection.await.map_err(|error| error.to_string()));
            });

            let streams: Arc<Mutex<BTreeMap<u64, ClientHttp2StreamState>>> =
                Arc::new(Mutex::new(BTreeMap::new()));

            loop {
                tokio::select! {
                    Some(result) = status_rx.recv() => {
                        if let Err(message) = result {
                            push_http2_session_event(
                                &shared,
                                session_id,
                                Http2BridgeEvent {
                                    kind: String::from("sessionError"),
                                    id: session_id,
                                    data: Some(http2_error_payload(message)),
                                    ..Http2BridgeEvent::default()
                                },
                            );
                        }
                        push_http2_session_event(
                            &shared,
                            session_id,
                            Http2BridgeEvent {
                                kind: String::from("sessionClose"),
                                id: session_id,
                                ..Http2BridgeEvent::default()
                            },
                        );
                        remove_http2_session_resources(&shared, session_id);
                        break;
                    }
                    Some(command) = command_rx.recv() => {
                        match command {
                            Http2SessionCommand::Request { headers_json, options_json, respond_to } => {
                                let request = match build_http2_request(&headers_json) {
                                    Ok(request) => request,
                                    Err(error) => {
                                        let _ = respond_to.send(Err(error.to_string()));
                                        continue;
                                    }
                                };
                                let options: JavascriptHttp2RequestOptions =
                                    serde_json::from_str(&options_json).unwrap_or_default();
                                let stream_id = {
                                    let mut state = shared.lock().expect("http2 shared state");
                                    let stream_id = next_http2_stream_id(&mut state);
                                    state.streams.insert(
                                        stream_id,
                                        ActiveHttp2Stream {
                                            session_id,
                                            direction: Http2StreamDirection::Client,
                                            paused: Arc::new(AtomicBool::new(false)),
                                        },
                                    );
                                    stream_id
                                };
                                match sender.send_request(request, options.end_stream) {
                                    Ok((response_future, send_stream)) => {
                                        if !options.end_stream {
                                            streams
                                                .lock()
                                                .expect("http2 client streams")
                                                .insert(stream_id, ClientHttp2StreamState { send_stream: Some(send_stream) });
                                        }
                                        let shared_clone = Arc::clone(&shared);
                                        let snapshot_clone = Arc::clone(&snapshot);
                                        tokio::spawn(async move {
                                            match response_future.await {
                                                Ok(response) => {
                                                    if let Ok(headers_json) = serialize_http2_response_headers(&response) {
                                                        push_http2_session_event(
                                                            &shared_clone,
                                                            session_id,
                                                            Http2BridgeEvent {
                                                                kind: String::from("clientResponseHeaders"),
                                                                id: stream_id,
                                                                data: Some(headers_json),
                                                                ..Http2BridgeEvent::default()
                                                            },
                                                        );
                                                    }
                                                    let mut body = response.into_body();
                                                    while let Some(chunk) = body.data().await {
                                                        match chunk {
                                                            Ok(bytes) => {
                                                                let paused = {
                                                                    let state = shared_clone.lock().expect("http2 shared state");
                                                                    state.streams.get(&stream_id).map(|stream| Arc::clone(&stream.paused))
                                                                };
                                                                if let Some(paused) = paused {
                                                                    while paused.load(Ordering::SeqCst) {
                                                                        tokio::time::sleep(HTTP2_POLL_DELAY).await;
                                                                    }
                                                                }
                                                                let _ = body.flow_control().release_capacity(bytes.len());
                                                                push_http2_session_event(
                                                                    &shared_clone,
                                                                    session_id,
                                                                    Http2BridgeEvent {
                                                                        kind: String::from("clientData"),
                                                                        id: stream_id,
                                                                        data: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
                                                                        ..Http2BridgeEvent::default()
                                                                    },
                                                                );
                                                            }
                                                            Err(error) => {
                                                                push_http2_session_event(
                                                                    &shared_clone,
                                                                    session_id,
                                                                    Http2BridgeEvent {
                                                                        kind: String::from("clientError"),
                                                                        id: stream_id,
                                                                        data: Some(http2_error_payload(error.to_string())),
                                                                        ..Http2BridgeEvent::default()
                                                                    },
                                                                );
                                                                break;
                                                            }
                                                        }
                                                    }
                                                    {
                                                        let mut snapshot = snapshot_clone.lock().expect("http2 snapshot lock");
                                                        snapshot.state.next_stream_id =
                                                            snapshot.state.next_stream_id.saturating_add(2);
                                                    }
                                                    push_http2_session_event(
                                                        &shared_clone,
                                                        session_id,
                                                        Http2BridgeEvent {
                                                            kind: String::from("clientEnd"),
                                                            id: stream_id,
                                                            ..Http2BridgeEvent::default()
                                                        },
                                                    );
                                                    push_http2_session_event(
                                                        &shared_clone,
                                                        session_id,
                                                        Http2BridgeEvent {
                                                            kind: String::from("clientClose"),
                                                            id: stream_id,
                                                            extra_number: Some(0),
                                                            ..Http2BridgeEvent::default()
                                                        },
                                                    );
                                                    if let Ok(mut state) = shared_clone.lock() {
                                                        state.streams.remove(&stream_id);
                                                    }
                                                }
                                                Err(error) => {
                                                    push_http2_session_event(
                                                        &shared_clone,
                                                        session_id,
                                                        Http2BridgeEvent {
                                                            kind: String::from("clientError"),
                                                            id: stream_id,
                                                            data: Some(http2_error_payload(error.to_string())),
                                                            ..Http2BridgeEvent::default()
                                                        },
                                                    );
                                                    push_http2_session_event(
                                                        &shared_clone,
                                                        session_id,
                                                        Http2BridgeEvent {
                                                            kind: String::from("clientClose"),
                                                            id: stream_id,
                                                            extra_number: Some(u32::from(Reason::INTERNAL_ERROR) as u64),
                                                            ..Http2BridgeEvent::default()
                                                        },
                                                    );
                                                    if let Ok(mut state) = shared_clone.lock() {
                                                        state.streams.remove(&stream_id);
                                                    }
                                                }
                                            }
                                        });
                                        let _ = respond_to.send(Ok(json!(stream_id)));
                                    }
                                    Err(error) => {
                                        if let Ok(mut state) = shared.lock() {
                                            state.streams.remove(&stream_id);
                                        }
                                        let _ = respond_to.send(Err(error.to_string()));
                                    }
                                }
                            }
                            Http2SessionCommand::Settings { settings_json, respond_to } => {
                                let settings = serde_json::from_str::<BTreeMap<String, Value>>(&settings_json)
                                    .unwrap_or_default();
                                {
                                    let mut snapshot = snapshot.lock().expect("http2 snapshot lock");
                                    snapshot.local_settings = http2_settings_from_value(&settings);
                                }
                                if let Ok(headers_json) = serde_json::to_string(&settings) {
                                    push_http2_session_event(
                                        &shared,
                                        session_id,
                                        Http2BridgeEvent {
                                            kind: String::from("sessionLocalSettings"),
                                            id: session_id,
                                            data: Some(headers_json.clone()),
                                            ..Http2BridgeEvent::default()
                                        },
                                    );
                                    push_http2_session_event(
                                        &shared,
                                        session_id,
                                        Http2BridgeEvent {
                                            kind: String::from("sessionSettingsAck"),
                                            id: session_id,
                                            ..Http2BridgeEvent::default()
                                        },
                                    );
                                }
                                let _ = respond_to.send(Ok(Value::Null));
                            }
                            Http2SessionCommand::SetLocalWindowSize { size, respond_to } => {
                                {
                                    let mut snapshot = snapshot.lock().expect("http2 snapshot lock");
                                    snapshot.state.local_window_size = size;
                                    snapshot.state.effective_local_window_size = size;
                                }
                                let value = snapshot
                                    .lock()
                                    .ok()
                                    .and_then(|snapshot| http2_snapshot_json(&snapshot.clone()).ok())
                                    .map(Value::String)
                                    .unwrap_or(Value::Null);
                                let _ = respond_to.send(Ok(value));
                            }
                            Http2SessionCommand::Goaway { error_code, last_stream_id, opaque_data, respond_to } => {
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionGoaway"),
                                        id: session_id,
                                        data: opaque_data.map(|value| {
                                            base64::engine::general_purpose::STANDARD.encode(value)
                                        }),
                                        extra_number: Some(error_code as u64),
                                        flags: Some(last_stream_id as u64),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                let _ = respond_to.send(Ok(Value::Null));
                            }
                            Http2SessionCommand::Close { respond_to, .. } => {
                                let _ = respond_to.send(Ok(Value::Null));
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionClose"),
                                        id: session_id,
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                remove_http2_session_resources(&shared, session_id);
                                break;
                            }
                            Http2SessionCommand::StreamWrite { stream_id, chunk, end_stream, respond_to } => {
                                let result = streams
                                    .lock()
                                    .expect("http2 client streams")
                                    .get_mut(&stream_id)
                                    .and_then(|stream| stream.send_stream.as_mut())
                                    .ok_or_else(|| SidecarError::InvalidState(format!("unknown HTTP/2 client stream {stream_id}")))
                                    .and_then(|stream| stream.send_data(Bytes::from(chunk), end_stream).map_err(|error| SidecarError::Execution(error.to_string())));
                                match result {
                                    Ok(()) => {
                                        if end_stream {
                                            streams.lock().expect("http2 client streams").remove(&stream_id);
                                        }
                                        let _ = respond_to.send(Ok(Value::Bool(true)));
                                    }
                                    Err(error) => {
                                        let _ = respond_to.send(Err(error.to_string()));
                                    }
                                }
                            }
                            Http2SessionCommand::StreamClose { stream_id, error_code, respond_to } => {
                                let mut streams = streams.lock().expect("http2 client streams");
                                let Some(mut state) = streams.remove(&stream_id) else {
                                    let _ = respond_to.send(Err(format!("unknown HTTP/2 client stream {stream_id}")));
                                    continue;
                                };
                                if let Some(stream) = state.send_stream.as_mut() {
                                    stream.send_reset(http2_reason(error_code));
                                }
                                if let Ok(mut state) = shared.lock() {
                                    state.streams.remove(&stream_id);
                                }
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("clientClose"),
                                        id: stream_id,
                                        extra_number: Some(u32::from(http2_reason(error_code)) as u64),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                let _ = respond_to.send(Ok(Value::Null));
                            }
                            Http2SessionCommand::StreamRespond { respond_to, .. }
                            | Http2SessionCommand::StreamPush { respond_to, .. }
                            | Http2SessionCommand::StreamRespondWithFile { respond_to, .. } => {
                                let _ = respond_to.send(Err(String::from("HTTP/2 client streams cannot send server responses")));
                            }
                        }
                    }
                    else => break,
                }
            }
        });
    });
}

fn spawn_http2_server_session(
    shared: Arc<Mutex<crate::state::Http2SharedState>>,
    server_id: u64,
    session_id: u64,
    stream: TcpStream,
    snapshot: Arc<Mutex<Http2SessionSnapshot>>,
    mut command_rx: UnboundedReceiver<Http2SessionCommand>,
) {
    thread::spawn(move || {
        let runtime = match TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                push_http2_server_event(
                    &shared,
                    server_id,
                    Http2BridgeEvent {
                        kind: String::from("serverStreamError"),
                        id: session_id,
                        data: Some(http2_error_payload(error.to_string())),
                        ..Http2BridgeEvent::default()
                    },
                );
                remove_http2_session_resources(&shared, session_id);
                return;
            }
        };

        runtime.block_on(async move {
            if let Err(error) = stream.set_nonblocking(true) {
                push_http2_server_event(
                    &shared,
                    server_id,
                    Http2BridgeEvent {
                        kind: String::from("serverStreamError"),
                        id: session_id,
                        data: Some(http2_error_payload(error.to_string())),
                        ..Http2BridgeEvent::default()
                    },
                );
                remove_http2_session_resources(&shared, session_id);
                return;
            }
            let stream = match tokio::net::TcpStream::from_std(stream) {
                Ok(stream) => stream,
                Err(error) => {
                    push_http2_server_event(
                        &shared,
                        server_id,
                        Http2BridgeEvent {
                            kind: String::from("serverStreamError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };
            let local_addr = match stream.local_addr() {
                Ok(addr) => addr,
                Err(error) => {
                    push_http2_server_event(
                        &shared,
                        server_id,
                        Http2BridgeEvent {
                            kind: String::from("serverStreamError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };
            let remote_addr = match stream.peer_addr() {
                Ok(addr) => addr,
                Err(error) => {
                    push_http2_server_event(
                        &shared,
                        server_id,
                        Http2BridgeEvent {
                            kind: String::from("serverStreamError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };
            {
                let mut snapshot_guard = snapshot.lock().expect("http2 snapshot lock");
                snapshot_guard.socket = http2_socket_snapshot(local_addr, remote_addr);
                snapshot_guard.state = http2_runtime_snapshot();
            }
            if let Ok(snapshot_json) =
                http2_snapshot_json(&snapshot.lock().expect("http2 snapshot lock").clone())
            {
                push_http2_server_event(
                    &shared,
                    server_id,
                    Http2BridgeEvent {
                        kind: String::from("serverConnection"),
                        id: server_id,
                        data: Some(serde_json::to_string(&http2_socket_snapshot(local_addr, remote_addr)).unwrap_or_default()),
                        ..Http2BridgeEvent::default()
                    },
                );
                push_http2_server_event(
                    &shared,
                    server_id,
                    Http2BridgeEvent {
                        kind: String::from("serverSession"),
                        id: server_id,
                        data: Some(snapshot_json),
                        extra_number: Some(session_id),
                        ..Http2BridgeEvent::default()
                    },
                );
            }

            let mut connection = match server::handshake(stream).await {
                Ok(connection) => connection,
                Err(error) => {
                    push_http2_server_event(
                        &shared,
                        server_id,
                        Http2BridgeEvent {
                            kind: String::from("serverStreamError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };

            let streams: Arc<Mutex<BTreeMap<u64, ServerHttp2StreamState>>> =
                Arc::new(Mutex::new(BTreeMap::new()));

            loop {
                tokio::select! {
                    incoming = connection.accept() => {
                        match incoming {
                            Some(Ok((request, respond))) => {
                                let headers_json = match serialize_http2_request_headers(&request) {
                                    Ok(headers) => headers,
                                    Err(error) => {
                                        push_http2_server_event(
                                            &shared,
                                            server_id,
                                            Http2BridgeEvent {
                                                kind: String::from("serverStreamError"),
                                                id: server_id,
                                                data: Some(http2_error_payload(error.to_string())),
                                                ..Http2BridgeEvent::default()
                                            },
                                        );
                                        continue;
                                    }
                                };
                                let stream_id = {
                                    let mut state = shared.lock().expect("http2 shared state");
                                    let stream_id = next_http2_stream_id(&mut state);
                                    state.streams.insert(
                                        stream_id,
                                        ActiveHttp2Stream {
                                            session_id,
                                            direction: Http2StreamDirection::Server,
                                            paused: Arc::new(AtomicBool::new(false)),
                                        },
                                    );
                                    stream_id
                                };
                                streams.lock().expect("http2 server streams").insert(
                                    stream_id,
                                    ServerHttp2StreamState {
                                        send_response: Some(ServerHttp2Responder::Regular(respond)),
                                        send_stream: None,
                                    },
                                );
                                let snapshot_json = snapshot
                                    .lock()
                                    .ok()
                                    .and_then(|snapshot| http2_snapshot_json(&snapshot.clone()).ok());
                                push_http2_server_event(
                                    &shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("serverStream"),
                                        id: server_id,
                                        data: Some(stream_id.to_string()),
                                        extra: snapshot_json,
                                        extra_number: Some(session_id),
                                        extra_headers: Some(headers_json),
                                        flags: Some(0),
                                    },
                                );
                                let shared_clone = Arc::clone(&shared);
                                tokio::spawn(async move {
                                    let mut body = request.into_body();
                                    while let Some(chunk) = body.data().await {
                                        match chunk {
                                            Ok(bytes) => {
                                                let paused = {
                                                    let state = shared_clone.lock().expect("http2 shared state");
                                                    state.streams.get(&stream_id).map(|stream| Arc::clone(&stream.paused))
                                                };
                                                if let Some(paused) = paused {
                                                    while paused.load(Ordering::SeqCst) {
                                                        tokio::time::sleep(HTTP2_POLL_DELAY).await;
                                                    }
                                                }
                                                let _ = body.flow_control().release_capacity(bytes.len());
                                                push_http2_server_event(
                                                    &shared_clone,
                                                    server_id,
                                                    Http2BridgeEvent {
                                                        kind: String::from("serverStreamData"),
                                                        id: stream_id,
                                                        data: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
                                                        ..Http2BridgeEvent::default()
                                                    },
                                                );
                                            }
                                            Err(error) => {
                                                push_http2_server_event(
                                                    &shared_clone,
                                                    server_id,
                                                    Http2BridgeEvent {
                                                        kind: String::from("serverStreamError"),
                                                        id: stream_id,
                                                        data: Some(http2_error_payload(error.to_string())),
                                                        ..Http2BridgeEvent::default()
                                                    },
                                                );
                                                break;
                                            }
                                        }
                                    }
                                    push_http2_server_event(
                                        &shared_clone,
                                        server_id,
                                        Http2BridgeEvent {
                                            kind: String::from("serverStreamEnd"),
                                            id: stream_id,
                                            ..Http2BridgeEvent::default()
                                        },
                                    );
                                });
                            }
                            Some(Err(error)) => {
                                push_http2_server_event(
                                    &shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("serverStreamError"),
                                        id: server_id,
                                        data: Some(http2_error_payload(error.to_string())),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                break;
                            }
                            None => {
                                push_http2_server_event(
                                    &shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionClose"),
                                        id: session_id,
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                remove_http2_session_resources(&shared, session_id);
                                break;
                            }
                        }
                    }
                    Some(command) = command_rx.recv() => {
                        match command {
                            Http2SessionCommand::Settings { settings_json, respond_to } => {
                                let settings = serde_json::from_str::<BTreeMap<String, Value>>(&settings_json)
                                    .unwrap_or_default();
                                if let Some(initial_window_size) = settings
                                    .get("initialWindowSize")
                                    .and_then(Value::as_u64)
                                {
                                    let _ = connection.set_initial_window_size(initial_window_size as u32);
                                }
                                {
                                    let mut snapshot = snapshot.lock().expect("http2 snapshot lock");
                                    snapshot.local_settings = http2_settings_from_value(&settings);
                                }
                                if let Ok(headers_json) = serde_json::to_string(&settings) {
                                    push_http2_session_event(
                                        &shared,
                                        session_id,
                                        Http2BridgeEvent {
                                            kind: String::from("sessionLocalSettings"),
                                            id: session_id,
                                            data: Some(headers_json),
                                            ..Http2BridgeEvent::default()
                                        },
                                    );
                                }
                                let _ = respond_to.send(Ok(Value::Null));
                            }
                            Http2SessionCommand::SetLocalWindowSize { size, respond_to } => {
                                connection.set_target_window_size(size);
                                {
                                    let mut snapshot = snapshot.lock().expect("http2 snapshot lock");
                                    snapshot.state.local_window_size = size;
                                    snapshot.state.effective_local_window_size = size;
                                }
                                let value = snapshot
                                    .lock()
                                    .ok()
                                    .and_then(|snapshot| http2_snapshot_json(&snapshot.clone()).ok())
                                    .map(Value::String)
                                    .unwrap_or(Value::Null);
                                let _ = respond_to.send(Ok(value));
                            }
                            Http2SessionCommand::Goaway { error_code, last_stream_id, opaque_data, respond_to } => {
                                connection.abrupt_shutdown(http2_reason(Some(error_code)));
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionGoaway"),
                                        id: session_id,
                                        data: opaque_data.map(|value| {
                                            base64::engine::general_purpose::STANDARD.encode(value)
                                        }),
                                        extra_number: Some(error_code as u64),
                                        flags: Some(last_stream_id as u64),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                let _ = respond_to.send(Ok(Value::Null));
                            }
                            Http2SessionCommand::Close { abrupt, respond_to } => {
                                if abrupt {
                                    connection.abrupt_shutdown(Reason::NO_ERROR);
                                } else {
                                    connection.graceful_shutdown();
                                }
                                let _ = respond_to.send(Ok(Value::Null));
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionClose"),
                                        id: session_id,
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                remove_http2_session_resources(&shared, session_id);
                                break;
                            }
                            Http2SessionCommand::StreamRespond { stream_id, headers_json, respond_to } => {
                                let response = match build_http2_response(&headers_json) {
                                    Ok(response) => response,
                                    Err(error) => {
                                        let _ = respond_to.send(Err(error.to_string()));
                                        continue;
                                    }
                                };
                                let mut streams = streams.lock().expect("http2 server streams");
                                let Some(state) = streams.get_mut(&stream_id) else {
                                    let _ = respond_to.send(Err(format!("unknown HTTP/2 server stream {stream_id}")));
                                    continue;
                                };
                                let Some(send_response) = state.send_response.as_mut() else {
                                    let _ = respond_to.send(Err(format!("HTTP/2 server stream {stream_id} already responded")));
                                    continue;
                                };
                                match match send_response {
                                    ServerHttp2Responder::Regular(send_response) => {
                                        send_response.send_response(response, false)
                                    }
                                    ServerHttp2Responder::Pushed(send_response) => {
                                        send_response.send_response(response, false)
                                    }
                                } {
                                    Ok(send_stream) => {
                                        state.send_stream = Some(send_stream);
                                        state.send_response = None;
                                        let _ = respond_to.send(Ok(Value::Null));
                                    }
                                    Err(error) => {
                                        let _ = respond_to.send(Err(error.to_string()));
                                    }
                                }
                            }
                            Http2SessionCommand::StreamPush { stream_id, headers_json, options_json: _, respond_to } => {
                                let request = match build_http2_request(&headers_json) {
                                    Ok(request) => request,
                                    Err(error) => {
                                        let _ = respond_to.send(Err(error.to_string()));
                                        continue;
                                    }
                                };
                                let mut streams_guard = streams.lock().expect("http2 server streams");
                                let Some(state) = streams_guard.get_mut(&stream_id) else {
                                    let _ = respond_to.send(Err(format!("unknown HTTP/2 server stream {stream_id}")));
                                    continue;
                                };
                                let Some(send_response) = state.send_response.as_mut() else {
                                    let _ = respond_to.send(Err(format!("HTTP/2 server stream {stream_id} cannot push after responding")));
                                    continue;
                                };
                                let ServerHttp2Responder::Regular(send_response) = send_response else {
                                    let _ = respond_to.send(Err(format!("HTTP/2 pushed stream {stream_id} cannot create nested push promises")));
                                    continue;
                                };
                                match send_response.push_request(request) {
                                    Ok(mut pushed) => {
                                        let pushed_stream_id = {
                                            let mut state = shared.lock().expect("http2 shared state");
                                            let pushed_stream_id = next_http2_stream_id(&mut state);
                                            state.streams.insert(
                                                pushed_stream_id,
                                                ActiveHttp2Stream {
                                                    session_id,
                                                    direction: Http2StreamDirection::Server,
                                                    paused: Arc::new(AtomicBool::new(false)),
                                                },
                                            );
                                            pushed_stream_id
                                        };
                                        streams_guard.insert(
                                            pushed_stream_id,
                                            ServerHttp2StreamState {
                                                send_response: Some(ServerHttp2Responder::Pushed(pushed)),
                                                send_stream: None,
                                            },
                                        );
                                        let _ = respond_to.send(Ok(json!({
                                            "streamId": pushed_stream_id,
                                            "headers": headers_json,
                                        }).to_string().into()));
                                    }
                                    Err(error) => {
                                        let _ = respond_to.send(Err(error.to_string()));
                                    }
                                }
                            }
                            Http2SessionCommand::StreamWrite { stream_id, chunk, end_stream, respond_to } => {
                                let mut streams = streams.lock().expect("http2 server streams");
                                let Some(state) = streams.get_mut(&stream_id) else {
                                    let _ = respond_to.send(Err(format!("unknown HTTP/2 server stream {stream_id}")));
                                    continue;
                                };
                                let Some(send_stream) = state.send_stream.as_mut() else {
                                    let _ = respond_to.send(Err(format!("HTTP/2 server stream {stream_id} has not sent response headers")));
                                    continue;
                                };
                                match send_stream.send_data(Bytes::from(chunk), end_stream) {
                                    Ok(()) => {
                                        if end_stream {
                                            streams.remove(&stream_id);
                                            if let Ok(mut state) = shared.lock() {
                                                state.streams.remove(&stream_id);
                                            }
                                            push_http2_server_event(
                                                &shared,
                                                server_id,
                                                Http2BridgeEvent {
                                                    kind: String::from("serverStreamClose"),
                                                    id: stream_id,
                                                    extra_number: Some(0),
                                                    ..Http2BridgeEvent::default()
                                                },
                                            );
                                        }
                                        let _ = respond_to.send(Ok(Value::Bool(true)));
                                    }
                                    Err(error) => {
                                        let _ = respond_to.send(Err(error.to_string()));
                                    }
                                }
                            }
                            Http2SessionCommand::StreamClose { stream_id, error_code, respond_to } => {
                                let mut streams_guard = streams.lock().expect("http2 server streams");
                                let Some(mut state) = streams_guard.remove(&stream_id) else {
                                    let _ = respond_to.send(Err(format!("unknown HTTP/2 server stream {stream_id}")));
                                    continue;
                                };
                                let reason = http2_reason(error_code);
                                if let Some(send_stream) = state.send_stream.as_mut() {
                                    send_stream.send_reset(reason);
                                }
                                if let Some(send_response) = state.send_response.as_mut() {
                                    match send_response {
                                        ServerHttp2Responder::Regular(send_response) => {
                                            send_response.send_reset(reason)
                                        }
                                        ServerHttp2Responder::Pushed(send_response) => {
                                            send_response.send_reset(reason)
                                        }
                                    }
                                }
                                if let Ok(mut shared_guard) = shared.lock() {
                                    shared_guard.streams.remove(&stream_id);
                                }
                                push_http2_server_event(
                                    &shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("serverStreamClose"),
                                        id: stream_id,
                                        extra_number: Some(u32::from(reason) as u64),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                let _ = respond_to.send(Ok(Value::Null));
                            }
                            Http2SessionCommand::StreamRespondWithFile { stream_id, path, headers_json, options_json, respond_to } => {
                                let options: JavascriptHttp2FileResponseOptions =
                                    serde_json::from_str(&options_json).unwrap_or_default();
                                let response = match build_http2_response(&headers_json) {
                                    Ok(response) => response,
                                    Err(error) => {
                                        let _ = respond_to.send(Err(error.to_string()));
                                        continue;
                                    }
                                };
                                let body = match fs::read(&path) {
                                    Ok(body) => body,
                                    Err(error) => {
                                        let _ = respond_to.send(Err(error.to_string()));
                                        continue;
                                    }
                                };
                                let offset = usize::try_from(options.offset.unwrap_or_default()).unwrap_or(0);
                                let body = if offset >= body.len() {
                                    Vec::new()
                                } else {
                                    let body = &body[offset..];
                                    match options.length {
                                        Some(length) if length >= 0 => {
                                            body[..body.len().min(length as usize)].to_vec()
                                        }
                                        _ => body.to_vec(),
                                    }
                                };
                                let mut streams_guard = streams.lock().expect("http2 server streams");
                                let Some(state) = streams_guard.get_mut(&stream_id) else {
                                    let _ = respond_to.send(Err(format!("unknown HTTP/2 server stream {stream_id}")));
                                    continue;
                                };
                                let Some(send_response) = state.send_response.as_mut() else {
                                    let _ = respond_to.send(Err(format!("HTTP/2 server stream {stream_id} already responded")));
                                    continue;
                                };
                                match match send_response {
                                    ServerHttp2Responder::Regular(send_response) => {
                                        send_response.send_response(response, body.is_empty())
                                    }
                                    ServerHttp2Responder::Pushed(send_response) => {
                                        send_response.send_response(response, body.is_empty())
                                    }
                                } {
                                    Ok(mut send_stream) => {
                                        state.send_response = None;
                                        if body.is_empty() {
                                            streams_guard.remove(&stream_id);
                                            if let Ok(mut shared_guard) = shared.lock() {
                                                shared_guard.streams.remove(&stream_id);
                                            }
                                        } else {
                                            if let Err(error) = send_stream.send_data(Bytes::from(body), true) {
                                                let _ = respond_to.send(Err(error.to_string()));
                                                continue;
                                            }
                                            streams_guard.remove(&stream_id);
                                            if let Ok(mut shared_guard) = shared.lock() {
                                                shared_guard.streams.remove(&stream_id);
                                            }
                                        }
                                        push_http2_server_event(
                                            &shared,
                                            server_id,
                                            Http2BridgeEvent {
                                                kind: String::from("serverStreamClose"),
                                                id: stream_id,
                                                extra_number: Some(0),
                                                ..Http2BridgeEvent::default()
                                            },
                                        );
                                        let _ = respond_to.send(Ok(Value::Null));
                                    }
                                    Err(error) => {
                                        let _ = respond_to.send(Err(error.to_string()));
                                    }
                                }
                            }
                            Http2SessionCommand::Request { respond_to, .. } => {
                                let _ = respond_to.send(Err(String::from("HTTP/2 server sessions cannot initiate client requests")));
                            }
                        }
                    }
                    else => break,
                }
            }
        });
    });
}

fn spawn_http2_server_accept_loop(
    shared: Arc<Mutex<crate::state::Http2SharedState>>,
    server_id: u64,
    listener: TcpListener,
) {
    thread::spawn(move || {
        let listener = listener;
        loop {
            let closed = shared
                .lock()
                .ok()
                .and_then(|state| {
                    state
                        .servers
                        .get(&server_id)
                        .map(|server| server.closed.load(Ordering::SeqCst))
                })
                .unwrap_or(true);
            if closed {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let (command_tx, command_rx) = unbounded_channel();
                    let (guest_local_addr, secure) = {
                        let state = shared.lock().expect("http2 shared state");
                        let server = state.servers.get(&server_id).expect("http2 server state");
                        (server.guest_local_addr, server.secure)
                    };
                    let (local_addr, remote_addr) = match (stream.local_addr(), stream.peer_addr())
                    {
                        (Ok(local_addr), Ok(remote_addr)) => (local_addr, remote_addr),
                        _ => continue,
                    };
                    let session_snapshot = Arc::new(Mutex::new(Http2SessionSnapshot {
                        encrypted: secure,
                        alpn_protocol: Some(if secure {
                            String::from("h2")
                        } else {
                            String::from("h2c")
                        }),
                        local_settings: BTreeMap::new(),
                        remote_settings: BTreeMap::new(),
                        state: http2_runtime_snapshot(),
                        socket: Http2SocketSnapshot {
                            local_address: Some(guest_local_addr.ip().to_string()),
                            local_port: Some(guest_local_addr.port()),
                            local_family: Some(socket_addr_family(&guest_local_addr).to_string()),
                            remote_address: Some(remote_addr.ip().to_string()),
                            remote_port: Some(remote_addr.port()),
                            remote_family: Some(socket_addr_family(&remote_addr).to_string()),
                            ..http2_socket_snapshot(local_addr, remote_addr)
                        },
                        ..Http2SessionSnapshot::default()
                    }));
                    let session_id = {
                        let mut state = shared.lock().expect("http2 shared state");
                        let session_id = next_http2_session_id(&mut state);
                        state.sessions.insert(
                            session_id,
                            ActiveHttp2Session {
                                server_id: Some(server_id),
                                secure,
                                command_tx,
                                snapshot: Arc::clone(&session_snapshot),
                            },
                        );
                        session_id
                    };
                    spawn_http2_server_session(
                        Arc::clone(&shared),
                        server_id,
                        session_id,
                        stream,
                        session_snapshot,
                        command_rx,
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(HTTP2_POLL_DELAY);
                }
                Err(error) => {
                    push_http2_server_event(
                        &shared,
                        server_id,
                        Http2BridgeEvent {
                            kind: String::from("serverStreamError"),
                            id: server_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    thread::sleep(HTTP2_POLL_DELAY);
                }
            }
        }
    });
}

fn send_http2_command(
    session: &ActiveHttp2Session,
    command: impl FnOnce(Sender<Result<Value, String>>) -> Http2SessionCommand,
) -> Result<Value, SidecarError> {
    let (respond_to, response_rx) = mpsc::channel();
    session.command_tx.send(command(respond_to)).map_err(|_| {
        SidecarError::InvalidState(String::from("HTTP/2 session command channel closed"))
    })?;
    response_rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| {
            SidecarError::Execution(String::from("timed out waiting for HTTP/2 session command"))
        })?
        .map_err(SidecarError::Execution)
}

fn parse_http2_server_listen_payload(
    request: &JavascriptSyncRpcRequest,
) -> Result<JavascriptHttp2ServerListenRequest, SidecarError> {
    let payload_json =
        javascript_sync_rpc_arg_str(&request.args, 0, "net.http2_server_listen payload")?;
    serde_json::from_str(payload_json).map_err(|error| {
        SidecarError::InvalidState(format!(
            "net.http2_server_listen payload must be valid JSON: {error}"
        ))
    })
}

fn parse_http2_connect_payload(
    request: &JavascriptSyncRpcRequest,
) -> Result<JavascriptHttp2SessionConnectRequest, SidecarError> {
    let payload_json =
        javascript_sync_rpc_arg_str(&request.args, 0, "net.http2_session_connect payload")?;
    serde_json::from_str(payload_json).map_err(|error| {
        SidecarError::InvalidState(format!(
            "net.http2_session_connect payload must be valid JSON: {error}"
        ))
    })
}

fn http2_session_for_id(
    process: &ActiveProcess,
    session_id: u64,
) -> Result<ActiveHttp2Session, SidecarError> {
    let shared = process
        .http2
        .shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?;
    shared
        .sessions
        .get(&session_id)
        .cloned()
        .ok_or_else(|| SidecarError::InvalidState(format!("unknown HTTP/2 session {session_id}")))
}

fn http2_stream_for_id(
    process: &ActiveProcess,
    stream_id: u64,
) -> Result<ActiveHttp2Stream, SidecarError> {
    let shared = process
        .http2
        .shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?;
    shared
        .streams
        .get(&stream_id)
        .cloned()
        .ok_or_else(|| SidecarError::InvalidState(format!("unknown HTTP/2 stream {stream_id}")))
}

fn service_javascript_http2_sync_rpc<B>(
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
        "net.http2_server_listen" => {
            check_network_resource_limit(
                resource_limits.max_sockets,
                network_counts.sockets,
                1,
                "socket",
            )?;
            let payload = parse_http2_server_listen_payload(request)?;
            if payload.secure {
                return Err(SidecarError::Unsupported(String::from(
                    "HTTP/2 secure servers are not supported yet in the sidecar bridge",
                )));
            }
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
            let guest_local_addr = listener.guest_local_addr();
            let closed = Arc::new(AtomicBool::new(false));
            {
                let mut state = process.http2.shared.lock().map_err(|_| {
                    SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned"))
                })?;
                state.servers.insert(
                    payload.server_id,
                    ActiveHttp2Server {
                        actual_local_addr: listener.local_addr(),
                        guest_local_addr,
                        secure: false,
                        closed: Arc::clone(&closed),
                    },
                );
                state.server_events.entry(payload.server_id).or_default();
            }
            spawn_http2_server_accept_loop(
                Arc::clone(&process.http2.shared),
                payload.server_id,
                listener.listener,
            );
            javascript_net_json_string(
                json!({
                    "address": {
                        "address": guest_local_addr.ip().to_string(),
                        "family": socket_addr_family(&guest_local_addr),
                        "port": guest_local_addr.port(),
                    }
                }),
                "net.http2_server_listen",
            )
        }
        "net.http2_server_poll" => {
            let server_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_server_poll server id")?;
            let wait_ms = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "net.http2_server_poll wait ms",
            )?
            .unwrap_or_default();
            match wait_for_http2_event(&process.http2.shared, server_id, true, wait_ms) {
                Some(event) => http2_event_value(&event),
                None => Ok(Value::Null),
            }
        }
        "net.http2_server_wait" => Ok(Value::Null),
        "net.http2_server_close" => {
            let server_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_server_close server id")?;
            let server = {
                let mut state = process.http2.shared.lock().map_err(|_| {
                    SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned"))
                })?;
                state.servers.remove(&server_id)
            }
            .ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown HTTP/2 server {server_id}"))
            })?;
            server.closed.store(true, Ordering::SeqCst);
            push_http2_server_event(
                &process.http2.shared,
                server_id,
                Http2BridgeEvent {
                    kind: String::from("serverClose"),
                    id: server_id,
                    ..Http2BridgeEvent::default()
                },
            );
            Ok(Value::Null)
        }
        "net.http2_server_respond" => Ok(Value::Null),
        "net.http2_session_connect" => {
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
            let payload = parse_http2_connect_payload(request)?;
            let authority = payload.authority.clone().unwrap_or_else(|| {
                format!(
                    "{}://{}:{}",
                    payload.protocol.as_deref().unwrap_or("http"),
                    payload.host.as_deref().unwrap_or("localhost"),
                    payload.port.unwrap_or(80)
                )
            });
            let url = Url::parse(&authority).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid HTTP/2 authority {authority:?}: {error}"
                ))
            })?;
            if url.scheme() == "https" || payload.protocol.as_deref() == Some("https:") {
                return Err(SidecarError::Unsupported(String::from(
                    "HTTP/2 TLS clients are not supported yet in the sidecar bridge",
                )));
            }
            let host = payload
                .host
                .as_deref()
                .or_else(|| url.host_str())
                .unwrap_or("localhost");
            let port = payload.port.or_else(|| url.port()).unwrap_or(80);
            bridge.require_network_access(
                vm_id,
                NetworkOperation::Http,
                format_tcp_resource(host, port),
            )?;
            let resolved = {
                let shared = process.http2.shared.lock().map_err(|_| {
                    SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned"))
                })?;
                shared
                    .servers
                    .values()
                    .find(|server| {
                        is_loopback_request_host(host) && server.guest_local_addr.port() == port
                    })
                    .map(|server| ResolvedTcpConnectAddr {
                        actual_addr: server.actual_local_addr,
                        guest_remote_addr: server.guest_local_addr,
                    })
            };
            let resolved = match resolved {
                Some(resolved) => resolved,
                None => resolve_tcp_connect_addr(bridge, vm_id, dns, host, port, socket_paths)?,
            };
            let (command_tx, command_rx) = unbounded_channel();
            let snapshot = Arc::new(Mutex::new(Http2SessionSnapshot {
                encrypted: false,
                alpn_protocol: Some(String::from("h2c")),
                local_settings: http2_settings_from_value(&payload.settings),
                remote_settings: BTreeMap::new(),
                state: http2_runtime_snapshot(),
                socket: Http2SocketSnapshot {
                    remote_address: Some(resolved.guest_remote_addr.ip().to_string()),
                    remote_port: Some(resolved.guest_remote_addr.port()),
                    remote_family: Some(
                        socket_addr_family(&resolved.guest_remote_addr).to_string(),
                    ),
                    ..Http2SocketSnapshot::default()
                },
                ..Http2SessionSnapshot::default()
            }));
            let session_id = {
                let mut state = process.http2.shared.lock().map_err(|_| {
                    SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned"))
                })?;
                let session_id = next_http2_session_id(&mut state);
                state.sessions.insert(
                    session_id,
                    ActiveHttp2Session {
                        server_id: None,
                        secure: false,
                        command_tx,
                        snapshot: Arc::clone(&snapshot),
                    },
                );
                state.session_events.entry(session_id).or_default();
                session_id
            };
            spawn_http2_client_session(
                Arc::clone(&process.http2.shared),
                session_id,
                resolved.actual_addr,
                Arc::clone(&snapshot),
                command_rx,
            );
            let snapshot_json =
                http2_snapshot_json(&snapshot.lock().expect("http2 snapshot lock").clone())?;
            javascript_net_json_string(
                json!({
                    "sessionId": session_id,
                    "state": snapshot_json,
                }),
                "net.http2_session_connect",
            )
        }
        "net.http2_session_request" => {
            let session_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_session_request session id",
            )?;
            let headers_json =
                javascript_sync_rpc_arg_str(&request.args, 1, "net.http2_session_request headers")?;
            let options_json =
                javascript_sync_rpc_arg_str(&request.args, 2, "net.http2_session_request options")?;
            let session = http2_session_for_id(process, session_id)?;
            send_http2_command(&session, |respond_to| Http2SessionCommand::Request {
                headers_json: headers_json.to_owned(),
                options_json: options_json.to_owned(),
                respond_to,
            })
        }
        "net.http2_session_settings" => {
            let session_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_session_settings session id",
            )?;
            let settings_json = javascript_sync_rpc_arg_str(
                &request.args,
                1,
                "net.http2_session_settings settings",
            )?;
            let session = http2_session_for_id(process, session_id)?;
            send_http2_command(&session, |respond_to| Http2SessionCommand::Settings {
                settings_json: settings_json.to_owned(),
                respond_to,
            })
        }
        "net.http2_session_set_local_window_size" => {
            let session_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_session_set_local_window_size session id",
            )?;
            let window_size = javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "net.http2_session_set_local_window_size window size",
            )?;
            let session = http2_session_for_id(process, session_id)?;
            send_http2_command(&session, |respond_to| {
                Http2SessionCommand::SetLocalWindowSize {
                    size: window_size as u32,
                    respond_to,
                }
            })
        }
        "net.http2_session_goaway" => {
            let session_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_session_goaway session id",
            )?;
            let error_code = javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "net.http2_session_goaway error code",
            )?;
            let last_stream_id = javascript_sync_rpc_arg_u64(
                &request.args,
                2,
                "net.http2_session_goaway last stream id",
            )?;
            let opaque_data = request
                .args
                .get(3)
                .and_then(Value::as_str)
                .map(|value| {
                    base64::engine::general_purpose::STANDARD
                        .decode(value)
                        .map_err(|error| {
                            SidecarError::InvalidState(format!("invalid GOAWAY payload: {error}"))
                        })
                })
                .transpose()?;
            let session = http2_session_for_id(process, session_id)?;
            send_http2_command(&session, |respond_to| Http2SessionCommand::Goaway {
                error_code: error_code as u32,
                last_stream_id: last_stream_id as u32,
                opaque_data,
                respond_to,
            })
        }
        "net.http2_session_close" | "net.http2_session_destroy" => {
            let session_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_session_close session id",
            )?;
            let session = http2_session_for_id(process, session_id)?;
            send_http2_command(&session, |respond_to| Http2SessionCommand::Close {
                abrupt: request.method == "net.http2_session_destroy",
                respond_to,
            })
        }
        "net.http2_session_poll" => {
            let session_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_session_poll session id")?;
            let wait_ms = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "net.http2_session_poll wait ms",
            )?
            .unwrap_or_default();
            match wait_for_http2_event(&process.http2.shared, session_id, false, wait_ms) {
                Some(event) => http2_event_value(&event),
                None => Ok(Value::Null),
            }
        }
        "net.http2_session_wait" => Ok(Value::Null),
        "net.http2_stream_respond" => {
            let stream_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_stream_respond stream id",
            )?;
            let headers_json =
                javascript_sync_rpc_arg_str(&request.args, 1, "net.http2_stream_respond headers")?;
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            send_http2_command(&session, |respond_to| Http2SessionCommand::StreamRespond {
                stream_id,
                headers_json: headers_json.to_owned(),
                respond_to,
            })
        }
        "net.http2_stream_push_stream" => {
            let stream_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_stream_push_stream stream id",
            )?;
            let headers_json = javascript_sync_rpc_arg_str(
                &request.args,
                1,
                "net.http2_stream_push_stream headers",
            )?;
            let options_json = javascript_sync_rpc_arg_str(
                &request.args,
                2,
                "net.http2_stream_push_stream options",
            )?;
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            send_http2_command(&session, |respond_to| Http2SessionCommand::StreamPush {
                stream_id,
                headers_json: headers_json.to_owned(),
                options_json: options_json.to_owned(),
                respond_to,
            })
        }
        "net.http2_stream_write" => {
            let stream_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_stream_write stream id")?;
            let chunk =
                javascript_sync_rpc_base64_arg(&request.args, 1, "net.http2_stream_write data")?;
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            send_http2_command(&session, |respond_to| Http2SessionCommand::StreamWrite {
                stream_id,
                chunk,
                end_stream: false,
                respond_to,
            })
        }
        "net.http2_stream_end" => {
            let stream_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_stream_end stream id")?;
            let chunk = request
                .args
                .get(1)
                .and_then(Value::as_str)
                .map(|value| {
                    base64::engine::general_purpose::STANDARD
                        .decode(value)
                        .map_err(|error| {
                            SidecarError::InvalidState(format!(
                                "invalid HTTP/2 stream payload: {error}"
                            ))
                        })
                })
                .transpose()?
                .unwrap_or_default();
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            send_http2_command(&session, |respond_to| Http2SessionCommand::StreamWrite {
                stream_id,
                chunk,
                end_stream: true,
                respond_to,
            })
        }
        "net.http2_stream_close" => {
            let stream_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_stream_close stream id")?;
            let code = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "net.http2_stream_close error code",
            )?
            .map(|value| value as u32);
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            send_http2_command(&session, |respond_to| Http2SessionCommand::StreamClose {
                stream_id,
                error_code: code,
                respond_to,
            })
        }
        "net.http2_stream_pause" => {
            let stream_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_stream_pause stream id")?;
            let stream = http2_stream_for_id(process, stream_id)?;
            stream.paused.store(true, Ordering::SeqCst);
            Ok(Value::Null)
        }
        "net.http2_stream_resume" => {
            let stream_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_stream_resume stream id")?;
            let stream = http2_stream_for_id(process, stream_id)?;
            stream.paused.store(false, Ordering::SeqCst);
            Ok(Value::Null)
        }
        "net.http2_stream_respond_with_file" => {
            let stream_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_stream_respond_with_file stream id",
            )?;
            let path = javascript_sync_rpc_arg_str(
                &request.args,
                1,
                "net.http2_stream_respond_with_file path",
            )?;
            let headers_json = javascript_sync_rpc_arg_str(
                &request.args,
                2,
                "net.http2_stream_respond_with_file headers",
            )?;
            let options_json = javascript_sync_rpc_arg_str(
                &request.args,
                3,
                "net.http2_stream_respond_with_file options",
            )?;
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            send_http2_command(&session, |respond_to| {
                Http2SessionCommand::StreamRespondWithFile {
                    stream_id,
                    path: path.to_owned(),
                    headers_json: headers_json.to_owned(),
                    options_json: options_json.to_owned(),
                    respond_to,
                }
            })
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript HTTP/2 sync RPC method {other}"
        ))),
    }
}

pub(crate) fn service_javascript_net_sync_rpc<B>(
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
        "net.http_request" => {
            let (url, options, headers) = parse_http_request_options(request)?;
            let host = url.host_str().ok_or_else(|| {
                SidecarError::Execution(String::from("ERR_INVALID_URL: missing host"))
            })?;
            let port = url.port_or_known_default().ok_or_else(|| {
                SidecarError::Execution(String::from("ERR_INVALID_URL: missing port"))
            })?;
            bridge.require_network_access(
                vm_id,
                NetworkOperation::Http,
                format_tcp_resource(host, port),
            )?;

            if is_loopback_request_host(host) {
                if let Some((server_id, request_id, request_json)) = process
                    .http_servers
                    .iter_mut()
                    .find(|(_, server)| server.guest_local_addr.port() == port)
                    .map(|(server_id, server)| {
                        server.next_request_id += 1;
                        let request_id = server.next_request_id;
                        serialize_http_loopback_request(&url, &options, &headers)
                            .map(|request_json| (*server_id, request_id, request_json))
                    })
                    .transpose()?
                {
                    process
                        .pending_http_requests
                        .insert((server_id, request_id), None);
                    process.execution.send_javascript_stream_event(
                        "http_request",
                        json!({
                            "serverId": server_id,
                            "requestId": request_id,
                            "request": request_json,
                        }),
                    )?;
                    let response = wait_for_loopback_http_response(
                        bridge,
                        vm_id,
                        dns,
                        socket_paths,
                        kernel,
                        process,
                        resource_limits,
                        (server_id, request_id),
                    )?;
                    return Ok(Value::String(response));
                }
            }

            issue_outbound_http_request(&url, &options, &headers)
        }
        "net.http_listen" => {
            check_network_resource_limit(
                resource_limits.max_sockets,
                network_counts.sockets,
                1,
                "socket",
            )?;
            let payload_json =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.http_listen payload")?;
            let payload: JavascriptHttpListenRequest =
                serde_json::from_str(payload_json).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "net.http_listen payload must be valid JSON: {error}"
                    ))
                })?;
            let (family, host) = normalize_tcp_listen_host(payload.hostname.as_deref())?;
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
            let listener =
                ActiveTcpListener::bind(host, port, Some(DEFAULT_JAVASCRIPT_NET_BACKLOG))?;
            let guest_local_addr = listener.guest_local_addr();
            process.http_servers.insert(
                payload.server_id,
                ActiveHttpServer {
                    listener: listener.listener,
                    guest_local_addr,
                    next_request_id: 0,
                },
            );
            serde_json::to_string(&json!({
                "address": {
                    "address": guest_local_addr.ip().to_string(),
                    "family": socket_addr_family(&guest_local_addr),
                    "port": guest_local_addr.port(),
                }
            }))
            .map(Value::String)
            .map_err(|error| {
                SidecarError::Execution(format!("ERR_AGENT_OS_NODE_SYNC_RPC: {error}"))
            })
        }
        "net.http_close" => {
            let server_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http_close server id")?;
            let server = process.http_servers.remove(&server_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown HTTP server {server_id}"))
            })?;
            drop(server.listener);
            process
                .pending_http_requests
                .retain(|(pending_server_id, _), _| *pending_server_id != server_id);
            Ok(Value::Null)
        }
        "net.http_wait" => Ok(Value::Null),
        "net.http_respond" => {
            let server_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http_respond server id")?;
            let request_id =
                javascript_sync_rpc_arg_u64(&request.args, 1, "net.http_respond request id")?;
            let response_json =
                javascript_sync_rpc_arg_str(&request.args, 2, "net.http_respond payload")?;
            serde_json::from_str::<Value>(response_json).map_err(|error| {
                SidecarError::Execution(format!(
                    "net.http_respond payload must be valid JSON: {error}"
                ))
            })?;
            let Some(pending) = process
                .pending_http_requests
                .get_mut(&(server_id, request_id))
            else {
                return Err(SidecarError::InvalidState(format!(
                    "unknown pending HTTP request {request_id} for server {server_id}"
                )));
            };
            *pending = Some(response_json.to_owned());
            Ok(Value::Null)
        }
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
        "net.socket_wait_connect" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_wait_connect socket id")?;
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                javascript_net_json_string(socket.socket_info(), "net.socket_wait_connect")
            } else {
                let socket = process.unix_sockets.get(socket_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net socket {socket_id}"))
                })?;
                javascript_net_json_string(socket.socket_info(), "net.socket_wait_connect")
            }
        }
        "net.socket_read" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_read socket id")?;
            if let Some(socket) = process.tcp_sockets.get_mut(socket_id) {
                javascript_net_read_value(socket.poll(Duration::ZERO)?)
            } else {
                let socket = process.unix_sockets.get_mut(socket_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net socket {socket_id}"))
                })?;
                javascript_net_read_value(socket.poll(Duration::ZERO)?)
            }
        }
        "net.socket_set_no_delay" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_set_no_delay socket id")?;
            let enable =
                javascript_sync_rpc_arg_bool(&request.args, 1, "net.socket_set_no_delay enabled")?;
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                socket.set_no_delay(enable)?;
            } else if !process.unix_sockets.contains_key(socket_id) {
                return Err(SidecarError::InvalidState(format!(
                    "unknown net socket {socket_id}"
                )));
            }
            Ok(Value::Null)
        }
        "net.socket_set_keep_alive" => {
            let socket_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.socket_set_keep_alive socket id",
            )?;
            let enable = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "net.socket_set_keep_alive enabled",
            )?;
            let initial_delay_secs = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                2,
                "net.socket_set_keep_alive initial delay seconds",
            )?;
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                socket.set_keep_alive(enable, initial_delay_secs)?;
            } else if !process.unix_sockets.contains_key(socket_id) {
                return Err(SidecarError::InvalidState(format!(
                    "unknown net socket {socket_id}"
                )));
            }
            Ok(Value::Null)
        }
        "net.socket_upgrade_tls" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_upgrade_tls socket id")?;
            let options_json =
                javascript_sync_rpc_arg_str(&request.args, 1, "net.socket_upgrade_tls options")?;
            let options: JavascriptTlsBridgeOptions =
                serde_json::from_str(options_json).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "net.socket_upgrade_tls options must be valid JSON: {error}"
                    ))
                })?;
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "unknown TCP socket {socket_id} for TLS upgrade"
                ))
            })?;
            socket.upgrade_tls(options)?;
            Ok(Value::Null)
        }
        "net.socket_get_tls_client_hello" => {
            let socket_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.socket_get_tls_client_hello socket id",
            )?;
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "unknown TCP socket {socket_id} for TLS client hello query"
                ))
            })?;
            socket.tls_client_hello_json()
        }
        "net.socket_tls_query" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_tls_query socket id")?;
            let query =
                javascript_sync_rpc_arg_str(&request.args, 1, "net.socket_tls_query query")?;
            let detailed = request
                .args
                .get(2)
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown TCP socket {socket_id} for TLS query"))
            })?;
            socket.tls_query(query, detailed)
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
        "net.server_accept" => {
            let listener_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.server_accept listener id")?;
            if let Some(listener) = process.tcp_listeners.get_mut(listener_id) {
                return match listener.poll(Duration::ZERO)? {
                    Some(JavascriptTcpListenerEvent::Connection(pending)) => {
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
                        let info = json!({
                            "localAddress": pending.guest_local_addr.ip().to_string(),
                            "localPort": pending.guest_local_addr.port(),
                            "localFamily": socket_addr_family(&pending.guest_local_addr),
                            "remoteAddress": pending.guest_remote_addr.ip().to_string(),
                            "remotePort": pending.guest_remote_addr.port(),
                            "remoteFamily": socket_addr_family(&pending.guest_remote_addr),
                        });
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
                        javascript_net_json_string(
                            json!({
                                "socketId": socket_id,
                                "info": info,
                            }),
                            "net.server_accept",
                        )
                    }
                    Some(JavascriptTcpListenerEvent::Error { code, message }) => {
                        let detail = code.unwrap_or_else(|| String::from("server accept"));
                        Err(SidecarError::Execution(format!("{detail}: {message}")))
                    }
                    None => Ok(javascript_net_timeout_value()),
                };
            }

            let listener = process.unix_listeners.get_mut(listener_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown net listener {listener_id}"))
            })?;
            match listener.poll(Duration::ZERO)? {
                Some(JavascriptUnixListenerEvent::Connection(pending)) => {
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
                    let info = json!({
                        "localPath": pending.local_path.clone(),
                        "remotePath": pending.remote_path.clone(),
                    });
                    let socket = ActiveUnixSocket::from_stream(
                        pending.stream,
                        Some(listener_id.to_string()),
                        pending.local_path,
                        pending.remote_path,
                    )?;
                    let socket_id = process.allocate_unix_socket_id();
                    if let Some(listener) = process.unix_listeners.get_mut(listener_id) {
                        listener.register_connection(&socket_id);
                    }
                    process.unix_sockets.insert(socket_id.clone(), socket);
                    javascript_net_json_string(
                        json!({
                            "socketId": socket_id,
                            "info": info,
                        }),
                        "net.server_accept",
                    )
                }
                Some(JavascriptUnixListenerEvent::Error { code, message }) => {
                    let detail = code.unwrap_or_else(|| String::from("server accept"));
                    Err(SidecarError::Execution(format!("{detail}: {message}")))
                }
                None => Ok(javascript_net_timeout_value()),
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
        "net.upgrade_socket_write" => {
            let socket_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.upgrade_socket_write socket id",
            )?;
            let chunk =
                javascript_sync_rpc_base64_arg(&request.args, 1, "net.upgrade_socket_write chunk")?;
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown TCP socket {socket_id}"))
            })?;
            socket.write_all(&chunk).map(|written| json!(written))
        }
        "net.upgrade_socket_end" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.upgrade_socket_end socket id")?;
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown TCP socket {socket_id}"))
            })?;
            socket.shutdown_write()?;
            Ok(Value::Null)
        }
        "net.upgrade_socket_destroy" => {
            let socket_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.upgrade_socket_destroy socket id",
            )?;
            let socket = process.tcp_sockets.remove(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown TCP socket {socket_id}"))
            })?;
            if let Some(listener_id) = socket.listener_id.as_deref() {
                if let Some(listener) = process.tcp_listeners.get_mut(listener_id) {
                    listener.release_connection(socket_id);
                }
            }
            let _ = socket.close();
            Ok(Value::Null)
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
        "tls.get_ciphers" => javascript_net_json_string(
            Value::Array(
                tls_provider()
                    .cipher_suites
                    .iter()
                    .filter_map(|suite| {
                        suite
                            .suite()
                            .as_str()
                            .map(|value| Value::String(value.to_owned()))
                    })
                    .collect(),
            ),
            "tls.get_ciphers",
        ),
        _ => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript net sync RPC method {}",
            request.method
        ))),
    }
}

pub(crate) fn parse_signal(signal: &str) -> Result<i32, SidecarError> {
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

pub(crate) fn runtime_child_is_alive(child_pid: u32) -> Result<bool, SidecarError> {
    if child_pid == 0 {
        return Ok(false);
    }

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

pub(crate) fn signal_runtime_process(child_pid: u32, signal: i32) -> Result<(), SidecarError> {
    if child_pid == 0 {
        return Ok(());
    }

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

pub(crate) fn error_code(error: &SidecarError) -> &'static str {
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

pub(crate) fn javascript_sync_rpc_error_code(error: &SidecarError) -> String {
    match error {
        SidecarError::Execution(message) => guest_errno_code(message)
            .unwrap_or("ERR_AGENT_OS_NODE_SYNC_RPC")
            .to_owned(),
        _ => String::from("ERR_AGENT_OS_NODE_SYNC_RPC"),
    }
}

pub(crate) fn ignore_stale_javascript_sync_rpc_response(
    error: SidecarError,
) -> Result<(), SidecarError> {
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
