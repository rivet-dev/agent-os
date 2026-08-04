use super::super::*;

const PYTHON_SOCKET_DEFAULT_RECV: usize = 65536;
const PYTHON_SOCKET_MAX_RECV: usize = 4 * 1024 * 1024;

fn python_socket_host(request: &PythonVfsRpcRequest) -> Result<String, SidecarError> {
    request
        .hostname
        .clone()
        .ok_or_else(|| SidecarError::InvalidState(String::from("python socket op requires a host")))
}

fn python_socket_port(request: &PythonVfsRpcRequest) -> Result<u16, SidecarError> {
    request
        .port
        .ok_or_else(|| SidecarError::InvalidState(String::from("python socket op requires a port")))
}

#[derive(Debug)]
struct PythonSocketPayload {
    bytes: Vec<u8>,
    _reservation: Reservation,
}

fn python_socket_payload(
    request: &PythonVfsRpcRequest,
    resources: &ResourceLedger,
) -> Result<PythonSocketPayload, SidecarError> {
    decode_python_socket_payload(request.body_base64.as_deref(), resources)
}

fn decode_python_socket_payload(
    body: Option<&str>,
    resources: &ResourceLedger,
) -> Result<PythonSocketPayload, SidecarError> {
    let Some(body) = body else {
        return Ok(PythonSocketPayload {
            bytes: Vec::new(),
            _reservation: resources
                .reserve(ResourceClass::BufferedBytes, 0)
                .map_err(SidecarError::from)?,
        });
    };
    let padding = body
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .take(2)
        .count();
    let capacity = base64::decoded_len_estimate(body.len()).saturating_sub(padding);
    let mut reservation = resources
        .reserve(ResourceClass::BufferedBytes, capacity)
        .map_err(SidecarError::from)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|error| {
            SidecarError::InvalidState(format!("invalid base64 python socket payload: {error}"))
        })?;
    if capacity > bytes.len() {
        drop(
            reservation
                .split(capacity - bytes.len())
                .expect("decoded payload cannot exceed its reserved estimate"),
        );
    }
    Ok(PythonSocketPayload {
        bytes,
        _reservation: reservation,
    })
}

fn python_socket_recv_len(request: &PythonVfsRpcRequest) -> usize {
    request
        .max_buffer
        .unwrap_or(PYTHON_SOCKET_DEFAULT_RECV)
        .clamp(1, PYTHON_SOCKET_MAX_RECV)
}

fn python_socket_wait_timeout(request: &PythonVfsRpcRequest, limits: ReactorIoLimits) -> Duration {
    request
        .timeout_ms
        .map_or(limits.operation_deadline, |timeout_ms| {
            Duration::from_millis(timeout_ms).min(limits.operation_deadline)
        })
}

pub(in crate::execution) fn python_socket_id(
    request: &PythonVfsRpcRequest,
) -> Result<u64, SidecarError> {
    request.socket_id.ok_or_else(|| {
        SidecarError::InvalidState(String::from("python socket op requires socketId"))
    })
}

fn python_socket_missing_error(socket_id: u64) -> SidecarError {
    SidecarError::Execution(format!("EBADF: unknown python socket {socket_id}"))
}

fn python_socket_backend_missing_error(socket_id: u64) -> SidecarError {
    SidecarError::InvalidState(format!(
        "ERR_AGENTOS_CAPABILITY_BACKEND_MISSING: Python socket {socket_id} lost its shared backend"
    ))
}

fn consume_python_tcp_pending_read(
    pending_read: &mut Option<PythonTcpReadBuffer>,
    max: usize,
    resources: &ResourceLedger,
) -> Result<Option<PythonSocketImmediate>, SidecarError> {
    let Some(pending) = pending_read.as_mut() else {
        return Ok(None);
    };
    let (data_base64, response_reservation, consumed_all) = {
        let end = pending.offset.saturating_add(max).min(pending.data.len());
        let (data_base64, response_reservation) =
            encode_python_socket_bytes(&pending.data[pending.offset..end], resources)?;
        pending.offset = end;
        (data_base64, response_reservation, end == pending.data.len())
    };
    if consumed_all {
        *pending_read = None;
    }
    Ok(Some(PythonSocketImmediate {
        payload: PythonVfsRpcResponsePayload::SocketReceived {
            data_base64,
            closed: false,
            timed_out: false,
        },
        _response_reservation: response_reservation,
    }))
}

fn python_tcp_event_response(
    event: Option<JavascriptTcpSocketEvent>,
    pending_read: &mut Option<PythonTcpReadBuffer>,
    max: usize,
    resources: &ResourceLedger,
) -> Result<PythonSocketResponse, SidecarError> {
    match event {
        Some(JavascriptTcpSocketEvent::Data {
            bytes,
            reservation,
            source_reservations,
        }) => {
            let end = max.min(bytes.len());
            let (data_base64, response_reservation) =
                encode_python_socket_bytes(&bytes[..end], resources)?;
            if end < bytes.len() {
                *pending_read = Some(PythonTcpReadBuffer {
                    data: bytes,
                    offset: end,
                    _reservation: reservation,
                    _source_reservations: source_reservations,
                });
            }
            Ok(PythonSocketResponse::Charged(PythonSocketImmediate {
                payload: PythonVfsRpcResponsePayload::SocketReceived {
                    data_base64,
                    closed: false,
                    timed_out: false,
                },
                _response_reservation: response_reservation,
            }))
        }
        Some(JavascriptTcpSocketEvent::End | JavascriptTcpSocketEvent::Close { .. }) => Ok(
            PythonSocketResponse::Uncharged(PythonVfsRpcResponsePayload::SocketReceived {
                data_base64: String::new(),
                closed: true,
                timed_out: false,
            }),
        ),
        Some(JavascriptTcpSocketEvent::Error { code, message }) => {
            let code = code.unwrap_or_else(|| String::from("EIO"));
            Err(SidecarError::Execution(format!("{code}: {message}")))
        }
        None => Ok(PythonSocketResponse::Uncharged(
            PythonVfsRpcResponsePayload::SocketReceived {
                data_base64: String::new(),
                closed: false,
                timed_out: true,
            },
        )),
    }
}

