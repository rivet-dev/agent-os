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
    ResponsePayload, SignalDispositionAction, SignalHandlerRegistration, SignalStateResponse,
    SocketStateEntry, StdinClosedResponse, StdinWrittenResponse, StreamChannel, WasmPermissionTier,
    WriteStdinRequest, ZombieTimerCountResponse,
};
use crate::service::{
    audit_fields, dirname, emit_security_audit_event, emit_structured_event, javascript_error,
    kernel_error, normalize_host_path, normalize_path, path_is_within_root, python_error,
    wasm_error,
};
use crate::state::{
    ActiveExecution, ActiveExecutionEvent, ActiveProcess, ActiveTcpListener, ActiveTcpSocket,
    ActiveUdpSocket, ActiveUnixListener, ActiveUnixSocket, BridgeError, DnsResolutionSource,
    JavascriptSocketFamily, JavascriptSocketPathContext, JavascriptTcpListenerEvent,
    JavascriptTcpSocketEvent, JavascriptUdpFamily, JavascriptUdpSocketEvent,
    JavascriptUnixListenerEvent, NetworkResourceCounts, PendingTcpSocket, PendingUnixSocket,
    ProcNetEntry, ProcessEventEnvelope, ResolvedChildProcessExecution, ResolvedTcpConnectAddr,
    SharedBridge, SidecarKernel, SocketQueryKind, VmDnsConfig, VmListenPolicy, VmState,
    DEFAULT_JAVASCRIPT_NET_BACKLOG, EXECUTION_DRIVER_NAME, EXECUTION_SANDBOX_ROOT_ENV,
    JAVASCRIPT_COMMAND, LOOPBACK_EXEMPT_PORTS_ENV, PYTHON_COMMAND,
    VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY, VM_LISTEN_PORT_MAX_METADATA_KEY,
    VM_LISTEN_PORT_MIN_METADATA_KEY, WASM_COMMAND,
};
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
use agent_os_kernel::kernel::{KernelProcessHandle, SpawnOptions};
use agent_os_kernel::permissions::NetworkOperation;
use agent_os_kernel::process_table::{SIGKILL, SIGTERM};
use agent_os_kernel::resource_accounting::ResourceLimits;
use base64::Engine;
use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::TokioResolver;
use nix::libc;
use nix::sys::signal::{kill as send_signal, Signal};
use nix::sys::wait::{waitid as wait_on_child, Id as WaitId, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
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
use url::Url;

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
            runtime,
            execution,
            host_cwd: PathBuf::from("/"),
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

    pub(crate) fn with_host_cwd(mut self, host_cwd: PathBuf) -> Self {
        self.host_cwd = host_cwd;
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

// ActiveExecution, ActiveExecutionEvent, SocketQueryKind moved to crate::state

impl ActiveExecution {
    pub(crate) fn child_pid(&self) -> u32 {
        match self {
            Self::Javascript(execution) => execution.child_pid(),
            Self::Python(execution) => execution.child_pid(),
            Self::Wasm(execution) => execution.child_pid(),
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

        let command = match payload.runtime {
            GuestRuntimeKind::JavaScript => JAVASCRIPT_COMMAND,
            GuestRuntimeKind::Python => PYTHON_COMMAND,
            GuestRuntimeKind::WebAssembly => WASM_COMMAND,
        };
        let mut env = vm.guest_env.clone();
        env.extend(payload.env.clone());
        let sandbox_root = normalize_host_path(&vm.cwd);
        let cwd = resolve_execution_cwd(vm, payload.cwd.as_deref())?;
        if payload.runtime == GuestRuntimeKind::JavaScript {
            let guest_entrypoint = if payload.entrypoint.starts_with('/') {
                Some(normalize_path(&payload.entrypoint))
            } else {
                guest_runtime_path_for_host_path(&env, &cwd, &payload.entrypoint)
            };
            if let Some(guest_entrypoint) = guest_entrypoint {
                env.entry(String::from("AGENT_OS_GUEST_ENTRYPOINT"))
                    .or_insert(guest_entrypoint);
            }
        }
        env.insert(
            String::from(EXECUTION_SANDBOX_ROOT_ENV),
            sandbox_root.to_string_lossy().into_owned(),
        );
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
                // Prefer guest VFS source when the entrypoint is available there,
                // but fall back to the VM's host root for fixture-style executions
                // that only provide a host cwd plus relative/absolute host paths.
                let inline_code =
                    load_javascript_entrypoint_source(vm, &cwd, &payload.entrypoint, &env);

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
                        inline_code,
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
            ActiveProcess::new(
                kernel_handle.pid(),
                kernel_handle,
                payload.runtime,
                execution,
            )
            .with_host_cwd(cwd.clone()),
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
                candidate
            } else {
                vm.cwd.clone()
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

        vm.active_processes
            .get_mut(process_id)
            .expect("process should still exist")
            .child_processes
            .insert(
                child_process_id.clone(),
                ActiveProcess::new(kernel_pid, kernel_handle, resolved.runtime, execution)
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
                            "unknown child process {child_process_id}"
                        ))
                    })?;
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

    pub(crate) fn close_javascript_child_process_stdin(
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
    if !path_is_within_root(&normalized_entrypoint, &sandbox_root) {
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
            '|'
                | '&'
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

#[derive(Deserialize)]
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

pub(crate) fn javascript_sync_rpc_arg_str<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a str, SidecarError> {
    args.get(index)
        .and_then(Value::as_str)
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} must be a string argument")))
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
        "dns.lookup" | "dns.resolve" | "dns.resolve4" | "dns.resolve6" => {
            service_javascript_dns_sync_rpc(bridge, vm_id, dns, request)
        }
        "net.fetch" => service_javascript_fetch_sync_rpc(bridge, vm_id, request),
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
        .map_err(|error| {
            SidecarError::Execution(format!("ERR_AGENT_OS_NODE_SYNC_RPC: {error}"))
        })
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
