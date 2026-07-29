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

fn python_socket_kind_error(op: &str, expected: &str) -> SidecarError {
    SidecarError::Execution(format!(
        "EOPNOTSUPP: python socket {op} requires a {expected} socket"
    ))
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
        if !self.vms.contains_key(vm_id) {
            return Ok(());
        }
        match self.python_socket_op(vm_id, process_id, &request).await {
            Ok(PythonSocketOp::Immediate(response)) => {
                self.respond_python_rpc(vm_id, process_id, request.id, Ok(response))
            }
            Ok(PythonSocketOp::Charged(response)) => {
                self.respond_python_rpc(vm_id, process_id, request.id, Ok(response.payload))
            }
            Ok(PythonSocketOp::Deferred) => Ok(()),
            Ok(PythonSocketOp::Wait(wait)) => {
                self.schedule_python_socket_wait(vm_id, process_id, request, wait)
            }
            Err(error) => self.respond_python_rpc(vm_id, process_id, request.id, Err(error)),
        }
    }

    async fn python_socket_op(
        &mut self,
        vm_id: &str,
        process_id: &str,
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
                let socket_paths = build_javascript_socket_path_context(
                    self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?,
                )?;
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
                    return self
                        .defer_python_native_tcp_connect(vm_id, process_id, request.id, resolved);
                }
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let pending = reserve_capability(&vm.capabilities, CapabilityKind::TcpSocket)?;
                let process = vm.active_processes.get_mut(process_id).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "python socket op for reaped vm/process",
                    ))
                })?;
                let socket = ActiveTcpSocket::connect_kernel_loopback(
                    &mut vm.kernel,
                    process.kernel_pid,
                    resolved,
                    None,
                    None,
                    None,
                    &socket_paths,
                    vm.capabilities.resources(),
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
                        if let Err(close_error) = socket.close(&mut vm.kernel, process.kernel_pid) {
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
                    &vm.kernel_socket_readiness,
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
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let process = vm.active_processes.get_mut(process_id).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "python socket op for reaped vm/process",
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
                    let bytes_sent =
                        socket.write_all(&mut vm.kernel, process.kernel_pid, &data.bytes)?;
                    return Ok(PythonSocketOp::Immediate(
                        PythonVfsRpcResponsePayload::SocketSent { bytes_sent },
                    ));
                }
                let response = socket.begin_plain_write(&data.bytes)?;
                let (runtime, responder) = self.python_socket_async_context(vm_id, process_id)?;
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
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let process = vm.active_processes.get_mut(process_id).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "python socket op for reaped vm/process",
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
                    let event =
                        socket.poll(&mut vm.kernel, process.kernel_pid, Duration::ZERO, false)?;
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
                self.remove_python_socket(vm_id, process_id, request)?;
                Ok(PythonSocketOp::Immediate(
                    PythonVfsRpcResponsePayload::Empty,
                ))
            }
            PythonVfsRpcMethod::UdpCreate => {
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let pending = reserve_capability(&vm.capabilities, CapabilityKind::UdpSocket)?;
                let process = vm.active_processes.get_mut(process_id).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "python socket op for reaped vm/process",
                    ))
                })?;
                let mut socket = ActiveUdpSocket::new_native(
                    JavascriptUdpFamily::Ipv4,
                    vm.capabilities.resources(),
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
                let socket_paths = build_javascript_socket_path_context(
                    self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?,
                )?;
                let python_socket_id = python_socket_id(request)?;
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let process = vm.active_processes.get_mut(process_id).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "python socket op for reaped vm/process",
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
                let send = socket.send_to(ActiveUdpSendToRequest {
                    bridge: &self.bridge,
                    kernel: &mut vm.kernel,
                    kernel_pid: process.kernel_pid,
                    vm_id,
                    dns: &vm.dns,
                    host: &host,
                    port,
                    context: &socket_paths,
                    contents: &data.bytes,
                })?;
                let bytes_sent = await_udp_send_result(send).await?;
                Ok(PythonSocketOp::Immediate(
                    PythonVfsRpcResponsePayload::SocketSent { bytes_sent },
                ))
            }
            PythonVfsRpcMethod::UdpRecvfrom => {
                let max = python_socket_recv_len(request);
                let python_socket_id = python_socket_id(request)?;
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let process = vm.active_processes.get_mut(process_id).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "python socket op for reaped vm/process",
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
                let socket = process
                    .udp_sockets
                    .get(&native_socket_id)
                    .ok_or_else(|| python_socket_backend_missing_error(python_socket_id))?;
                let event = socket
                    .poll(&mut vm.kernel, process.kernel_pid, Duration::ZERO)
                    .await?;
                let wait_timeout = python_socket_wait_timeout(request, socket.reactor_limits);
                if event.is_none() && !wait_timeout.is_zero() {
                    return Ok(PythonSocketOp::Wait(PythonSocketWait {
                        source: PythonSocketWaitSource::Notify(Arc::clone(
                            &socket.read_event_notify,
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
        process_id: &str,
        request_id: u64,
        resolved: ResolvedTcpConnectAddr,
    ) -> Result<PythonSocketOp, SidecarError> {
        debug_assert!(!resolved.use_kernel_loopback);
        let (
            connection_id,
            session_id,
            runtime,
            resources,
            limits,
            pending_capability,
            native_socket_id,
            python_socket_id,
        ) = {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let pending_capability =
                reserve_capability(&vm.capabilities, CapabilityKind::TcpSocket)?;
            let process = vm.active_processes.get_mut(process_id).ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "python socket connect for reaped vm/process",
                ))
            })?;
            let native_socket_id = process.allocate_tcp_socket_id();
            let python_socket_id = process.next_python_socket_id;
            process.next_python_socket_id = process.next_python_socket_id.wrapping_add(1);
            (
                vm.connection_id.clone(),
                vm.session_id.clone(),
                process.runtime_context.clone(),
                vm.capabilities.resources(),
                reactor_io_limits(&process.limits),
                pending_capability,
                native_socket_id,
                python_socket_id,
            )
        };
        let task_runtime = runtime.clone();
        let sender = self.process_event_sender.clone();
        let event_notify = Arc::clone(&self.process_event_notify);
        let vm_id = vm_id.to_owned();
        let process_id = process_id.to_owned();
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
                if sender
                    .send(ProcessEventEnvelope {
                        connection_id,
                        session_id,
                        vm_id,
                        process_id,
                        event: ActiveExecutionEvent::PythonSocketConnectCompletion(
                            Box::new(PythonSocketConnectCompletion { request_id, result }),
                        ),
                    })
                    .await
                    .is_err()
                {
                    eprintln!(
                        "ERR_AGENTOS_PROCESS_EVENT_CHANNEL_CLOSED: Python TCP connect completion could not be delivered"
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
        process_id: &str,
    ) -> Result<(agentos_runtime::RuntimeContext, PythonVfsRpcResponder), SidecarError> {
        let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        let process = vm.active_processes.get(process_id).ok_or_else(|| {
            SidecarError::InvalidState(String::from("python socket op for reaped vm/process"))
        })?;
        Ok((
            vm.runtime_context.clone(),
            process.execution.python_vfs_rpc_responder()?,
        ))
    }

    fn schedule_python_socket_wait(
        &self,
        vm_id: &str,
        process_id: &str,
        mut request: PythonVfsRpcRequest,
        wait: PythonSocketWait,
    ) -> Result<(), SidecarError> {
        let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        let runtime = vm.runtime_context.clone();
        let connection_id = vm.connection_id.clone();
        let session_id = vm.session_id.clone();
        let vm_id = vm_id.to_owned();
        let process_id = process_id.to_owned();
        let sender = self.process_event_sender.clone();
        let event_notify = Arc::clone(&self.process_event_notify);
        request.timeout_ms = Some(0);
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
                if sender
                    .send(ProcessEventEnvelope {
                        connection_id,
                        session_id,
                        vm_id,
                        process_id,
                        event: ActiveExecutionEvent::PythonVfsRpcRequest(Box::new(request)),
                    })
                    .await
                    .is_err()
                {
                    eprintln!(
                        "ERR_AGENTOS_PROCESS_EVENT_CHANNEL_CLOSED: Python socket readiness completion could not be delivered"
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
        process_id: &str,
        request: &PythonVfsRpcRequest,
    ) -> Result<(), SidecarError> {
        let Some(socket_id) = request.socket_id else {
            return Ok(());
        };
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let Some(process) = vm.active_processes.get_mut(process_id) else {
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
                        &mut vm.kernel,
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
                        &mut vm.kernel,
                        &kernel_readiness,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(in crate::execution) fn respond_python_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request_id: u64,
        response: Result<PythonVfsRpcResponsePayload, SidecarError>,
    ) -> Result<(), SidecarError> {
        let Some(vm) = self.vms.get_mut(vm_id) else {
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
        reserve_plain_socket_write_payload,
    };
    use agentos_runtime::accounting::{ResourceClass, ResourceLedger, ResourceLimit};
    use std::sync::Arc;

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