fn python_udp_event_response(
    event: Option<JavascriptUdpSocketEvent>,
    max: usize,
    resources: &ResourceLedger,
) -> Result<PythonSocketResponse, SidecarError> {
    match event {
        Some(JavascriptUdpSocketEvent::Message {
            data, remote_addr, ..
        }) => {
            let (data_base64, response_reservation) =
                encode_python_socket_bytes(&data[..max.min(data.len())], resources)?;
            Ok(PythonSocketResponse::Charged(PythonSocketImmediate {
                payload: PythonVfsRpcResponsePayload::UdpReceived {
                    data_base64,
                    host: remote_addr.ip().to_string(),
                    port: remote_addr.port(),
                    timed_out: false,
                },
                _response_reservation: response_reservation,
            }))
        }
        Some(JavascriptUdpSocketEvent::Error { code, message }) => {
            let code = code.unwrap_or_else(|| String::from("EIO"));
            Err(SidecarError::Execution(format!("{code}: {message}")))
        }
        None => Ok(PythonSocketResponse::Uncharged(
            PythonVfsRpcResponsePayload::UdpReceived {
                data_base64: String::new(),
                host: String::new(),
                port: 0,
                timed_out: true,
            },
        )),
    }
}

fn encode_python_socket_bytes(
    bytes: &[u8],
    resources: &ResourceLedger,
) -> Result<(String, Reservation), SidecarError> {
    let encoded_len = base64::encoded_len(bytes.len(), true).ok_or_else(|| {
        SidecarError::Execution(String::from(
            "ERR_AGENTOS_RESOURCE_LIMIT: Python socket response length overflowed usize",
        ))
    })?;
    let reservation = resources
        .reserve(ResourceClass::BufferedBytes, encoded_len)
        .map_err(SidecarError::from)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    debug_assert_eq!(encoded.len(), encoded_len);
    Ok((encoded, reservation))
}

enum PythonSocketOp {
    Immediate(PythonVfsRpcResponsePayload),
    Charged(PythonSocketImmediate),
    Deferred,
    Wait(PythonSocketWait),
}

struct PythonSocketImmediate {
    payload: PythonVfsRpcResponsePayload,
    _response_reservation: Reservation,
}

enum PythonSocketResponse {
    Uncharged(PythonVfsRpcResponsePayload),
    Charged(PythonSocketImmediate),
}

struct PythonSocketWait {
    source: PythonSocketWaitSource,
    timeout: Duration,
    task_class: agentos_runtime::TaskClass,
}

enum PythonSocketWaitSource {
    Notify(Arc<tokio::sync::Notify>),
}

fn python_socket_completion_dropped_error() -> SidecarError {
    SidecarError::Execution(String::from(
        "EPIPE: Python socket task stopped before command completion",
    ))
}

fn respond_python_socket_async(
    responder: &PythonVfsRpcResponder,
    request_id: u64,
    response: Result<PythonVfsRpcResponsePayload, SidecarError>,
) {
    let result = match response {
        Ok(payload) => responder.respond_success(request_id, payload),
        Err(error) => {
            responder.respond_error(request_id, "ERR_AGENTOS_PYTHON_VFS_RPC", error.to_string())
        }
    };
    if let Err(error) = result {
        eprintln!(
            "ERR_AGENTOS_PYTHON_SOCKET_RESPONSE: async Python socket response {request_id} failed: {error}"
        );
    }
}

trait PythonSocketConnectResponder {
    fn respond(
        &self,
        request_id: u64,
        response: Result<PythonVfsRpcResponsePayload, SidecarError>,
    ) -> Result<(), SidecarError>;
}

impl PythonSocketConnectResponder for PythonVfsRpcResponder {
    fn respond(
        &self,
        request_id: u64,
        response: Result<PythonVfsRpcResponsePayload, SidecarError>,
    ) -> Result<(), SidecarError> {
        respond_owned_python_rpc(self, request_id, response)
    }
}

fn respond_python_socket_connect_result<R: PythonSocketConnectResponder>(
    responder: &R,
    request_id: u64,
    response: Result<PythonVfsRpcResponsePayload, SidecarError>,
) -> Result<(), SidecarError> {
    responder.respond(request_id, response)
}

fn python_socket_kind_error(op: &str, expected: &str) -> SidecarError {
    SidecarError::Execution(format!(
        "EOPNOTSUPP: python socket {op} requires a {expected} socket"
    ))
}

struct OwnedPythonVmRegistry {
    vm_id: String,
    vm: VmHandle,
}

impl OwnedPythonVmRegistry {
    fn contains_key(&self, vm_id: &str) -> bool {
        self.vm_id == vm_id
    }

    fn get(&self, vm_id: &str) -> Option<std::cell::Ref<'_, VmState>> {
        self.contains_key(vm_id).then(|| self.vm.borrow())
    }

    fn get_mut(&self, vm_id: &str) -> Option<std::cell::RefMut<'_, VmState>> {
        self.contains_key(vm_id).then(|| self.vm.borrow_mut())
    }

    fn handle(&self, vm_id: &str) -> Option<VmHandle> {
        self.contains_key(vm_id).then(|| self.vm.clone())
    }
}

struct OwnedPythonSocketService<B> {
    bridge: SharedBridge<B>,
    vms: OwnedPythonVmRegistry,
    process_event_sender: tokio::sync::mpsc::Sender<ProcessEventEnvelope>,
    process_event_notify: Arc<tokio::sync::Notify>,
    target: PythonProcessTarget,
    responder: PythonVfsRpcResponder,
}

#[derive(Clone, Debug)]
struct PythonProcessTarget {
    root_process_id: String,
    child_path: Vec<String>,
}

impl PythonProcessTarget {
    fn process<'a>(&self, vm: &'a VmState) -> Option<&'a ActiveProcess> {
        self.process_in_roots(&vm.active_processes)
    }

    fn process_in_roots<'a>(
        &self,
        roots: &'a BTreeMap<String, ActiveProcess>,
    ) -> Option<&'a ActiveProcess> {
        let mut process = roots.get(&self.root_process_id)?;
        for child_id in &self.child_path {
            process = process.child_processes.get(child_id)?;
        }
        Some(process)
    }

    fn process_mut<'a>(&self, vm: &'a mut VmState) -> Option<&'a mut ActiveProcess> {
        self.process_in_roots_mut(&mut vm.active_processes)
    }

    fn process_in_roots_mut<'a>(
        &self,
        roots: &'a mut BTreeMap<String, ActiveProcess>,
    ) -> Option<&'a mut ActiveProcess> {
        let mut process = roots.get_mut(&self.root_process_id)?;
        for child_id in &self.child_path {
            process = process.child_processes.get_mut(child_id)?;
        }
        Some(process)
    }

    fn label(&self) -> String {
        if self.child_path.is_empty() {
            return self.root_process_id.clone();
        }
        format!("{}:{}", self.root_process_id, self.child_path.join(":"))
    }
}

struct DetachedPythonUdpSocket {
    vm: VmHandle,
    target: PythonProcessTarget,
    native_socket_id: String,
    socket: Option<ActiveUdpSocket>,
}

impl DetachedPythonUdpSocket {
    fn socket(&self) -> &ActiveUdpSocket {
        self.socket
            .as_ref()
            .expect("detached Python UDP socket remains owned until guard drop")
    }
}

impl Drop for DetachedPythonUdpSocket {
    fn drop(&mut self) {
        let Some(socket) = self.socket.take() else {
            return;
        };
        let native_socket_id = self.native_socket_id.clone();
        let target = self.target.clone();
        if let Err(error) = self.vm.try_command("restore detached Python UDP socket", |vm| {
            let Some(process) = target.process_mut(vm) else {
                return Ok(());
            };
            if process.udp_sockets.contains_key(&native_socket_id) {
                return Err(SidecarError::InvalidState(format!(
                    "ERR_AGENTOS_PYTHON_SOCKET_RESTORE_CONFLICT: UDP socket {native_socket_id} was replaced while an owned receive was pending"
                )));
            }
            process.udp_sockets.insert(native_socket_id, socket);
            Ok(())
        }) {
            eprintln!(
                "ERR_AGENTOS_PYTHON_SOCKET_RESTORE: failed to restore detached UDP socket: {error}"
            );
        }
    }
}

impl<B> OwnedPythonSocketService<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(in crate::execution) async fn handle_python_socket_rpc_request(
        &mut self,
        vm_id: &str,
        request: PythonVfsRpcRequest,
    ) -> Result<(), SidecarError> {
        if !self.vms.contains_key(vm_id) {
            return Ok(());
        }
        match self.python_socket_op(vm_id, &request).await {
            Ok(PythonSocketOp::Immediate(response)) => {
                self.respond_python_rpc(request.id, Ok(response))
            }
            Ok(PythonSocketOp::Charged(response)) => {
                self.respond_python_rpc(request.id, Ok(response.payload))
            }
            Ok(PythonSocketOp::Deferred) => Ok(()),
            Ok(PythonSocketOp::Wait(wait)) => {
                self.schedule_python_socket_wait(vm_id, request, wait)
            }
            Err(error) => self.respond_python_rpc(request.id, Err(error)),
        }
    }

    #[deny(clippy::await_holding_refcell_ref)]
    async fn python_socket_op(
        &mut self,
        vm_id: &str,
        request: &PythonVfsRpcRequest,
    ) -> Result<PythonSocketOp, SidecarError> {
        match request.method {
            PythonVfsRpcMethod::SocketConnect => {
                let host = python_socket_host(request)?;
                let port = python_socket_port(request)?;
                self.bridge.require_network_access(
                    vm_id,
                    NetworkOperation::Http,
                    format_tcp_resource(&host, port),
                )?;
                let socket_paths = {
                    let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                    build_javascript_socket_path_context(&vm)?
                };
                let resolved = {
                    let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                    resolve_tcp_connect_addr(
                        &self.bridge,
                        &vm.kernel,
                        vm_id,
                        &vm.dns,
                        &host,
                        port,
                        None,
                        &socket_paths,
                    )?
                };
                if !resolved.use_kernel_loopback {
                    return self.defer_python_native_tcp_connect(vm_id, request.id, resolved);
                }
                let mut vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let pending = reserve_capability(&vm.capabilities, CapabilityKind::TcpSocket)?;
                let resources = vm.capabilities.resources();
                let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                let vm = &mut *vm;
                let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
                let process = self
                    .target
                    .process_in_roots_mut(active_processes)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "python socket op for reaped process {}",
                            self.target.label()
                        ))
                    })?;
                let socket = ActiveTcpSocket::connect_kernel_loopback(
                    kernel,
                    process.kernel_pid,
                    resolved,
                    None,
                    None,
                    None,
                    &socket_paths,
                    resources,
                    process.runtime_context.clone(),
                    reactor_io_limits(&process.limits),
                )?;
                let native_socket_id = process.allocate_tcp_socket_id();
                let capability_key = NativeCapabilityKey::TcpSocket(native_socket_id.clone());
                let identity = match commit_process_capability(
                    process,
                    pending,
                    capability_key.clone(),
                    native_socket_id.clone(),
                    socket.kernel_socket_id,
                ) {
                    Ok(identity) => identity,
                    Err(error) => {
                        if let Err(close_error) = socket.close(kernel, process.kernel_pid) {
                            eprintln!(
                                "ERR_AGENTOS_PYTHON_SOCKET_CLOSE: TCP connect rollback failed: {close_error}"
                            );
                        }
                        return Err(error);
                    }
                };
                socket
                    .set_fairness_identity(process.capability_fairness_identity(&capability_key))?;
                socket.retain_description_lease(
                    process
                        .shared_capability_lease(&capability_key)
                        .expect("committed Python TCP capability lease"),
                );
                register_kernel_readiness_target(
                    &kernel_readiness,
                    socket.kernel_socket_id,
                    None,
                    Some(Arc::clone(&socket.read_event_notify)),
                    process.capability_readiness_identity(&capability_key),
                    native_socket_id.clone(),
                    KernelSocketReadinessEvent::Data,
                );
                process.tcp_sockets.insert(native_socket_id.clone(), socket);
                let python_socket_id = process.next_python_socket_id;
                process.next_python_socket_id = process.next_python_socket_id.wrapping_add(1);
                process.python_sockets.insert(
                    python_socket_id,
                    PythonHostSocket::Tcp {
                        socket_id: native_socket_id,
                        pending_read: None,
                    },
                );
                debug_assert!(process.capability_leases.contains_key(&capability_key));
                let _ = identity;
                Ok(PythonSocketOp::Immediate(
                    PythonVfsRpcResponsePayload::SocketCreated {
                        socket_id: python_socket_id,
                    },
                ))
            }
            PythonVfsRpcMethod::SocketSend => {
                let python_socket_id = python_socket_id(request)?;
                let mut vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let vm = &mut *vm;
                let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
                let process = self
                    .target
                    .process_in_roots_mut(active_processes)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "python socket op for reaped process {}",
                            self.target.label()
                        ))
                    })?;
                let data = python_socket_payload(request, process.runtime_context.resources())?;
                let native_socket_id = match process.python_sockets.get(&python_socket_id) {
                    Some(PythonHostSocket::Tcp { socket_id, .. }) => socket_id.clone(),
                    Some(PythonHostSocket::Udp { .. }) => {
                        return Err(python_socket_kind_error("send", "TCP"));
                    }
                    None => return Err(python_socket_missing_error(python_socket_id)),
                };
                process.validate_capability_alias(
                    &NativeCapabilityKey::TcpSocket(native_socket_id.clone()),
                    CapabilityKind::TcpSocket,
                )?;
                let socket = process
                    .tcp_sockets
                    .get(&native_socket_id)
                    .ok_or_else(|| python_socket_backend_missing_error(python_socket_id))?;
                if socket.kernel_socket_id.is_some() {
                    let bytes_sent = socket.write_all(kernel, process.kernel_pid, &data.bytes)?;
                    return Ok(PythonSocketOp::Immediate(
                        PythonVfsRpcResponsePayload::SocketSent { bytes_sent },
                    ));
                }
                let response = socket.begin_plain_write(&data.bytes)?;
                let (runtime, responder) = self.python_socket_async_context(vm_id)?;
                let request_id = request.id;
                runtime
                    .spawn(agentos_runtime::TaskClass::Socket, async move {
                        let response = match response.await {
                            Ok(Ok(value)) => value
                                .as_u64()
                                .and_then(|value| usize::try_from(value).ok())
                                .map(|bytes_sent| PythonVfsRpcResponsePayload::SocketSent {
                                    bytes_sent,
                                })
                                .ok_or_else(|| {
                                    SidecarError::InvalidState(String::from(
                                        "plain TCP transport returned an invalid byte count",
                                    ))
                                }),
                            Ok(Err(error)) => Err(SidecarError::Execution(format!(
                                "{}: {}",
                                error.code, error.message
                            ))),
                            Err(_) => Err(python_socket_completion_dropped_error()),
                        };
                        respond_python_socket_async(&responder, request_id, response);
                    })
                    .map_err(SidecarError::from)?;
                Ok(PythonSocketOp::Deferred)
            }
            PythonVfsRpcMethod::SocketRecv => {
                let max = python_socket_recv_len(request);
                let python_socket_id = python_socket_id(request)?;
                let mut vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let vm = &mut *vm;
                let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
                let process = self
                    .target
                    .process_in_roots_mut(active_processes)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "python socket op for reaped process {}",
                            self.target.label()
                        ))
                    })?;
                let resources = Arc::clone(process.runtime_context.resources());
                let mut handle = process
                    .python_sockets
                    .remove(&python_socket_id)
                    .ok_or_else(|| python_socket_missing_error(python_socket_id))?;
                let result = (|| {
                    let PythonHostSocket::Tcp {
                        socket_id,
                        pending_read,
                    } = &mut handle
                    else {
                        return Err(python_socket_kind_error("recv", "TCP"));
                    };
                    process.validate_capability_alias(
                        &NativeCapabilityKey::TcpSocket(socket_id.clone()),
                        CapabilityKind::TcpSocket,
                    )?;
                    if let Some(response) =
                        consume_python_tcp_pending_read(pending_read, max, &resources)?
                    {
                        return Ok(PythonSocketOp::Charged(response));
                    }
                    let socket = process
                        .tcp_sockets
                        .get_mut(socket_id)
                        .ok_or_else(|| python_socket_backend_missing_error(python_socket_id))?;
                    socket.set_application_read_interest(true)?;
                    let event = socket.poll(kernel, process.kernel_pid, Duration::ZERO, false)?;
                    let wait_timeout = python_socket_wait_timeout(request, socket.reactor_limits);
                    if event.is_none() && !wait_timeout.is_zero() {
                        return Ok(PythonSocketOp::Wait(PythonSocketWait {
                            source: PythonSocketWaitSource::Notify(Arc::clone(
                                &socket.read_event_notify,
                            )),
                            timeout: wait_timeout,
                            task_class: agentos_runtime::TaskClass::Socket,
                        }));
                    }
                    python_tcp_event_response(event, pending_read, max, &resources).map(
                        |response| match response {
                            PythonSocketResponse::Uncharged(response) => {
                                PythonSocketOp::Immediate(response)
                            }
                            PythonSocketResponse::Charged(response) => {
                                PythonSocketOp::Charged(response)
                            }
                        },
                    )
                })();
                process.python_sockets.insert(python_socket_id, handle);
                result
            }
            PythonVfsRpcMethod::SocketClose => {
                self.remove_python_socket(vm_id, request)?;
                Ok(PythonSocketOp::Immediate(
                    PythonVfsRpcResponsePayload::Empty,
                ))
            }
            PythonVfsRpcMethod::UdpCreate => {
                let mut vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let pending = reserve_capability(&vm.capabilities, CapabilityKind::UdpSocket)?;
                let resources = vm.capabilities.resources();
                let process = self.target.process_mut(&mut vm).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "python socket op for reaped process {}",
                        self.target.label()
                    ))
                })?;
                let mut socket = ActiveUdpSocket::new_native(
                    JavascriptUdpFamily::Ipv4,
                    resources,
                    process.runtime_context.clone(),
                    reactor_io_limits(&process.limits),
                )?;
                let native_socket_id = process.allocate_udp_socket_id();
                let capability_key = NativeCapabilityKey::UdpSocket(native_socket_id.clone());
                commit_process_capability(
                    process,
                    pending,
                    capability_key.clone(),
                    native_socket_id.clone(),
                    None,
                )?;
                socket.set_fairness_identity(process.capability_fairness_identity(&capability_key));
                socket.retain_description_lease(
                    process
                        .shared_capability_lease(&capability_key)
                        .expect("committed Python UDP capability lease"),
                );
                process.udp_sockets.insert(native_socket_id.clone(), socket);
                let python_socket_id = process.next_python_socket_id;
                process.next_python_socket_id = process.next_python_socket_id.wrapping_add(1);
                process.python_sockets.insert(
                    python_socket_id,
                    PythonHostSocket::Udp {
                        socket_id: native_socket_id,
                    },
                );
                Ok(PythonSocketOp::Immediate(
                    PythonVfsRpcResponsePayload::SocketCreated {
                        socket_id: python_socket_id,
                    },
                ))
            }
            PythonVfsRpcMethod::UdpSendto => {
                let host = python_socket_host(request)?;
                let port = python_socket_port(request)?;
                self.bridge.require_network_access(
                    vm_id,
                    NetworkOperation::Http,
                    format_tcp_resource(&host, port),
                )?;
                let socket_paths = {
                    let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                    build_javascript_socket_path_context(&vm)?
                };
                let python_socket_id = python_socket_id(request)?;
                let send = {
                    let mut vm = self
                        .vms
                        .get_mut(vm_id)
                        .ok_or_else(|| missing_vm_error(vm_id))?;
                    let vm = &mut *vm;
                    let (kernel, dns, active_processes) =
                        (&mut vm.kernel, &vm.dns, &mut vm.active_processes);
                    let process = self
                        .target
                        .process_in_roots_mut(active_processes)
                        .ok_or_else(|| {
                            SidecarError::InvalidState(format!(
                                "python socket op for reaped process {}",
                                self.target.label()
                            ))
                        })?;
                    let data = python_socket_payload(request, process.runtime_context.resources())?;
                    let native_socket_id = match process.python_sockets.get(&python_socket_id) {
                        Some(PythonHostSocket::Udp { socket_id }) => socket_id.clone(),
                        Some(PythonHostSocket::Tcp { .. }) => {
                            return Err(python_socket_kind_error("sendto", "UDP"));
                        }
                        None => return Err(python_socket_missing_error(python_socket_id)),
                    };
                    process.validate_capability_alias(
                        &NativeCapabilityKey::UdpSocket(native_socket_id.clone()),
                        CapabilityKind::UdpSocket,
                    )?;
                    let socket = process
                        .udp_sockets
                        .get_mut(&native_socket_id)
                        .ok_or_else(|| python_socket_backend_missing_error(python_socket_id))?;
                    socket.send_to(ActiveUdpSendToRequest {
                        bridge: &self.bridge,
                        kernel,
                        kernel_pid: process.kernel_pid,
                        vm_id,
                        dns,
                        host: &host,
                        port,
                        context: &socket_paths,
                        contents: &data.bytes,
                    })?
                };
                let bytes_sent = await_udp_send_result(send).await?;
                Ok(PythonSocketOp::Immediate(
                    PythonVfsRpcResponsePayload::SocketSent { bytes_sent },
                ))
            }
            PythonVfsRpcMethod::UdpRecvfrom => {
                let max = python_socket_recv_len(request);
                let python_socket_id = python_socket_id(request)?;
                let vm_handle = self
                    .vms
                    .handle(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let (resources, kernel_pid, native_socket_id, active_socket) = {
                    let mut vm = self
                        .vms
                        .get_mut(vm_id)
                        .ok_or_else(|| missing_vm_error(vm_id))?;
                    let process = self.target.process_mut(&mut vm).ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "python socket op for reaped process {}",
                            self.target.label()
                        ))
                    })?;
                    let resources = Arc::clone(process.runtime_context.resources());
                    let native_socket_id = match process.python_sockets.get(&python_socket_id) {
                        Some(PythonHostSocket::Udp { socket_id }) => socket_id.clone(),
                        Some(PythonHostSocket::Tcp { .. }) => {
                            return Err(python_socket_kind_error("recvfrom", "UDP"));
                        }
                        None => return Err(python_socket_missing_error(python_socket_id)),
                    };
                    process.validate_capability_alias(
                        &NativeCapabilityKey::UdpSocket(native_socket_id.clone()),
                        CapabilityKind::UdpSocket,
                    )?;
                    let active_socket = process
                        .udp_sockets
                        .remove(&native_socket_id)
                        .ok_or_else(|| python_socket_backend_missing_error(python_socket_id))?;
                    (
                        resources,
                        process.kernel_pid,
                        native_socket_id,
                        active_socket,
                    )
                };
                let socket = DetachedPythonUdpSocket {
                    vm: vm_handle,
                    target: self.target.clone(),
                    native_socket_id,
                    socket: Some(active_socket),
                };
                let kernel_ready = {
                    let mut vm = self
                        .vms
                        .get_mut(vm_id)
                        .ok_or_else(|| missing_vm_error(vm_id))?;
                    socket
                        .socket()
                        .poll_kernel_ready(&mut vm.kernel, kernel_pid, Duration::ZERO)?
                };
                let event = if kernel_ready {
                    let turn = socket.socket().acquire_poll_fair_turn().await?;
                    let mut vm = self
                        .vms
                        .get_mut(vm_id)
                        .ok_or_else(|| missing_vm_error(vm_id))?;
                    socket.socket().consume_ready_kernel_datagram(
                        &mut vm.kernel,
                        kernel_pid,
                        turn,
                    )?
                } else {
                    socket.socket().poll_native(Duration::ZERO).await?
                };
                let wait_timeout =
                    python_socket_wait_timeout(request, socket.socket().reactor_limits);
                if event.is_none() && !wait_timeout.is_zero() {
                    return Ok(PythonSocketOp::Wait(PythonSocketWait {
                        source: PythonSocketWaitSource::Notify(Arc::clone(
                            &socket.socket().read_event_notify,
                        )),
                        timeout: wait_timeout,
                        task_class: agentos_runtime::TaskClass::Udp,
                    }));
                }
                python_udp_event_response(event, max, &resources).map(|response| match response {
                    PythonSocketResponse::Uncharged(response) => {
                        PythonSocketOp::Immediate(response)
                    }
                    PythonSocketResponse::Charged(response) => PythonSocketOp::Charged(response),
                })
            }
            _ => Err(SidecarError::InvalidState(String::from(
                "non-socket python RPC reached the socket dispatcher unexpectedly",
            ))),
        }
    }

    fn defer_python_native_tcp_connect(
        &mut self,
        vm_id: &str,
        request_id: u64,
        resolved: ResolvedTcpConnectAddr,
    ) -> Result<PythonSocketOp, SidecarError> {
        debug_assert!(!resolved.use_kernel_loopback);
        let (runtime, resources, limits, pending_capability, native_socket_id, python_socket_id) = {
            let mut vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let pending_capability =
                reserve_capability(&vm.capabilities, CapabilityKind::TcpSocket)?;
            let resources = vm.capabilities.resources();
            let process = self.target.process_mut(&mut vm).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "python socket connect for reaped process {}",
                    self.target.label()
                ))
            })?;
            let native_socket_id = process.allocate_tcp_socket_id();
            let python_socket_id = process.next_python_socket_id;
            process.next_python_socket_id = process.next_python_socket_id.wrapping_add(1);
            (
                process.runtime_context.clone(),
                resources,
                reactor_io_limits(&process.limits),
                pending_capability,
                native_socket_id,
                python_socket_id,
            )
        };
        let task_runtime = runtime.clone();
        let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        let connection_id = vm.connection_id.clone();
        let session_id = vm.session_id.clone();
        let vm_id = vm_id.to_owned();
        let process_id = self.target.root_process_id.clone();
        let child_path = self.target.child_path.clone();
        let sender = self.process_event_sender.clone();
        let responder = self.responder.clone();
        let event_notify = Arc::clone(&self.process_event_notify);
        runtime
            .spawn(agentos_runtime::TaskClass::Socket, async move {
                let result = match tokio::time::timeout(
                    limits.operation_deadline,
                    tokio::net::TcpStream::connect(resolved.actual_addr),
                )
                .await
                {
                    Ok(Ok(stream)) => {
                        let built = stream
                            .local_addr()
                            .map_err(sidecar_net_error)
                            .and_then(|local_addr| {
                                stream
                                    .into_std()
                                    .map_err(sidecar_net_error)
                                    .and_then(|stream| {
                                        ActiveTcpSocket::from_stream(
                                            stream,
                                            None,
                                            local_addr,
                                            resolved.guest_remote_addr,
                                            resources,
                                            task_runtime,
                                            limits,
                                        )
                                    })
                            });
                        match built {
                            Ok(socket) => Ok(PendingPythonTcpConnect {
                                native_socket_id,
                                python_socket_id,
                                socket,
                                pending_capability,
                            }),
                            Err(error) => Err(deferred_connect_error(error)),
                        }
                    }
                    Ok(Err(error)) => Err(deferred_connect_error(sidecar_net_error(error))),
                    Err(_) => Err(crate::state::DeferredRpcError {
                        code: String::from("ETIMEDOUT"),
                        message: format!(
                            "TCP connect exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                            limits.operation_deadline.as_millis()
                        ),
                    }),
                };
                let envelope = ProcessEventEnvelope {
                    connection_id,
                    session_id,
                    vm_id,
                    child_path,
                    process_id,
                    event: ActiveExecutionEvent::PythonSocketConnectCompletion(Box::new(
                        PythonSocketConnectCompletion { request_id, result },
                    )),
                };
                if sender.send(envelope).await.is_err() {
                    respond_python_socket_async(
                        &responder,
                        request_id,
                        Err(SidecarError::InvalidState(String::from(
                            "ERR_AGENTOS_PROCESS_EVENT_CHANNEL_CLOSED: Python TCP connect completion could not be delivered",
                        ))),
                    );
                } else {
                    event_notify.notify_one();
                }
            })
            .map_err(SidecarError::from)?;
        Ok(PythonSocketOp::Deferred)
    }

    fn python_socket_async_context(
        &self,
        vm_id: &str,
    ) -> Result<(agentos_runtime::RuntimeContext, PythonVfsRpcResponder), SidecarError> {
        let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        let process = self.target.process(&vm).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "python socket op for reaped process {}",
                self.target.label()
            ))
        })?;
        Ok((
            vm.runtime_context.clone(),
            process.execution.python_vfs_rpc_responder()?,
        ))
    }

    fn schedule_python_socket_wait(
        &self,
        vm_id: &str,
        mut request: PythonVfsRpcRequest,
        wait: PythonSocketWait,
    ) -> Result<(), SidecarError> {
        let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        let process = self.target.process(&vm).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "python socket wait for reaped process {}",
                self.target.label()
            ))
        })?;
        let runtime = process.runtime_context.clone();
        drop(vm);
        let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        let connection_id = vm.connection_id.clone();
        let session_id = vm.session_id.clone();
        let vm_id = vm_id.to_owned();
        let process_id = self.target.root_process_id.clone();
        let child_path = self.target.child_path.clone();
        let sender = self.process_event_sender.clone();
        let responder = self.responder.clone();
        let event_notify = Arc::clone(&self.process_event_notify);
        request.timeout_ms = Some(0);
        let request_id = request.id;
        let cancellation = runtime.clone();
        runtime
            .spawn(wait.task_class, async move {
                let readiness = async move {
                    match wait.source {
                        PythonSocketWaitSource::Notify(notify) => {
                            let _ = tokio::time::timeout(wait.timeout, notify.notified()).await;
                        }
                    }
                };
                tokio::select! {
                    () = readiness => {}
                    () = cancellation.admission_closed() => return,
                }
                if !cancellation.admission_is_open() {
                    return;
                }
                let envelope = ProcessEventEnvelope {
                    connection_id,
                    session_id,
                    vm_id,
                    child_path,
                    process_id,
                    event: ActiveExecutionEvent::PythonVfsRpcRequest(Box::new(request)),
                };
                if sender.send(envelope).await.is_err() {
                    respond_python_socket_async(
                        &responder,
                        request_id,
                        Err(SidecarError::InvalidState(String::from(
                            "ERR_AGENTOS_PROCESS_EVENT_CHANNEL_CLOSED: Python socket readiness retry could not be delivered",
                        ))),
                    );
                } else {
                    event_notify.notify_one();
                }
            })
            .map_err(SidecarError::from)?;
        Ok(())
    }

    fn remove_python_socket(
        &mut self,
        vm_id: &str,
        request: &PythonVfsRpcRequest,
    ) -> Result<(), SidecarError> {
        let Some(socket_id) = request.socket_id else {
            return Ok(());
        };
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let vm = &mut *vm;
        let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
        let Some(process) = self.target.process_in_roots_mut(active_processes) else {
            return Ok(());
        };
        let Some(socket) = process.python_sockets.get(&socket_id) else {
            return Ok(());
        };
        match socket {
            PythonHostSocket::Tcp { socket_id, .. } => process.validate_capability_alias(
                &NativeCapabilityKey::TcpSocket(socket_id.clone()),
                CapabilityKind::TcpSocket,
            )?,
            PythonHostSocket::Udp { socket_id } => process.validate_capability_alias(
                &NativeCapabilityKey::UdpSocket(socket_id.clone()),
                CapabilityKind::UdpSocket,
            )?,
        }
        let socket = process
            .python_sockets
            .remove(&socket_id)
            .expect("validated Python socket alias must remain present");
        match socket {
            PythonHostSocket::Tcp {
                socket_id: native_socket_id,
                ..
            } => {
                if let Some(socket) = process.tcp_sockets.remove(&native_socket_id) {
                    release_tcp_socket_handle(
                        process,
                        &native_socket_id,
                        socket,
                        kernel,
                        &kernel_readiness,
                    );
                }
            }
            PythonHostSocket::Udp {
                socket_id: native_socket_id,
            } => {
                if let Some(socket) = process.udp_sockets.remove(&native_socket_id) {
                    release_udp_socket_handle(
                        process,
                        &native_socket_id,
                        socket,
                        kernel,
                        &kernel_readiness,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(in crate::execution) fn respond_python_rpc(
        &mut self,
        request_id: u64,
        response: Result<PythonVfsRpcResponsePayload, SidecarError>,
    ) -> Result<(), SidecarError> {
        respond_owned_python_rpc(&self.responder, request_id, response)
    }
}

pub(in crate::execution) async fn service_owned_python_socket_rpc_request<B>(
    bridge: SharedBridge<B>,
    vm: VmHandle,
    vm_id: String,
    process_id: String,
    child_path: Vec<String>,
    responder: PythonVfsRpcResponder,
    request: PythonVfsRpcRequest,
    process_event_sender: tokio::sync::mpsc::Sender<ProcessEventEnvelope>,
    process_event_notify: Arc<tokio::sync::Notify>,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let mut service = OwnedPythonSocketService {
        bridge,
        vms: OwnedPythonVmRegistry {
            vm_id: vm_id.clone(),
            vm,
        },
        process_event_sender,
        process_event_notify,
        target: PythonProcessTarget {
            root_process_id: process_id,
            child_path,
        },
        responder,
    };
    service
        .handle_python_socket_rpc_request(&vm_id, request)
        .await
}

pub(crate) async fn service_owned_python_socket_connect_completion(
    vm: VmHandle,
    vm_id: String,
    process_id: String,
    child_path: Vec<String>,
    responder: PythonVfsRpcResponder,
    completion: PythonSocketConnectCompletion,
) -> Result<(), SidecarError> {
    let request_id = completion.request_id;
    let connected = match completion.result {
        Ok(connected) => connected,
        Err(error) => {
            return respond_python_socket_connect_result(
                &responder,
                request_id,
                Err(SidecarError::Execution(format!(
                    "{}: {}",
                    error.code, error.message
                ))),
            );
        }
    };
    let target = PythonProcessTarget {
        root_process_id: process_id,
        child_path,
    };
    let result = vm.try_command("complete owned Python TCP connect", move |state| {
        let kernel_readiness = Arc::clone(&state.kernel_socket_readiness);
        let state = &mut *state;
        let (kernel, active_processes) = (&mut state.kernel, &mut state.active_processes);
        let process = target
            .process_in_roots_mut(active_processes)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "ERR_AGENTOS_STALE_PROCESS_EVENT: Python TCP completion targeted reaped process {} in VM {vm_id}",
                    target.label()
                ))
            })?;
        let PendingPythonTcpConnect {
            native_socket_id,
            python_socket_id,
            socket,
            pending_capability,
        } = connected;
        let capability_key = NativeCapabilityKey::TcpSocket(native_socket_id.clone());
        if let Err(error) = commit_process_capability(
            process,
            pending_capability,
            capability_key.clone(),
            native_socket_id.clone(),
            socket.kernel_socket_id,
        ) {
            if let Err(close_error) = socket.close(kernel, process.kernel_pid) {
                eprintln!(
                    "ERR_AGENTOS_PYTHON_SOCKET_CLOSE: deferred TCP connect rollback failed: {close_error}"
                );
            }
            return Err(error);
        }
        if let Err(error) =
            socket.set_fairness_identity(process.capability_fairness_identity(&capability_key))
        {
            if let Err(release_error) = process.release_capability(&capability_key) {
                eprintln!(
                    "ERR_AGENTOS_CAPABILITY_RELEASE: deferred Python TCP rollback failed: {release_error}"
                );
            }
            if let Err(close_error) = socket.close(kernel, process.kernel_pid) {
                eprintln!(
                    "ERR_AGENTOS_PYTHON_SOCKET_CLOSE: deferred TCP fairness rollback failed: {close_error}"
                );
            }
            return Err(error);
        }
        socket.retain_description_lease(
            process
                .shared_capability_lease(&capability_key)
                .expect("committed deferred Python TCP capability lease"),
        );
        register_kernel_readiness_target(
            &kernel_readiness,
            socket.kernel_socket_id,
            None,
            Some(Arc::clone(&socket.read_event_notify)),
            process.capability_readiness_identity(&capability_key),
            native_socket_id.clone(),
            KernelSocketReadinessEvent::Data,
        );
        process.tcp_sockets.insert(native_socket_id.clone(), socket);
        process.python_sockets.insert(
            python_socket_id,
            PythonHostSocket::Tcp {
                socket_id: native_socket_id,
                pending_read: None,
            },
        );
        debug_assert!(process.capability_leases.contains_key(&capability_key));
        Ok(PythonVfsRpcResponsePayload::SocketCreated {
            socket_id: python_socket_id,
        })
    });
    respond_python_socket_connect_result(&responder, request_id, result)
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(in crate::execution) async fn handle_python_socket_rpc_request(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: PythonVfsRpcRequest,
    ) -> Result<(), SidecarError> {
        let Some(vm) = self.vms.handle(vm_id) else {
            return Ok(());
        };
        let responder = vm.try_read("prepare Python socket RPC", |state| {
            state
                .active_processes
                .get(process_id)
                .map(|process| process.execution.python_vfs_rpc_responder())
        })?;
        let Some(responder) = responder else {
            return Ok(());
        };
        service_owned_python_socket_rpc_request(
            self.bridge.clone(),
            vm,
            vm_id.to_owned(),
            process_id.to_owned(),
            Vec::new(),
            responder?,
            request,
            self.process_event_sender.clone(),
            Arc::clone(&self.process_event_notify),
        )
        .await
    }

    pub(in crate::execution) fn respond_python_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request_id: u64,
        response: Result<PythonVfsRpcResponsePayload, SidecarError>,
    ) -> Result<(), SidecarError> {
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let Some(process) = vm.active_processes.get_mut(process_id) else {
            return Ok(());
        };
        let result = match response {
            Ok(payload) => process
                .execution
                .respond_python_vfs_rpc_success(request_id, payload),
            Err(error) => process.execution.respond_python_vfs_rpc_error(
                request_id,
                "ERR_AGENTOS_PYTHON_VFS_RPC",
                error.to_string(),
            ),
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) if is_broken_pipe_error(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod python_socket_accounting_tests {
    use super::{
        decode_python_socket_payload, encode_python_socket_bytes,
        reserve_plain_socket_write_payload, respond_python_socket_connect_result,
        PythonSocketConnectResponder, PythonVfsRpcResponsePayload, SidecarError,
    };
    use agentos_runtime::accounting::{ResourceClass, ResourceLedger, ResourceLimit};
    use std::cell::RefCell;
    use std::sync::Arc;

    #[derive(Default)]
    struct CapturingConnectResponder {
        response: RefCell<Option<(u64, Result<PythonVfsRpcResponsePayload, SidecarError>)>>,
    }

    impl PythonSocketConnectResponder for CapturingConnectResponder {
        fn respond(
            &self,
            request_id: u64,
            response: Result<PythonVfsRpcResponsePayload, SidecarError>,
        ) -> Result<(), SidecarError> {
            *self.response.borrow_mut() = Some((request_id, response));
            Ok(())
        }
    }

    #[test]
    fn root_socket_connect_success_replies_with_the_created_socket_id() {
        let responder = CapturingConnectResponder::default();
        respond_python_socket_connect_result(
            &responder,
            41,
            Ok(PythonVfsRpcResponsePayload::SocketCreated { socket_id: 73 }),
        )
        .expect("deliver root Python socket success");
        let (request_id, response) = responder
            .response
            .borrow_mut()
            .take()
            .expect("captured root Python socket response");
        assert_eq!(request_id, 41);
        match response.expect("successful root Python socket response") {
            PythonVfsRpcResponsePayload::SocketCreated { socket_id } => {
                assert_eq!(socket_id, 73);
            }
            other => panic!("expected SocketCreated response, received {other:?}"),
        }
    }

    #[test]
    fn deferred_socket_events_preserve_explicit_process_paths() {
        let source = include_str!("sockets.rs");
        let connect = source
            .split("fn defer_python_native_tcp_connect")
            .nth(1)
            .and_then(|source| source.split("fn python_socket_async_context").next())
            .expect("deferred TCP connect source");
        let readiness = source
            .split("fn schedule_python_socket_wait")
            .nth(1)
            .and_then(|source| source.split("fn remove_python_socket").next())
            .expect("socket readiness source");
        for path in [connect, readiness] {
            assert!(path.contains("process_id = self.target.root_process_id.clone()"));
            assert!(path.contains("child_path = self.target.child_path.clone()"));
            assert!(path.contains("ProcessEventEnvelope"));
        }
    }

    #[test]
    fn root_socket_completion_is_owned_supervised_work() {
        let process_events = include_str!("../process_events.rs");
        let root_turn = process_events
            .split("pub(crate) fn pump_process_events_nowait")
            .nth(1)
            .and_then(|source| {
                source
                    .split("pub(crate) fn handle_public_execution_event_nowait")
                    .next()
            })
            .expect("root process-event turn source");
        assert!(root_turn.contains("OwnedPythonSocketCompletionService::new"));
        assert!(root_turn.contains("python_socket_completions.push"));
        assert!(!root_turn.contains("handle_python_socket_connect_completion("));

        let extension_services = include_str!("../../extension_services.rs");
        let supervised = extension_services
            .split("fn prepare_owned_python_socket_completion_service")
            .nth(1)
            .and_then(|source| source.split("pub(crate) fn prepare_owned_child").next())
            .expect("owned Python completion supervisor source");
        assert!(supervised.contains("with_internal_vm_event_admission"));
        assert!(supervised.contains("panic_responder.respond_error"));
        assert!(supervised.contains("completion_responder.respond_error"));
    }

    #[test]
    fn adapter_copies_are_charged_before_decode_encode_and_plain_write() {
        let resources = Arc::new(ResourceLedger::root(
            "python-socket-accounting",
            [
                (
                    ResourceClass::BufferedBytes,
                    ResourceLimit::new(16, "limits.resources.maxSocketBufferedBytes"),
                ),
                (
                    ResourceClass::HandleCommands,
                    ResourceLimit::new(1, "limits.reactor.maxHandleCommands"),
                ),
                (
                    ResourceClass::HandleCommandBytes,
                    ResourceLimit::new(4, "limits.reactor.maxHandleCommandBytes"),
                ),
            ],
        ));

        let decoded = decode_python_socket_payload(Some("dGVzdA=="), &resources)
            .expect("decode four charged bytes");
        assert_eq!(decoded.bytes, b"test");
        assert_eq!(resources.usage(ResourceClass::BufferedBytes).used, 4);

        let (encoded, encoded_reservation) = encode_python_socket_bytes(&decoded.bytes, &resources)
            .expect("reserve base64 response before encoding");
        assert_eq!(encoded, "dGVzdA==");
        assert_eq!(resources.usage(ResourceClass::BufferedBytes).used, 12);
        drop(encoded_reservation);

        let write = reserve_plain_socket_write_payload(&resources, &decoded.bytes)
            .expect("reserve aggregate and command bytes before plain write copy");
        assert_eq!(resources.usage(ResourceClass::BufferedBytes).used, 8);
        assert_eq!(resources.usage(ResourceClass::HandleCommands).used, 1);
        assert_eq!(resources.usage(ResourceClass::HandleCommandBytes).used, 4);
        drop(write);
        drop(decoded);
        assert!(resources.is_zero());
    }

    #[test]
    fn adapter_decode_limit_rejects_before_payload_allocation() {
        let resources = Arc::new(ResourceLedger::root(
            "python-socket-small-buffer",
            [(
                ResourceClass::BufferedBytes,
                ResourceLimit::new(3, "limits.resources.maxSocketBufferedBytes"),
            )],
        ));
        let error = decode_python_socket_payload(Some("dGVzdA=="), &resources)
            .expect_err("four decoded bytes exceed the configured three-byte budget");
        assert!(error.to_string().contains("ERR_AGENTOS_RESOURCE_LIMIT"));
        assert!(resources.is_zero());
    }
}
