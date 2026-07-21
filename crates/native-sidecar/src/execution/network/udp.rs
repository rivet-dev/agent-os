use super::super::*;
use crate::state::SocketFairnessRetirement;

pub(in crate::execution) struct ActiveUdpSendToRequest<'a, B> {
    pub(in crate::execution) bridge: &'a SharedBridge<B>,
    pub(in crate::execution) kernel: &'a mut SidecarKernel,
    pub(in crate::execution) kernel_pid: u32,
    pub(in crate::execution) vm_id: &'a str,
    pub(in crate::execution) dns: &'a VmDnsConfig,
    pub(in crate::execution) host: &'a str,
    pub(in crate::execution) port: u16,
    pub(in crate::execution) context: &'a JavascriptSocketPathContext,
    pub(in crate::execution) contents: &'a [u8],
}

pub(in crate::execution) struct ActiveUdpConnectRequest<'a, B> {
    bridge: &'a SharedBridge<B>,
    kernel: &'a mut SidecarKernel,
    kernel_pid: u32,
    vm_id: &'a str,
    dns: &'a VmDnsConfig,
    host: &'a str,
    port: u16,
    context: &'a JavascriptSocketPathContext,
}

pub(in crate::execution) struct UdpRemoteAddrRequest<'a, B> {
    pub(in crate::execution) bridge: &'a SharedBridge<B>,
    pub(in crate::execution) kernel: &'a SidecarKernel,
    pub(in crate::execution) vm_id: &'a str,
    pub(in crate::execution) dns: &'a VmDnsConfig,
    pub(in crate::execution) host: &'a str,
    pub(in crate::execution) port: u16,
    pub(in crate::execution) family: JavascriptUdpFamily,
    pub(in crate::execution) context: &'a JavascriptSocketPathContext,
}

pub(in crate::execution) struct JavascriptDgramSyncRpcServiceRequest<'a, B> {
    pub(in crate::execution) bridge: &'a SharedBridge<B>,
    pub(in crate::execution) kernel: &'a mut SidecarKernel,
    pub(in crate::execution) vm_id: &'a str,
    pub(in crate::execution) dns: &'a VmDnsConfig,
    pub(in crate::execution) socket_paths: &'a JavascriptSocketPathContext,
    pub(in crate::execution) process: &'a mut ActiveProcess,
    pub(in crate::execution) kernel_readiness: KernelSocketReadinessRegistry,
    pub(in crate::execution) sync_request: &'a JavascriptSyncRpcRequest,
    pub(in crate::execution) capabilities: CapabilityRegistry,
}

const UDP_MAX_DATAGRAM_BYTES: usize = 64 * 1024;

fn udp_receive_capacity(resources: &ResourceLedger, limits: ReactorIoLimits) -> usize {
    let aggregate_limit = resources
        .usage(ResourceClass::BufferedBytes)
        .limit
        .unwrap_or(UDP_MAX_DATAGRAM_BYTES);
    let udp_limit = resources
        .usage(ResourceClass::UdpBytes)
        .limit
        .unwrap_or(UDP_MAX_DATAGRAM_BYTES);
    UDP_MAX_DATAGRAM_BYTES
        .min(limits.byte_quantum)
        .min(aggregate_limit)
        .min(udp_limit)
        .max(1)
}

pub(crate) fn reserve_udp_receive_buffer(
    resources: &ResourceLedger,
    capacity: usize,
) -> Result<(Vec<u8>, Reservation, Reservation, Reservation, Reservation), SidecarError> {
    let byte_reservation = resources
        .reserve(ResourceClass::BufferedBytes, capacity)
        .map_err(SidecarError::from)?;
    let datagram_reservation = resources
        .reserve(ResourceClass::Datagrams, 1)
        .map_err(SidecarError::from)?;
    let udp_byte_reservation = resources
        .reserve(ResourceClass::UdpBytes, capacity)
        .map_err(SidecarError::from)?;
    let udp_datagram_reservation = resources
        .reserve(ResourceClass::UdpDatagrams, 1)
        .map_err(SidecarError::from)?;
    let buffer = vec![0_u8; capacity];
    Ok((
        buffer,
        byte_reservation,
        datagram_reservation,
        udp_byte_reservation,
        udp_datagram_reservation,
    ))
}

pub(in crate::execution) enum ActiveUdpSendResult {
    Immediate {
        written: usize,
        local_addr: SocketAddr,
    },
    Deferred {
        receiver: tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>,
    },
}

pub(in crate::execution) enum ActiveUdpValueResult {
    Immediate(Value),
    Deferred(tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>),
}

fn udp_value_service_response(
    result: ActiveUdpValueResult,
    task_class: agentos_runtime::TaskClass,
) -> JavascriptSyncRpcServiceResponse {
    match result {
        ActiveUdpValueResult::Immediate(value) => JavascriptSyncRpcServiceResponse::Json(value),
        ActiveUdpValueResult::Deferred(receiver) => JavascriptSyncRpcServiceResponse::Deferred {
            receiver,
            timeout: None,
            task_class,
        },
    }
}

fn udp_send_service_response(result: ActiveUdpSendResult) -> JavascriptSyncRpcServiceResponse {
    match result {
        ActiveUdpSendResult::Immediate {
            written,
            local_addr,
        } => JavascriptSyncRpcServiceResponse::Json(json!({
            "bytes": written,
            "localAddress": local_addr.ip().to_string(),
            "localPort": local_addr.port(),
            "family": socket_addr_family(&local_addr),
        })),
        ActiveUdpSendResult::Deferred { receiver } => JavascriptSyncRpcServiceResponse::Deferred {
            receiver,
            timeout: None,
            task_class: agentos_runtime::TaskClass::Udp,
        },
    }
}

pub(in crate::execution) async fn await_udp_send_result(
    result: ActiveUdpSendResult,
) -> Result<usize, SidecarError> {
    match result {
        ActiveUdpSendResult::Immediate { written, .. } => Ok(written),
        ActiveUdpSendResult::Deferred { receiver } => {
            let value = receiver.await.map_err(|_| {
                SidecarError::Execution(String::from(
                    "EPIPE: native UDP owner dropped send completion",
                ))
            })?;
            let value = value.map_err(|error| {
                SidecarError::Execution(format!("{}: {}", error.code, error.message))
            })?;
            value
                .get("bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    SidecarError::Execution(String::from(
                        "ERR_AGENTOS_UDP_COMPLETION: native UDP send omitted byte count",
                    ))
                })
                .and_then(|written| {
                    usize::try_from(written).map_err(|_| {
                        SidecarError::Execution(String::from(
                            "ERR_AGENTOS_UDP_COMPLETION: native UDP byte count overflow",
                        ))
                    })
                })
        }
    }
}

fn udp_command_admission_error(
    error: tokio::sync::mpsc::error::TrySendError<NativeUdpCommand>,
    limit: usize,
) -> SidecarError {
    match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => SidecarError::Execution(format!(
            "ERR_AGENTOS_UDP_COMMAND_LIMIT: UDP command queue exceeded {limit}; raise limits.reactor.maxHandleCommands"
        )),
        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
            SidecarError::Execution(String::from("EBADF: native UDP owner task is closed"))
        }
    }
}

fn reserve_udp_command(resources: &ResourceLedger) -> Result<SharedReservation, SidecarError> {
    resources
        .reserve(ResourceClass::HandleCommands, 1)
        .map(SharedReservation::new)
        .map_err(SidecarError::from)
}

fn reserve_udp_send_payload(
    resources: &ResourceLedger,
    contents: &[u8],
) -> Result<NativeUdpSendPayload, SidecarError> {
    let command = resources
        .reserve(ResourceClass::HandleCommands, 1)
        .map_err(SidecarError::from)?;
    let command_bytes = resources
        .reserve(ResourceClass::HandleCommandBytes, contents.len())
        .map_err(SidecarError::from)?;
    let buffered = resources
        .reserve(ResourceClass::BufferedBytes, contents.len())
        .map_err(SidecarError::from)?;
    let udp_bytes = resources
        .reserve(ResourceClass::UdpBytes, contents.len())
        .map_err(SidecarError::from)?;
    Ok(NativeUdpSendPayload {
        bytes: contents.to_vec(),
        _command_reservation: SharedReservation::new(command),
        _command_bytes_reservation: SharedReservation::new(command_bytes),
        _buffered_reservation: SharedReservation::new(buffered),
        _udp_bytes_reservation: SharedReservation::new(udp_bytes),
    })
}

fn udp_deferred_error(error: SidecarError) -> crate::state::DeferredRpcError {
    crate::state::DeferredRpcError {
        code: javascript_sync_rpc_error_code(&error),
        message: javascript_sync_rpc_error_message(&error),
    }
}

fn udp_io_deferred_error(error: std::io::Error) -> crate::state::DeferredRpcError {
    crate::state::DeferredRpcError {
        code: io_error_code(&error).unwrap_or_else(|| String::from("ERR_AGENTOS_UDP_NATIVE")),
        message: error.to_string(),
    }
}

fn notify_native_udp_readable(
    event_pusher: &Arc<SocketReadinessSubscribers>,
    read_event_notify: &Arc<tokio::sync::Notify>,
    wake_pending: &Arc<AtomicBool>,
    event: &'static str,
) {
    if !wake_pending.swap(true, Ordering::AcqRel) {
        read_event_notify.notify_one();
        push_socket_event(event_pusher, event);
    }
}

fn ipv4_interface(interface: Option<&str>) -> Result<Ipv4Addr, std::io::Error> {
    interface
        .filter(|value| !value.is_empty())
        .unwrap_or("0.0.0.0")
        .parse()
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
}

fn interface_name_to_index(interface: &str) -> Result<u32, std::io::Error> {
    nix::net::if_::if_nametoindex(interface)
        .map_err(|error| std::io::Error::from_raw_os_error(error as i32))
}

fn ipv6_interface_index(interface: Option<&str>) -> Result<u32, std::io::Error> {
    let Some(interface) = interface.filter(|value| !value.is_empty()) else {
        return Ok(0);
    };
    if interface == "::" {
        return Ok(0);
    }
    if let Ok(address) = interface.parse::<Ipv6Addr>() {
        let interface_name = nix::ifaddrs::getifaddrs()
            .map_err(|error| std::io::Error::from_raw_os_error(error as i32))?
            .find_map(|interface| {
                interface
                    .address
                    .as_ref()
                    .and_then(|address| address.as_sockaddr_in6())
                    .filter(|socket_address| socket_address.ip() == address)
                    .map(|_| interface.interface_name)
            })
            .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EADDRNOTAVAIL))?;
        return interface_name_to_index(interface_name.as_str());
    }
    let scope = interface
        .rsplit_once('%')
        .map_or(interface, |(_, scope)| scope);
    if let Ok(index) = scope.parse::<u32>() {
        return Ok(index);
    }
    interface_name_to_index(scope)
}

fn set_udp_multicast_interface(
    socket: &tokio::net::UdpSocket,
    family: JavascriptUdpFamily,
    interface: &str,
) -> Result<(), std::io::Error> {
    let socket_ref = SockRef::from(socket);
    match family {
        JavascriptUdpFamily::Ipv4 => {
            let address = ipv4_interface(Some(interface))?;
            socket_ref.set_multicast_if_v4(&address)
        }
        JavascriptUdpFamily::Ipv6 => {
            let index = ipv6_interface_index(Some(interface))?;
            socket_ref.set_multicast_if_v6(index)
        }
    }
}

fn set_udp_source_membership(
    socket: &tokio::net::UdpSocket,
    source: IpAddr,
    group: IpAddr,
    interface: Option<&str>,
    join: bool,
) -> Result<(), std::io::Error> {
    let (IpAddr::V4(source), IpAddr::V4(group)) = (source, group) else {
        return Err(std::io::Error::from_raw_os_error(libc::EOPNOTSUPP));
    };
    let interface = ipv4_interface(interface)?;
    let socket_ref = SockRef::from(socket);
    if join {
        socket_ref.join_ssm_v4(&source, &group, &interface)
    } else {
        socket_ref.leave_ssm_v4(&source, &group, &interface)
    }
}

fn disconnect_native_udp(socket: &tokio::net::UdpSocket) -> Result<(), std::io::Error> {
    match rustix::net::connect_unspec(socket) {
        Ok(()) => Ok(()),
        // BSD-family kernels may report EINVAL/EAFNOSUPPORT even though the
        // AF_UNSPEC disconnect took effect. The observable peer state is the
        // compatibility invariant Node relies on.
        Err(_) if socket.peer_addr().is_err() => Ok(()),
        Err(error) => Err(std::io::Error::from_raw_os_error(error.raw_os_error())),
    }
}

fn apply_native_udp_option(
    socket: &tokio::net::UdpSocket,
    family: JavascriptUdpFamily,
    option: NativeUdpSocketOption,
) -> Result<Value, std::io::Error> {
    let socket_ref = SockRef::from(socket);
    match option {
        NativeUdpSocketOption::Broadcast(enabled) => {
            socket_ref.set_broadcast(enabled)?;
            Ok(Value::Null)
        }
        NativeUdpSocketOption::Ttl(ttl) => {
            match family {
                JavascriptUdpFamily::Ipv4 => socket_ref.set_ttl_v4(ttl)?,
                JavascriptUdpFamily::Ipv6 => socket_ref.set_unicast_hops_v6(ttl)?,
            }
            Ok(json!(ttl))
        }
        NativeUdpSocketOption::MulticastTtl(ttl) => {
            if family != JavascriptUdpFamily::Ipv4 {
                return Err(std::io::Error::from_raw_os_error(libc::ENOPROTOOPT));
            }
            socket_ref.set_multicast_ttl_v4(ttl)?;
            Ok(json!(ttl))
        }
        NativeUdpSocketOption::MulticastLoopback(enabled) => {
            match family {
                JavascriptUdpFamily::Ipv4 => socket_ref.set_multicast_loop_v4(enabled)?,
                JavascriptUdpFamily::Ipv6 => socket_ref.set_multicast_loop_v6(enabled)?,
            }
            Ok(json!(if enabled { 1 } else { 0 }))
        }
        NativeUdpSocketOption::MulticastInterface(interface) => {
            set_udp_multicast_interface(socket, family, &interface)?;
            Ok(Value::Null)
        }
        NativeUdpSocketOption::Membership {
            group,
            interface,
            join,
        } => {
            match group {
                IpAddr::V4(group) if family == JavascriptUdpFamily::Ipv4 => {
                    let interface = ipv4_interface(interface.as_deref())?;
                    if join {
                        socket_ref.join_multicast_v4(&group, &interface)?;
                    } else {
                        socket_ref.leave_multicast_v4(&group, &interface)?;
                    }
                }
                IpAddr::V6(group) if family == JavascriptUdpFamily::Ipv6 => {
                    let index = ipv6_interface_index(interface.as_deref())?;
                    if join {
                        socket_ref.join_multicast_v6(&group, index)?;
                    } else {
                        socket_ref.leave_multicast_v6(&group, index)?;
                    }
                }
                _ => return Err(std::io::Error::from_raw_os_error(libc::EINVAL)),
            }
            Ok(Value::Null)
        }
        NativeUdpSocketOption::SourceMembership {
            source,
            group,
            interface,
            join,
        } => {
            set_udp_source_membership(socket, source, group, interface.as_deref(), join)?;
            Ok(Value::Null)
        }
    }
}

async fn acquire_native_udp_fair_turn(
    runtime: &agentos_runtime::RuntimeContext,
    limits: ReactorIoLimits,
    fairness_identity: &OnceLock<(u64, u64)>,
    fairness_identity_committed: &tokio::sync::Notify,
) -> Result<FairWorkTurn, SidecarError> {
    let (capability_id, vm_generation) =
        committed_socket_fairness_identity(fairness_identity, fairness_identity_committed).await;
    runtime
        .fairness()
        .acquire(
            vm_generation,
            capability_id,
            FairBudget::new(
                limits.datagram_quantum.min(limits.operation_quantum).max(1),
                limits.byte_quantum.max(1),
            ),
        )
        .await
        .map_err(|error| SidecarError::Execution(error.to_string()))
}

async fn run_native_udp_send_fair_step<F>(
    runtime: &agentos_runtime::RuntimeContext,
    limits: ReactorIoLimits,
    fairness_identity: &OnceLock<(u64, u64)>,
    fairness_identity_committed: &tokio::sync::Notify,
    payload_len: usize,
    operation: F,
) -> Result<std::io::Result<usize>, SidecarError>
where
    F: FnOnce() -> std::io::Result<usize>,
{
    let turn = acquire_native_udp_fair_turn(
        runtime,
        limits,
        fairness_identity,
        fairness_identity_committed,
    )
    .await?;
    let allowance = turn.allowance();
    if payload_len > allowance.bytes {
        turn.complete(FairBudget::default(), false)
            .map_err(|error| SidecarError::Execution(error.to_string()))?;
        return Err(SidecarError::Execution(format!(
            "ERR_AGENTOS_FAIRNESS_BYTE_BUDGET: UDP datagram uses {payload_len} bytes, allowance {} bytes; raise limits.reactor.byteQuantum",
            allowance.bytes
        )));
    }

    // `try_send*` performs one nonblocking syscall. Settle the process-global
    // grant before the caller can await readiness again, including EAGAIN.
    let result = operation();
    let used_bytes = result.as_ref().copied().unwrap_or(0);
    turn.complete(FairBudget::new(1, used_bytes), false)
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    Ok(result)
}

async fn send_native_udp_datagram_fair(
    socket: &tokio::net::UdpSocket,
    runtime: &agentos_runtime::RuntimeContext,
    limits: ReactorIoLimits,
    fairness_identity: &OnceLock<(u64, u64)>,
    fairness_identity_committed: &tokio::sync::Notify,
    payload: &[u8],
    remote_addr: Option<SocketAddr>,
) -> Result<std::io::Result<usize>, SidecarError> {
    loop {
        if let Err(error) = socket.writable().await {
            return Ok(Err(error));
        }
        let result = run_native_udp_send_fair_step(
            runtime,
            limits,
            fairness_identity,
            fairness_identity_committed,
            payload.len(),
            || match remote_addr {
                Some(remote_addr) => socket.try_send_to(payload, remote_addr),
                None => socket.try_send(payload),
            },
        )
        .await?;
        match result {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            result => return Ok(result),
        }
    }
}

async fn run_native_udp_connect_fair_step<F>(
    runtime: &agentos_runtime::RuntimeContext,
    limits: ReactorIoLimits,
    fairness_identity: &OnceLock<(u64, u64)>,
    fairness_identity_committed: &tokio::sync::Notify,
    operation: F,
) -> Result<std::io::Result<()>, SidecarError>
where
    F: FnOnce() -> std::io::Result<()>,
{
    let turn = acquire_native_udp_fair_turn(
        runtime,
        limits,
        fairness_identity,
        fairness_identity_committed,
    )
    .await?;
    // UDP connect has no transport handshake: connect(2) records the peer in
    // one synchronous syscall. Calling socket2 directly avoids holding this
    // grant across Tokio's async ToSocketAddrs wrapper.
    let result = operation();
    turn.complete(FairBudget::new(1, 0), false)
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    Ok(result)
}

async fn connect_native_udp_socket_fair(
    socket: &tokio::net::UdpSocket,
    runtime: &agentos_runtime::RuntimeContext,
    limits: ReactorIoLimits,
    fairness_identity: &OnceLock<(u64, u64)>,
    fairness_identity_committed: &tokio::sync::Notify,
    remote_addr: SocketAddr,
) -> Result<std::io::Result<()>, SidecarError> {
    loop {
        if let Err(error) = socket.writable().await {
            return Ok(Err(error));
        }
        let result = run_native_udp_connect_fair_step(
            runtime,
            limits,
            fairness_identity,
            fairness_identity_committed,
            || {
                socket.try_io(tokio::io::Interest::WRITABLE, || {
                    SockRef::from(socket).connect(&SockAddr::from(remote_addr))
                })
            },
        )
        .await?;
        match result {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            result => return Ok(result),
        }
    }
}

struct NativeUdpOwnerRegistration {
    family: JavascriptUdpFamily,
    resources: Arc<ResourceLedger>,
    limits: ReactorIoLimits,
    fairness_identity: Arc<OnceLock<(u64, u64)>>,
    fairness_identity_committed: Arc<tokio::sync::Notify>,
    event_pusher: Arc<SocketReadinessSubscribers>,
    read_event_notify: Arc<tokio::sync::Notify>,
    wake_pending: Arc<AtomicBool>,
}

struct NativeUdpOwnerTask {
    socket: tokio::net::UdpSocket,
    commands: TokioReceiver<NativeUdpCommand>,
    runtime: agentos_runtime::RuntimeContext,
    registration: NativeUdpOwnerRegistration,
}

async fn run_native_udp_owner(task: NativeUdpOwnerTask) {
    let NativeUdpOwnerTask {
        socket,
        mut commands,
        runtime,
        registration:
            NativeUdpOwnerRegistration {
                family,
                resources,
                limits,
                fairness_identity,
                fairness_identity_committed,
                event_pusher,
                read_event_notify,
                wake_pending,
            },
    } = task;
    let mut receive_queue = VecDeque::new();
    let mut read_ready = false;
    let mut receive_paused = false;
    let mut read_failed = false;
    let mut connected_guest_remote = None;

    loop {
        if read_ready && !receive_paused && !read_failed {
            let operation_quantum = limits.datagram_quantum.min(limits.operation_quantum).max(1);
            let mut operations = 0;
            let mut bytes_this_turn = 0;
            while operations < operation_quantum && bytes_this_turn < limits.byte_quantum.max(1) {
                let configured_capacity = udp_receive_capacity(&resources, limits)
                    .min(limits.byte_quantum.max(1) - bytes_this_turn)
                    .max(1);
                let admission = reserve_udp_receive_buffer(&resources, configured_capacity);
                let (
                    mut buffer,
                    mut byte_reservation,
                    datagram_reservation,
                    mut udp_byte_reservation,
                    udp_datagram_reservation,
                ) = match admission {
                    Ok(admission) => admission,
                    Err(_) => {
                        // Resource reservations are the receive queue's count
                        // and byte capacity. Leave the datagram in the OS and
                        // sleep until a consumer releases either dimension.
                        receive_paused = true;
                        break;
                    }
                };
                let turn = match acquire_native_udp_fair_turn(
                    &runtime,
                    limits,
                    &fairness_identity,
                    &fairness_identity_committed,
                )
                .await
                {
                    Ok(turn) => turn,
                    Err(error) => {
                        drop((
                            buffer,
                            byte_reservation,
                            datagram_reservation,
                            udp_byte_reservation,
                            udp_datagram_reservation,
                        ));
                        let was_empty = receive_queue.is_empty();
                        receive_queue.push_back(JavascriptUdpSocketEvent::Error {
                            code: Some(javascript_sync_rpc_error_code(&error)),
                            message: javascript_sync_rpc_error_message(&error),
                        });
                        if was_empty {
                            notify_native_udp_readable(
                                &event_pusher,
                                &read_event_notify,
                                &wake_pending,
                                "error",
                            );
                        }
                        read_failed = true;
                        break;
                    }
                };
                match socket.try_recv_from(&mut buffer) {
                    Ok((bytes_read, remote_addr)) => {
                        buffer.truncate(bytes_read);
                        let unused = configured_capacity.saturating_sub(bytes_read);
                        drop(byte_reservation.split(unused));
                        drop(udp_byte_reservation.split(unused));
                        if let Err(error) = turn.complete(FairBudget::new(1, bytes_read), false) {
                            eprintln!(
                                "ERR_AGENTOS_UDP_FAIRNESS: failed to complete native receive turn: {error}"
                            );
                        }
                        let was_empty = receive_queue.is_empty();
                        receive_queue.push_back(JavascriptUdpSocketEvent::Message {
                            data: buffer,
                            remote_addr,
                            _byte_reservation: SharedReservation::new(byte_reservation),
                            _datagram_reservation: SharedReservation::new(datagram_reservation),
                            _udp_byte_reservation: SharedReservation::new(udp_byte_reservation),
                            _udp_datagram_reservation: SharedReservation::new(
                                udp_datagram_reservation,
                            ),
                        });
                        if was_empty {
                            notify_native_udp_readable(
                                &event_pusher,
                                &read_event_notify,
                                &wake_pending,
                                "data",
                            );
                        }
                        operations += 1;
                        bytes_this_turn += bytes_read;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        drop((
                            buffer,
                            byte_reservation,
                            datagram_reservation,
                            udp_byte_reservation,
                            udp_datagram_reservation,
                        ));
                        if let Err(error) = turn.complete(FairBudget::new(1, 0), false) {
                            eprintln!(
                                "ERR_AGENTOS_UDP_FAIRNESS: failed to complete EAGAIN receive turn: {error}"
                            );
                        }
                        read_ready = false;
                        break;
                    }
                    Err(error) => {
                        drop((
                            buffer,
                            byte_reservation,
                            datagram_reservation,
                            udp_byte_reservation,
                            udp_datagram_reservation,
                        ));
                        if let Err(fairness_error) = turn.complete(FairBudget::new(1, 0), false) {
                            eprintln!(
                                "ERR_AGENTOS_UDP_FAIRNESS: failed to complete errored receive turn: {fairness_error}"
                            );
                        }
                        let was_empty = receive_queue.is_empty();
                        receive_queue.push_back(JavascriptUdpSocketEvent::Error {
                            code: io_error_code(&error),
                            message: error.to_string(),
                        });
                        if was_empty {
                            notify_native_udp_readable(
                                &event_pusher,
                                &read_event_notify,
                                &wake_pending,
                                "error",
                            );
                        }
                        read_failed = true;
                        break;
                    }
                }
            }
            if read_ready && !receive_paused && !read_failed {
                // A hot UDP source gets one configured batch per Tokio turn.
                tokio::task::yield_now().await;
            }
        }

        let capacity_changed = resources.capacity_changed();
        tokio::pin!(capacity_changed);
        tokio::select! {
            biased;
            () = runtime.admission_closed() => return,
            command = commands.recv() => {
                let Some(command) = command else {
                    return;
                };
                match command {
                    NativeUdpCommand::Poll { _command_reservation: _, completion } => {
                        let event = receive_queue.pop_front();
                        if receive_queue.is_empty() {
                            wake_pending.store(false, Ordering::Release);
                        }
                        completion.settle(Ok(event));
                    }
                    NativeUdpCommand::Send {
                        payload,
                        remote_addr,
                        guest_local_addr,
                        completion,
                    } => {
                        if connected_guest_remote.is_some() && remote_addr.is_some() {
                            completion.settle(Err(crate::state::DeferredRpcError {
                                code: String::from("ERR_SOCKET_DGRAM_IS_CONNECTED"),
                                message: String::from(
                                    "Already connected: send() does not accept a destination",
                                ),
                            }));
                            continue;
                        }
                        if connected_guest_remote.is_none() && remote_addr.is_none() {
                            completion.settle(Err(crate::state::DeferredRpcError {
                                code: String::from("ERR_SOCKET_BAD_PORT"),
                                message: String::from(
                                    "Destination port is required for an unconnected UDP socket",
                                ),
                            }));
                            continue;
                        }
                        let result = match tokio::time::timeout(
                            limits.operation_deadline,
                            send_native_udp_datagram_fair(
                                &socket,
                                &runtime,
                                limits,
                                &fairness_identity,
                                &fairness_identity_committed,
                                &payload.bytes,
                                remote_addr,
                            ),
                        )
                        .await
                        {
                            Ok(Ok(Ok(written))) if written == payload.bytes.len() => Ok(json!({
                                "bytes": written,
                                "localAddress": guest_local_addr.ip().to_string(),
                                "localPort": guest_local_addr.port(),
                                "family": socket_addr_family(&guest_local_addr),
                            })),
                            Ok(Ok(Ok(written))) => Err(crate::state::DeferredRpcError {
                                code: String::from("EIO"),
                                message: format!(
                                    "partial UDP datagram write: {written} of {} bytes",
                                    payload.bytes.len()
                                ),
                            }),
                            Ok(Ok(Err(error))) => Err(udp_io_deferred_error(error)),
                            Ok(Err(error)) => Err(udp_deferred_error(error)),
                            Err(_) => Err(crate::state::DeferredRpcError {
                                code: String::from("ETIMEDOUT"),
                                message: format!(
                                    "UDP send exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                                    limits.operation_deadline.as_millis()
                                ),
                            }),
                        };
                        completion.settle(result);
                    }
                    NativeUdpCommand::Connect {
                        _command_reservation: _,
                        remote_addr,
                        guest_local_addr,
                        guest_remote_addr,
                        completion,
                    } => {
                        if connected_guest_remote.is_some() {
                            completion.settle(Err(crate::state::DeferredRpcError {
                                code: String::from("ERR_SOCKET_DGRAM_IS_CONNECTED"),
                                message: String::from("Already connected"),
                            }));
                            continue;
                        }
                        let result = match tokio::time::timeout(
                            limits.operation_deadline,
                            connect_native_udp_socket_fair(
                                &socket,
                                &runtime,
                                limits,
                                &fairness_identity,
                                &fairness_identity_committed,
                                remote_addr,
                            ),
                        )
                        .await
                        {
                            Ok(Ok(Ok(()))) => {
                                connected_guest_remote = Some(guest_remote_addr);
                                Ok(json!({
                                    "localAddress": guest_local_addr.ip().to_string(),
                                    "localPort": guest_local_addr.port(),
                                    "localFamily": socket_addr_family(&guest_local_addr),
                                    "remoteAddress": guest_remote_addr.ip().to_string(),
                                    "remotePort": guest_remote_addr.port(),
                                    "remoteFamily": socket_addr_family(&guest_remote_addr),
                                }))
                            }
                            Ok(Ok(Err(error))) => Err(udp_io_deferred_error(error)),
                            Ok(Err(error)) => Err(udp_deferred_error(error)),
                            Err(_) => Err(crate::state::DeferredRpcError {
                                code: String::from("ETIMEDOUT"),
                                message: format!(
                                    "UDP connect exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                                    limits.operation_deadline.as_millis()
                                ),
                            }),
                        };
                        completion.settle(result);
                    }
                    NativeUdpCommand::Disconnect { _command_reservation: _, completion } => {
                        if connected_guest_remote.is_none() {
                            completion.settle(Err(crate::state::DeferredRpcError {
                                code: String::from("ERR_SOCKET_DGRAM_NOT_CONNECTED"),
                                message: String::from("Not connected"),
                            }));
                            continue;
                        }
                        let result = disconnect_native_udp(&socket)
                            .map(|()| {
                                connected_guest_remote = None;
                                Value::Null
                            })
                            .map_err(udp_io_deferred_error);
                        completion.settle(result);
                    }
                    NativeUdpCommand::RemoteAddress { _command_reservation: _, completion } => {
                        let result = connected_guest_remote
                            .map(|remote_addr| json!({
                                "address": remote_addr.ip().to_string(),
                                "port": remote_addr.port(),
                                "family": socket_addr_family(&remote_addr),
                            }))
                            .ok_or_else(|| crate::state::DeferredRpcError {
                                code: String::from("ERR_SOCKET_DGRAM_NOT_CONNECTED"),
                                message: String::from("Not connected"),
                            });
                        completion.settle(result);
                    }
                    NativeUdpCommand::SetOption {
                        _command_reservation: _,
                        option,
                        guest_local_addr,
                        completion,
                    } => {
                        completion.settle(
                            apply_native_udp_option(&socket, family, option)
                                .map(|value| json!({
                                    "value": value,
                                    "localAddress": guest_local_addr.ip().to_string(),
                                    "localPort": guest_local_addr.port(),
                                    "localFamily": socket_addr_family(&guest_local_addr),
                                }))
                                .map_err(udp_io_deferred_error),
                        );
                    }
                    NativeUdpCommand::SetBufferSize {
                        _command_reservation: _,
                        which,
                        size,
                        completion,
                    } => {
                        let socket_ref = SockRef::from(&socket);
                        let result = match which.as_str() {
                            "recv" => socket_ref.set_recv_buffer_size(size),
                            "send" => socket_ref.set_send_buffer_size(size),
                            _ => Err(std::io::Error::from_raw_os_error(libc::EINVAL)),
                        }
                        .map(|()| Value::Null)
                        .map_err(udp_io_deferred_error);
                        completion.settle(result);
                    }
                    NativeUdpCommand::GetBufferSize {
                        _command_reservation: _,
                        which,
                        completion,
                    } => {
                        let socket_ref = SockRef::from(&socket);
                        let result = match which.as_str() {
                            "recv" => socket_ref.recv_buffer_size(),
                            "send" => socket_ref.send_buffer_size(),
                            _ => Err(std::io::Error::from_raw_os_error(libc::EINVAL)),
                        }
                        .map(|size| json!(size))
                        .map_err(udp_io_deferred_error);
                        completion.settle(result);
                    }
                }
            }
            () = &mut capacity_changed, if receive_paused => {
                receive_paused = false;
                read_ready = true;
            }
            readiness = socket.readable(), if !read_ready && !receive_paused && !read_failed => {
                match readiness {
                    Ok(()) => read_ready = true,
                    Err(error) => {
                        let was_empty = receive_queue.is_empty();
                        receive_queue.push_back(JavascriptUdpSocketEvent::Error {
                            code: io_error_code(&error),
                            message: error.to_string(),
                        });
                        if was_empty {
                            notify_native_udp_readable(
                                &event_pusher,
                                &read_event_notify,
                                &wake_pending,
                                "error",
                            );
                        }
                        read_failed = true;
                    }
                }
            }
        }
    }
}

fn spawn_native_udp_owner(
    runtime: &agentos_runtime::RuntimeContext,
    socket: UdpSocket,
    registration: NativeUdpOwnerRegistration,
) -> Result<TokioSender<NativeUdpCommand>, SidecarError> {
    socket.set_nonblocking(true).map_err(sidecar_net_error)?;
    let socket = tokio::net::UdpSocket::from_std(socket).map_err(sidecar_net_error)?;
    let capacity = registration.limits.max_handle_commands.max(1);
    let (commands, receiver) = tokio_channel(capacity);
    let task_runtime = runtime.clone();
    runtime
        .spawn(agentos_runtime::TaskClass::Udp, async move {
            run_native_udp_owner(NativeUdpOwnerTask {
                socket,
                commands: receiver,
                runtime: task_runtime,
                registration,
            })
            .await;
        })
        .map_err(SidecarError::from)?;
    Ok(commands)
}

impl ActiveUdpSocket {
    pub(in crate::execution) fn set_fairness_identity(&mut self, identity: Option<(u64, u64)>) {
        let Some(identity) = identity else {
            return;
        };
        if self.fairness_identity.set(identity).is_err()
            && self.fairness_identity.get().copied() != Some(identity)
        {
            eprintln!(
                "ERR_AGENTOS_FAIRNESS_IDENTITY: attempted to replace committed UDP capability identity"
            );
        }
        self.fairness_identity_committed.notify_waiters();
    }

    pub(in crate::execution) fn set_event_pusher(
        &self,
        session: Option<V8SessionHandle>,
        identity: Option<(
            agentos_runtime::capability::CapabilityId,
            agentos_runtime::capability::CapabilityGeneration,
        )>,
    ) {
        self.readiness_registration.register(
            session,
            identity,
            agentos_runtime::readiness::ReadyFlags::DATAGRAM,
        );
    }

    async fn acquire_fair_turn(&self) -> Result<FairWorkTurn, SidecarError> {
        acquire_native_udp_fair_turn(
            &self.runtime_context,
            self.reactor_limits,
            &self.fairness_identity,
            &self.fairness_identity_committed,
        )
        .await
    }

    pub(in crate::execution) fn new(
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        family: JavascriptUdpFamily,
        resources: Arc<ResourceLedger>,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        let spec = match family {
            JavascriptUdpFamily::Ipv4 => SocketSpec::udp(),
            JavascriptUdpFamily::Ipv6 => SocketSpec::new(SocketDomain::Inet6, SocketType::Datagram),
        };
        let socket_id = kernel
            .socket_create(EXECUTION_DRIVER_NAME, kernel_pid, spec)
            .map_err(kernel_error)?;
        let fairness_identity = Arc::new(OnceLock::new());
        let fairness_retirement =
            SocketFairnessRetirement::new(Arc::clone(&fairness_identity), runtime_context.clone());
        let event_pusher = SocketReadinessSubscribers::new(&resources);
        Ok(Self {
            family,
            native_commands: None,
            kernel_socket_id: Some(socket_id),
            guest_local_addr: None,
            native_local_addr: None,
            kernel_connected_remote_addr: None,
            recv_buffer_size: 0,
            send_buffer_size: 0,
            description_handles: Arc::new(()),
            kernel_transfer_guard: None,
            resources,
            runtime_context,
            reactor_limits,
            fairness_identity,
            fairness_identity_committed: Arc::new(tokio::sync::Notify::new()),
            fairness_retirement,
            description_lease: Arc::new(SocketDescriptionLease::default()),
            read_event_notify: Arc::new(tokio::sync::Notify::new()),
            event_pusher: Arc::clone(&event_pusher),
            readiness_registration: SocketReadinessRegistration::new(event_pusher, None, None),
            native_read_wake_pending: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Create a native-backed UDP capability without an adapter-owned task or
    /// descriptor registry. The socket is bound lazily by the same `bind`,
    /// `send_to`, and `poll` operations used by every native UDP consumer.
    pub(in crate::execution) fn new_native(
        family: JavascriptUdpFamily,
        resources: Arc<ResourceLedger>,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        let bind_addr = match family {
            JavascriptUdpFamily::Ipv4 => "127.0.0.1:0",
            JavascriptUdpFamily::Ipv6 => "[::1]:0",
        };
        let socket = UdpSocket::bind(bind_addr).map_err(sidecar_net_error)?;
        let local_addr = socket.local_addr().map_err(sidecar_net_error)?;
        let fairness_identity = Arc::new(OnceLock::new());
        let fairness_identity_committed = Arc::new(tokio::sync::Notify::new());
        let fairness_retirement =
            SocketFairnessRetirement::new(Arc::clone(&fairness_identity), runtime_context.clone());
        let read_event_notify = Arc::new(tokio::sync::Notify::new());
        let event_pusher = SocketReadinessSubscribers::new(&resources);
        let native_read_wake_pending = Arc::new(AtomicBool::new(false));
        let native_commands = spawn_native_udp_owner(
            &runtime_context,
            socket,
            NativeUdpOwnerRegistration {
                family,
                resources: Arc::clone(&resources),
                limits: reactor_limits,
                fairness_identity: Arc::clone(&fairness_identity),
                fairness_identity_committed: Arc::clone(&fairness_identity_committed),
                event_pusher: Arc::clone(&event_pusher),
                read_event_notify: Arc::clone(&read_event_notify),
                wake_pending: Arc::clone(&native_read_wake_pending),
            },
        )?;
        Ok(Self {
            family,
            native_commands: Some(native_commands),
            kernel_socket_id: None,
            guest_local_addr: Some(local_addr),
            native_local_addr: Some(local_addr),
            kernel_connected_remote_addr: None,
            recv_buffer_size: 0,
            send_buffer_size: 0,
            description_handles: Arc::new(()),
            kernel_transfer_guard: None,
            resources,
            runtime_context,
            reactor_limits,
            fairness_identity,
            fairness_identity_committed,
            fairness_retirement,
            description_lease: Arc::new(SocketDescriptionLease::default()),
            read_event_notify,
            event_pusher: Arc::clone(&event_pusher),
            readiness_registration: SocketReadinessRegistration::new(event_pusher, None, None),
            native_read_wake_pending,
        })
    }

    pub(in crate::execution) fn clone_for_fd_transfer(&self) -> Result<Self, SidecarError> {
        Ok(Self {
            family: self.family,
            native_commands: self.native_commands.clone(),
            kernel_socket_id: self.kernel_socket_id,
            guest_local_addr: self.guest_local_addr,
            native_local_addr: self.native_local_addr,
            kernel_connected_remote_addr: self.kernel_connected_remote_addr,
            recv_buffer_size: self.recv_buffer_size,
            send_buffer_size: self.send_buffer_size,
            description_handles: Arc::clone(&self.description_handles),
            kernel_transfer_guard: self.kernel_transfer_guard.clone(),
            resources: Arc::clone(&self.resources),
            runtime_context: self.runtime_context.clone(),
            reactor_limits: self.reactor_limits,
            fairness_identity: Arc::clone(&self.fairness_identity),
            fairness_identity_committed: Arc::clone(&self.fairness_identity_committed),
            fairness_retirement: Arc::clone(&self.fairness_retirement),
            description_lease: Arc::clone(&self.description_lease),
            read_event_notify: Arc::clone(&self.read_event_notify),
            event_pusher: Arc::clone(&self.event_pusher),
            readiness_registration: SocketReadinessRegistration::new(
                Arc::clone(&self.event_pusher),
                None,
                None,
            ),
            native_read_wake_pending: Arc::clone(&self.native_read_wake_pending),
        })
    }

    pub(in crate::execution) fn is_final_description_handle(&self) -> bool {
        Arc::strong_count(&self.description_handles) == 1
    }

    pub(in crate::execution) fn retain_description_lease(
        &self,
        lease: Arc<agentos_runtime::capability::CapabilityLease>,
    ) {
        self.description_lease.retain(lease);
    }

    pub(in crate::execution) fn local_addr(&self) -> Option<SocketAddr> {
        self.guest_local_addr
    }

    pub(in crate::execution) fn bind(
        &mut self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        host: Option<&str>,
        port: u16,
        context: &JavascriptSocketPathContext,
    ) -> Result<SocketAddr, SidecarError> {
        if self.native_commands.is_some() || self.guest_local_addr.is_some() {
            return Err(SidecarError::Execution(String::from(
                "EINVAL: agentos dgram socket is already bound",
            )));
        }

        let (_bind_host, guest_host, guest_family) = normalize_udp_bind_host(host, self.family)?;
        let guest_port = allocate_guest_listen_port(
            port,
            guest_family,
            &context.used_udp_guest_ports,
            context.listen_policy,
        )?;
        let local_addr = resolve_udp_bind_addr(guest_host, guest_port, self.family)?;
        if let Some(socket_id) = self.kernel_socket_id {
            kernel
                .socket_bind_inet(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    socket_id,
                    InetSocketAddress::new(local_addr.ip().to_string(), local_addr.port()),
                )
                .map_err(kernel_error)?;
        } else {
            return Err(SidecarError::Execution(String::from(
                "EINVAL: native UDP socket is already bound",
            )));
        }
        self.guest_local_addr = Some(local_addr);
        Ok(local_addr)
    }

    fn ensure_bound_for_send(
        &mut self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        context: &JavascriptSocketPathContext,
    ) -> Result<SocketAddr, SidecarError> {
        if let Some(local_addr) = self.local_addr() {
            return Ok(local_addr);
        }

        self.bind(kernel, kernel_pid, None, 0, context)
    }

    fn ensure_native_owner(&mut self) -> Result<&TokioSender<NativeUdpCommand>, SidecarError> {
        if self.native_commands.is_none() {
            let guest_addr = self.guest_local_addr.ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "ERR_AGENTOS_UDP_NOT_BOUND: UDP socket must have a guest address before native activation",
                ))
            })?;
            let socket =
                UdpSocket::bind(SocketAddr::new(guest_addr.ip(), 0)).map_err(sidecar_net_error)?;
            let native_local_addr = socket.local_addr().map_err(sidecar_net_error)?;
            socket.set_nonblocking(true).map_err(sidecar_net_error)?;
            let socket_ref = SockRef::from(&socket);
            if self.recv_buffer_size > 0 {
                socket_ref
                    .set_recv_buffer_size(self.recv_buffer_size)
                    .map_err(sidecar_net_error)?;
            }
            if self.send_buffer_size > 0 {
                socket_ref
                    .set_send_buffer_size(self.send_buffer_size)
                    .map_err(sidecar_net_error)?;
            }
            let commands = spawn_native_udp_owner(
                &self.runtime_context,
                socket,
                NativeUdpOwnerRegistration {
                    family: self.family,
                    resources: Arc::clone(&self.resources),
                    limits: self.reactor_limits,
                    fairness_identity: Arc::clone(&self.fairness_identity),
                    fairness_identity_committed: Arc::clone(&self.fairness_identity_committed),
                    event_pusher: Arc::clone(&self.event_pusher),
                    read_event_notify: Arc::clone(&self.read_event_notify),
                    wake_pending: Arc::clone(&self.native_read_wake_pending),
                },
            )?;
            self.native_commands = Some(commands);
            self.native_local_addr = Some(native_local_addr);
        }
        self.native_commands.as_ref().ok_or_else(|| {
            SidecarError::Execution(String::from("EBADF: native UDP owner is unavailable"))
        })
    }

    pub(in crate::execution) fn send_to<B>(
        &mut self,
        request: ActiveUdpSendToRequest<'_, B>,
    ) -> Result<ActiveUdpSendResult, SidecarError>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        if self.kernel_connected_remote_addr.is_some() {
            return Err(SidecarError::Execution(String::from(
                "ERR_SOCKET_DGRAM_IS_CONNECTED: send() does not accept a destination on a connected UDP socket",
            )));
        }
        let ActiveUdpSendToRequest {
            bridge,
            kernel,
            kernel_pid,
            vm_id,
            dns,
            host,
            port,
            context,
            contents,
        } = request;
        let remote_addr = resolve_udp_addr(UdpRemoteAddrRequest {
            bridge,
            kernel,
            vm_id,
            dns,
            host,
            port,
            family: self.family,
            context,
        })?;
        let local_addr = self.ensure_bound_for_send(kernel, kernel_pid, context)?;
        let use_kernel_loopback = self.kernel_socket_id.is_some()
            && is_loopback_ip(remote_addr.ip())
            && remote_addr.port() == port
            && !context.loopback_exempt_ports.contains(&port);
        if use_kernel_loopback {
            if let Some(socket_id) = self.kernel_socket_id {
                let written = kernel
                    .socket_send_to_inet_loopback(
                        EXECUTION_DRIVER_NAME,
                        kernel_pid,
                        socket_id,
                        InetSocketAddress::new(remote_addr.ip().to_string(), remote_addr.port()),
                        contents,
                    )
                    .map_err(kernel_error)?;
                return Ok(ActiveUdpSendResult::Immediate {
                    written,
                    local_addr,
                });
            } else {
                unreachable!("kernel UDP path selected without a kernel socket")
            }
        }

        let payload = reserve_udp_send_payload(&self.resources, contents)?;
        let (completion, receiver) = tokio::sync::oneshot::channel();
        let command = NativeUdpCommand::Send {
            payload,
            remote_addr: Some(remote_addr),
            guest_local_addr: local_addr,
            completion,
        };
        self.ensure_native_owner()?
            .try_send(command)
            .map_err(|error| {
                udp_command_admission_error(error, self.reactor_limits.max_handle_commands)
            })?;
        Ok(ActiveUdpSendResult::Deferred { receiver })
    }

    fn send_connected(
        &mut self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        context: &JavascriptSocketPathContext,
        contents: &[u8],
    ) -> Result<ActiveUdpSendResult, SidecarError> {
        let local_addr = self.ensure_bound_for_send(kernel, kernel_pid, context)?;
        if let Some(remote_addr) = self.kernel_connected_remote_addr {
            let socket_id = self
                .kernel_socket_id
                .expect("kernel connected UDP send selected without a kernel socket");
            let written = kernel
                .socket_send_to_inet_loopback(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    socket_id,
                    InetSocketAddress::new(remote_addr.ip().to_string(), remote_addr.port()),
                    contents,
                )
                .map_err(kernel_error)?;
            return Ok(ActiveUdpSendResult::Immediate {
                written,
                local_addr,
            });
        }
        let payload = reserve_udp_send_payload(&self.resources, contents)?;
        let (completion, receiver) = tokio::sync::oneshot::channel();
        let command = NativeUdpCommand::Send {
            payload,
            remote_addr: None,
            guest_local_addr: local_addr,
            completion,
        };
        self.ensure_native_owner()?
            .try_send(command)
            .map_err(|error| {
                udp_command_admission_error(error, self.reactor_limits.max_handle_commands)
            })?;
        Ok(ActiveUdpSendResult::Deferred { receiver })
    }

    pub(in crate::execution) async fn poll(
        &self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        wait: Duration,
    ) -> Result<Option<JavascriptUdpSocketEvent>, SidecarError> {
        let wait = wait.min(self.reactor_limits.operation_deadline);
        let receive_capacity = udp_receive_capacity(&self.resources, self.reactor_limits);
        if let Some(socket_id) = self.kernel_socket_id {
            // A hybrid socket may be readable through either the VM-local
            // kernel socket or its native external socket. Never block on one
            // source while the other can already make progress.
            let kernel_wait = if self.native_commands.is_some() {
                Duration::ZERO
            } else {
                wait
            };
            let result = kernel
                .poll_targets(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    vec![PollTargetEntry::socket(socket_id, POLLIN)],
                    i32::try_from(kernel_wait.as_millis()).unwrap_or(i32::MAX),
                )
                .map_err(kernel_error)?;
            let revents = result
                .targets
                .first()
                .map(|entry| entry.revents)
                .unwrap_or_else(PollEvents::empty);
            if revents.is_empty() && self.native_commands.is_none() {
                return Ok(None);
            }
            if !revents.is_empty() {
                let turn = self.acquire_fair_turn().await?;
                let receive_capacity = receive_capacity.min(turn.allowance().bytes).max(1);
                let (event, used_bytes) = match kernel.socket_recv_datagram_charged(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    socket_id,
                    receive_capacity,
                ) {
                    Ok(Some(datagram)) => {
                        let (source_address, payload, reservations) = datagram.into_parts();
                        let used_bytes = payload.len();
                        let (
                        byte_reservation,
                        datagram_reservation,
                        udp_byte_reservation,
                        udp_datagram_reservation,
                    ) = reservations.ok_or_else(|| {
                        SidecarError::Execution(String::from(
                            "ERR_AGENTOS_RESOURCE_ACCOUNTING_INVARIANT: kernel UDP handoff did not transfer its queue reservations",
                        ))
                    })?;
                        let remote_addr = source_address
                            .map(|source| {
                                resolve_udp_bind_addr(source.host(), source.port(), self.family)
                            })
                            .transpose()?
                            .unwrap_or_else(|| match self.family {
                                JavascriptUdpFamily::Ipv4 => {
                                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
                                }
                                JavascriptUdpFamily::Ipv6 => {
                                    SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0)
                                }
                            });
                        (
                            Some(JavascriptUdpSocketEvent::Message {
                                data: payload,
                                remote_addr,
                                _byte_reservation: SharedReservation::new(byte_reservation),
                                _datagram_reservation: SharedReservation::new(datagram_reservation),
                                _udp_byte_reservation: SharedReservation::new(udp_byte_reservation),
                                _udp_datagram_reservation: SharedReservation::new(
                                    udp_datagram_reservation,
                                ),
                            }),
                            used_bytes,
                        )
                    }
                    Ok(None) => (None, 0),
                    Err(error) if error.code() == "EAGAIN" => (None, 0),
                    Err(error) => (
                        Some(JavascriptUdpSocketEvent::Error {
                            code: Some(error.code().to_string()),
                            message: error.to_string(),
                        }),
                        0,
                    ),
                };
                turn.complete(FairBudget::new(1, used_bytes), false)
                    .map_err(|error| SidecarError::Execution(error.to_string()))?;
                if event.is_some() {
                    return Ok(event);
                }
            }
        }
        let Some(commands) = self.native_commands.as_ref() else {
            return Ok(None);
        };
        let poll_once = || async {
            let (completion, receiver) = tokio::sync::oneshot::channel();
            let command = NativeUdpCommand::Poll {
                _command_reservation: reserve_udp_command(&self.resources)?,
                completion,
            };
            commands.try_send(command).map_err(|error| {
                udp_command_admission_error(error, self.reactor_limits.max_handle_commands)
            })?;
            match tokio::time::timeout(self.reactor_limits.operation_deadline, receiver).await {
                Ok(Ok(result)) => result.map_err(|error| {
                    SidecarError::Execution(format!("{}: {}", error.code, error.message))
                }),
                Ok(Err(_)) => Err(SidecarError::Execution(String::from(
                    "EPIPE: native UDP owner dropped poll completion",
                ))),
                Err(_) => Err(SidecarError::Execution(format!(
                    "ETIMEDOUT: UDP poll exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                    self.reactor_limits.operation_deadline.as_millis()
                ))),
            }
        };
        let event = poll_once().await?;
        if event.is_some() || wait.is_zero() {
            return Ok(event);
        }
        let notified = self.read_event_notify.notified();
        if tokio::time::timeout(wait, notified).await.is_err() {
            return Ok(None);
        }
        poll_once().await
    }

    fn submit_native_value_command(
        &mut self,
        build: impl FnOnce(
            SharedReservation,
            tokio::sync::oneshot::Sender<Result<Value, crate::state::DeferredRpcError>>,
        ) -> NativeUdpCommand,
    ) -> Result<ActiveUdpValueResult, SidecarError> {
        let command_reservation = reserve_udp_command(&self.resources)?;
        let (completion, receiver) = tokio::sync::oneshot::channel();
        let command = build(command_reservation, completion);
        let limit = self.reactor_limits.max_handle_commands;
        self.ensure_native_owner()?
            .try_send(command)
            .map_err(|error| udp_command_admission_error(error, limit))?;
        Ok(ActiveUdpValueResult::Deferred(receiver))
    }

    pub(in crate::execution) fn connect<B>(
        &mut self,
        request: ActiveUdpConnectRequest<'_, B>,
    ) -> Result<ActiveUdpValueResult, SidecarError>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        if self.kernel_connected_remote_addr.is_some() {
            return Err(SidecarError::Execution(String::from(
                "ERR_SOCKET_DGRAM_IS_CONNECTED: Already connected",
            )));
        }
        let ActiveUdpConnectRequest {
            bridge,
            kernel,
            kernel_pid,
            vm_id,
            dns,
            host,
            port,
            context,
        } = request;
        let remote_addr = resolve_udp_addr(UdpRemoteAddrRequest {
            bridge,
            kernel,
            vm_id,
            dns,
            host,
            port,
            family: self.family,
            context,
        })?;
        let guest_remote_addr = SocketAddr::new(remote_addr.ip(), port);
        let guest_local_addr = self.ensure_bound_for_send(kernel, kernel_pid, context)?;
        let use_kernel_loopback = self.kernel_socket_id.is_some()
            && is_loopback_ip(remote_addr.ip())
            && remote_addr.port() == port
            && !context.loopback_exempt_ports.contains(&port);
        if use_kernel_loopback {
            let socket_id = self
                .kernel_socket_id
                .expect("kernel UDP connect selected without a kernel socket");
            kernel
                .socket_connect_udp_loopback(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    socket_id,
                    InetSocketAddress::new(
                        guest_remote_addr.ip().to_string(),
                        guest_remote_addr.port(),
                    ),
                )
                .map_err(kernel_error)?;
            self.kernel_connected_remote_addr = Some(guest_remote_addr);
            return Ok(ActiveUdpValueResult::Immediate(json!({
                "localAddress": guest_local_addr.ip().to_string(),
                "localPort": guest_local_addr.port(),
                "localFamily": socket_addr_family(&guest_local_addr),
                "remoteAddress": guest_remote_addr.ip().to_string(),
                "remotePort": guest_remote_addr.port(),
                "remoteFamily": socket_addr_family(&guest_remote_addr),
            })));
        }
        self.submit_native_value_command(|command_reservation, completion| {
            NativeUdpCommand::Connect {
                _command_reservation: command_reservation,
                remote_addr,
                guest_local_addr,
                guest_remote_addr,
                completion,
            }
        })
    }

    pub(in crate::execution) fn disconnect(
        &mut self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
    ) -> Result<ActiveUdpValueResult, SidecarError> {
        if self.kernel_connected_remote_addr.is_some() {
            let socket_id = self
                .kernel_socket_id
                .expect("kernel UDP disconnect selected without a kernel socket");
            kernel
                .socket_disconnect_udp(EXECUTION_DRIVER_NAME, kernel_pid, socket_id)
                .map_err(kernel_error)?;
            self.kernel_connected_remote_addr = None;
            return Ok(ActiveUdpValueResult::Immediate(Value::Null));
        }
        if self.native_commands.is_none() {
            return Err(SidecarError::Execution(String::from(
                "ERR_SOCKET_DGRAM_NOT_CONNECTED: Not connected",
            )));
        }
        self.submit_native_value_command(|command_reservation, completion| {
            NativeUdpCommand::Disconnect {
                _command_reservation: command_reservation,
                completion,
            }
        })
    }

    pub(in crate::execution) fn remote_address(
        &mut self,
    ) -> Result<ActiveUdpValueResult, SidecarError> {
        if let Some(remote_addr) = self.kernel_connected_remote_addr {
            return Ok(ActiveUdpValueResult::Immediate(json!({
                "address": remote_addr.ip().to_string(),
                "port": remote_addr.port(),
                "family": socket_addr_family(&remote_addr),
            })));
        }
        if self.native_commands.is_none() {
            return Err(SidecarError::Execution(String::from(
                "ERR_SOCKET_DGRAM_NOT_CONNECTED: Not connected",
            )));
        }
        self.submit_native_value_command(|command_reservation, completion| {
            NativeUdpCommand::RemoteAddress {
                _command_reservation: command_reservation,
                completion,
            }
        })
    }

    fn set_option(
        &mut self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        context: &JavascriptSocketPathContext,
        option: NativeUdpSocketOption,
    ) -> Result<ActiveUdpValueResult, SidecarError> {
        let permits_implicit_bind = matches!(
            &option,
            NativeUdpSocketOption::Membership { join: true, .. }
                | NativeUdpSocketOption::SourceMembership { join: true, .. }
        );
        if self.guest_local_addr.is_none() && !permits_implicit_bind {
            return Err(SidecarError::Execution(String::from(
                "EBADF: UDP socket option requires a bound socket",
            )));
        }
        let guest_local_addr = self.ensure_bound_for_send(kernel, kernel_pid, context)?;
        self.submit_native_value_command(|command_reservation, completion| {
            NativeUdpCommand::SetOption {
                _command_reservation: command_reservation,
                option,
                guest_local_addr,
                completion,
            }
        })
    }

    pub(in crate::execution) fn close(&mut self, kernel: &mut SidecarKernel, kernel_pid: u32) {
        self.native_read_wake_pending
            .store(false, Ordering::Release);
        if let Some(socket_id) = self.kernel_socket_id {
            let _ = close_kernel_socket_idempotent(kernel, kernel_pid, socket_id);
        }
        self.native_commands.take();
        self.guest_local_addr = None;
        self.native_local_addr = None;
        self.kernel_connected_remote_addr = None;
    }

    fn set_buffer_size(
        &mut self,
        which: &str,
        size: usize,
    ) -> Result<ActiveUdpValueResult, SidecarError> {
        match which {
            "recv" => self.recv_buffer_size = size,
            "send" => self.send_buffer_size = size,
            other => {
                return Err(SidecarError::InvalidState(format!(
                    "unsupported UDP buffer size kind {other}"
                )));
            }
        }
        if self.native_commands.is_none() {
            return Ok(ActiveUdpValueResult::Immediate(Value::Null));
        }
        let which = which.to_owned();
        self.submit_native_value_command(|command_reservation, completion| {
            NativeUdpCommand::SetBufferSize {
                _command_reservation: command_reservation,
                which,
                size,
                completion,
            }
        })
    }

    fn get_buffer_size(&mut self, which: &str) -> Result<ActiveUdpValueResult, SidecarError> {
        if self.native_commands.is_none() {
            return Ok(ActiveUdpValueResult::Immediate(json!(match which {
                "recv" => self.recv_buffer_size,
                "send" => self.send_buffer_size,
                other => {
                    return Err(SidecarError::InvalidState(format!(
                        "unsupported UDP buffer size kind {other}"
                    )));
                }
            })));
        }
        let which = which.to_owned();
        self.submit_native_value_command(|command_reservation, completion| {
            NativeUdpCommand::GetBufferSize {
                _command_reservation: command_reservation,
                which,
                completion,
            }
        })
    }
}

// ActiveExecution, ActiveExecutionEvent, SocketQueryKind moved to crate::state

fn dgram_option_field<'a>(payload: &'a Value, field: &str) -> Result<&'a Value, SidecarError> {
    payload.get(field).ok_or_else(|| {
        SidecarError::InvalidState(format!("dgram.setOption payload requires {field}"))
    })
}

fn dgram_option_ip(payload: &Value, field: &str) -> Result<IpAddr, SidecarError> {
    let value = dgram_option_field(payload, field)?
        .as_str()
        .ok_or_else(|| SidecarError::InvalidState(format!("{field} must be an IP address")))?;
    value.parse().map_err(|_| {
        SidecarError::Execution(format!("EINVAL: invalid UDP {field} address {value}"))
    })
}

fn dgram_option_interface(payload: &Value) -> Result<Option<String>, SidecarError> {
    match payload.get("interface") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(SidecarError::InvalidState(String::from(
            "dgram.setOption interface must be a string",
        ))),
    }
}

fn parse_native_udp_option(
    name: &str,
    payload: &Value,
) -> Result<NativeUdpSocketOption, SidecarError> {
    match name {
        "broadcast" => Ok(NativeUdpSocketOption::Broadcast(
            dgram_option_field(payload, "enabled")?
                .as_bool()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.setOption enabled must be boolean",
                    ))
                })?,
        )),
        "ttl" => Ok(NativeUdpSocketOption::Ttl(
            u32::try_from(
                dgram_option_field(payload, "ttl")?
                    .as_u64()
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "dgram.setOption ttl must be an unsigned integer",
                        ))
                    })?,
            )
            .map_err(|_| SidecarError::Execution(String::from("EINVAL: UDP TTL overflow")))?,
        )),
        "multicastTtl" => Ok(NativeUdpSocketOption::MulticastTtl(
            u32::try_from(
                dgram_option_field(payload, "ttl")?
                    .as_u64()
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "dgram.setOption ttl must be an unsigned integer",
                        ))
                    })?,
            )
            .map_err(|_| {
                SidecarError::Execution(String::from("EINVAL: UDP multicast TTL overflow"))
            })?,
        )),
        "multicastLoopback" => Ok(NativeUdpSocketOption::MulticastLoopback(
            dgram_option_field(payload, "enabled")?
                .as_bool()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.setOption enabled must be boolean",
                    ))
                })?,
        )),
        "multicastInterface" => Ok(NativeUdpSocketOption::MulticastInterface(
            dgram_option_field(payload, "interface")?
                .as_str()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.setOption interface must be a string",
                    ))
                })?
                .to_owned(),
        )),
        "membership" => Ok(NativeUdpSocketOption::Membership {
            group: dgram_option_ip(payload, "group")?,
            interface: dgram_option_interface(payload)?,
            join: dgram_option_field(payload, "join")?
                .as_bool()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from("dgram.setOption join must be boolean"))
                })?,
        }),
        "sourceMembership" => Ok(NativeUdpSocketOption::SourceMembership {
            source: dgram_option_ip(payload, "source")?,
            group: dgram_option_ip(payload, "group")?,
            interface: dgram_option_interface(payload)?,
            join: dgram_option_field(payload, "join")?
                .as_bool()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from("dgram.setOption join must be boolean"))
                })?,
        }),
        _ => Err(SidecarError::InvalidState(format!(
            "unsupported UDP option {name}"
        ))),
    }
}

pub(in crate::execution) fn release_udp_socket_handle(
    process: &mut ActiveProcess,
    socket_id: &str,
    mut socket: ActiveUdpSocket,
    kernel: &mut SidecarKernel,
    kernel_readiness: &KernelSocketReadinessRegistry,
) -> Result<(), SidecarError> {
    let identity = process
        .capability_readiness_identity(&NativeCapabilityKey::UdpSocket(socket_id.to_owned()));
    unregister_kernel_readiness_target(kernel_readiness, socket.kernel_socket_id, identity);
    if socket.is_final_description_handle() {
        socket.close(kernel, process.kernel_pid);
    }
    process.release_description_capability(
        &NativeCapabilityKey::UdpSocket(socket_id.to_owned()),
        socket.fairness_identity.get().copied(),
        &socket.description_lease,
    )
}

pub(in crate::execution) fn service_javascript_dgram_sync_rpc<B>(
    request: JavascriptDgramSyncRpcServiceRequest<'_, B>,
) -> Result<JavascriptSyncRpcServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let JavascriptDgramSyncRpcServiceRequest {
        bridge,
        kernel,
        vm_id,
        dns,
        socket_paths,
        process,
        kernel_readiness,
        sync_request: request,
        capabilities,
    } = request;
    match request.method.as_str() {
        "dgram.createSocket" => {
            let pending = reserve_capability(&capabilities, CapabilityKind::UdpSocket)?;
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
            let mut socket = ActiveUdpSocket::new(
                kernel,
                process.kernel_pid,
                family,
                capabilities.resources(),
                process.runtime_context.clone(),
                reactor_io_limits(&process.limits),
            )?;
            let capability_key = NativeCapabilityKey::UdpSocket(socket_id.clone());
            let identity = match commit_process_capability(
                process,
                pending,
                capability_key.clone(),
                socket_id.clone(),
                socket.kernel_socket_id,
            ) {
                Ok(identity) => identity,
                Err(error) => {
                    socket.close(kernel, process.kernel_pid);
                    return Err(error);
                }
            };
            socket.set_fairness_identity(process.capability_fairness_identity(&capability_key));
            socket.retain_description_lease(
                process
                    .shared_capability_lease(&capability_key)
                    .expect("committed UDP capability lease"),
            );
            socket.set_event_pusher(
                process.execution.javascript_v8_session_handle(),
                process.capability_readiness_identity(&capability_key),
            );
            register_kernel_readiness_target(
                &kernel_readiness,
                socket.kernel_socket_id,
                process.execution.javascript_v8_session_handle(),
                Some(Arc::clone(&socket.read_event_notify)),
                process.capability_readiness_identity(&capability_key),
                socket_id.clone(),
                KernelSocketReadinessEvent::Datagram,
            );
            process.udp_sockets.insert(socket_id.clone(), socket);
            Ok(json!({
                "socketId": socket_id,
                "capabilityId": identity.0,
                "capabilityGeneration": identity.1,
                "type": family.socket_type(),
            })
            .into())
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
            let local_addr = socket.bind(
                kernel,
                process.kernel_pid,
                payload.address.as_deref(),
                payload.port,
                socket_paths,
            )?;
            Ok(local_endpoint_value(&local_addr).into())
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
            let result = match payload.port {
                Some(port) => socket.send_to(ActiveUdpSendToRequest {
                    bridge,
                    kernel,
                    kernel_pid: process.kernel_pid,
                    vm_id,
                    dns,
                    host: payload.address.as_deref().unwrap_or("localhost"),
                    port,
                    context: socket_paths,
                    contents: &chunk,
                })?,
                None => socket.send_connected(kernel, process.kernel_pid, socket_paths, &chunk)?,
            };
            Ok(udp_send_service_response(result))
        }
        "dgram.connect" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "dgram.connect socket id")?;
            let payload = request
                .args
                .get(1)
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.connect requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptDgramConnectRequest>(value).map_err(
                        |error| {
                            SidecarError::InvalidState(format!(
                                "invalid dgram.connect payload: {error}"
                            ))
                        },
                    )
                })?;
            let socket = process.udp_sockets.get_mut(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            let result = socket.connect(ActiveUdpConnectRequest {
                bridge,
                kernel,
                kernel_pid: process.kernel_pid,
                vm_id,
                dns,
                host: payload.address.as_deref().unwrap_or("localhost"),
                port: payload.port,
                context: socket_paths,
            })?;
            Ok(udp_value_service_response(
                result,
                agentos_runtime::TaskClass::Udp,
            ))
        }
        "dgram.disconnect" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "dgram.disconnect socket id")?;
            let socket = process.udp_sockets.get_mut(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            let result = socket.disconnect(kernel, process.kernel_pid)?;
            Ok(udp_value_service_response(
                result,
                agentos_runtime::TaskClass::Udp,
            ))
        }
        "dgram.remoteAddress" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "dgram.remoteAddress socket id")?;
            let socket = process.udp_sockets.get_mut(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            let result = socket.remote_address()?;
            Ok(udp_value_service_response(
                result,
                agentos_runtime::TaskClass::Udp,
            ))
        }
        "dgram.close" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "dgram.close socket id")?;
            let socket = process.udp_sockets.remove(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            release_udp_socket_handle(process, socket_id, socket, kernel, &kernel_readiness)?;
            Ok(Value::Null.into())
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
            .map(Into::into)
        }
        "dgram.setOption" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "dgram.setOption socket id")?;
            let name =
                javascript_sync_rpc_arg_str(&request.args, 1, "dgram.setOption option name")?;
            let payload = request.args.get(2).ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "dgram.setOption requires an option payload",
                ))
            })?;
            let option = parse_native_udp_option(name, payload)?;
            let socket = process.udp_sockets.get_mut(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            let result = socket.set_option(kernel, process.kernel_pid, socket_paths, option)?;
            Ok(udp_value_service_response(
                result,
                agentos_runtime::TaskClass::Udp,
            ))
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
            let socket = process.udp_sockets.get_mut(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            let result = socket.set_buffer_size(which, size)?;
            Ok(udp_value_service_response(
                result,
                agentos_runtime::TaskClass::Udp,
            ))
        }
        "dgram.getBufferSize" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "dgram.getBufferSize socket id")?;
            let which =
                javascript_sync_rpc_arg_str(&request.args, 1, "dgram.getBufferSize buffer kind")?;
            let socket = process.udp_sockets.get_mut(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            let result = socket.get_buffer_size(which)?;
            Ok(udp_value_service_response(
                result,
                agentos_runtime::TaskClass::Udp,
            ))
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript dgram sync RPC method {other}"
        ))),
    }
}

#[cfg(test)]
mod native_udp_owner_tests {
    use super::*;
    use agentos_runtime::accounting::ResourceLimit;

    #[test]
    fn would_block_udp_step_releases_the_process_fairness_turn() {
        let process_runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create UDP fairness test runtime");
        let runtime = process_runtime.context();
        let first_generation = runtime
            .allocate_vm_generation()
            .expect("allocate first UDP fairness generation");
        let second_generation = runtime
            .allocate_vm_generation()
            .expect("allocate second UDP fairness generation");
        let identity = Arc::new(OnceLock::new());
        identity
            .set((91_001, first_generation))
            .expect("commit UDP fairness identity");
        let committed = Arc::new(tokio::sync::Notify::new());
        let limits = reactor_io_limits(&crate::limits::VmLimits::default());

        runtime.handle().block_on(async {
            let result =
                run_native_udp_send_fair_step(&runtime, limits, &identity, &committed, 1, || {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "synthetic stale readiness",
                    ))
                })
                .await
                .expect("run one nonblocking UDP fairness step")
                .expect_err("synthetic UDP syscall should report WouldBlock");
            assert_eq!(result.kind(), std::io::ErrorKind::WouldBlock);

            // The production caller waits for socket readiness only after the
            // step returns. Another VM must be schedulable during that wait.
            let (_readiness_tx, readiness_rx) = tokio::sync::oneshot::channel::<()>();
            let pending_readiness = tokio::spawn(async move {
                let _ = readiness_rx.await;
            });
            tokio::task::yield_now().await;
            assert!(!pending_readiness.is_finished());

            let next = tokio::time::timeout(
                Duration::from_secs(1),
                runtime
                    .fairness()
                    .acquire(second_generation, 91_002, FairBudget::new(1, 1)),
            )
            .await
            .expect("UDP readiness wait must not hold the global fairness turn")
            .expect("acquire second VM fairness turn");
            next.complete(FairBudget::new(1, 1), false)
                .expect("complete second VM fairness turn");

            pending_readiness.abort();
            runtime
                .fairness()
                .retire_vm(first_generation)
                .expect("retire first UDP fairness generation");
            runtime
                .fairness()
                .retire_vm(second_generation)
                .expect("retire second UDP fairness generation");
        });
    }

    #[test]
    fn fair_udp_connect_and_send_use_real_nonblocking_socket_steps() {
        let process_runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create UDP socket-step test runtime");
        let runtime = process_runtime.context();
        let generation = runtime
            .allocate_vm_generation()
            .expect("allocate UDP socket-step generation");
        let identity = OnceLock::new();
        identity
            .set((91_003, generation))
            .expect("commit UDP socket-step fairness identity");
        let committed = tokio::sync::Notify::new();
        let limits = reactor_io_limits(&crate::limits::VmLimits::default());

        runtime.handle().block_on(async {
            let receiver = tokio::net::UdpSocket::bind("127.0.0.1:0")
                .await
                .expect("bind UDP socket-step receiver");
            let sender = tokio::net::UdpSocket::bind("127.0.0.1:0")
                .await
                .expect("bind UDP socket-step sender");
            let receiver_addr = receiver.local_addr().expect("receiver address");

            connect_native_udp_socket_fair(
                &sender,
                &runtime,
                limits,
                &identity,
                &committed,
                receiver_addr,
            )
            .await
            .expect("admit UDP connect fairness step")
            .expect("connect UDP socket");
            let payload = b"fair-datagram";
            let written = send_native_udp_datagram_fair(
                &sender, &runtime, limits, &identity, &committed, payload, None,
            )
            .await
            .expect("admit UDP send fairness step")
            .expect("send connected UDP datagram");
            assert_eq!(written, payload.len());

            let mut received = [0_u8; 32];
            let (read, _) =
                tokio::time::timeout(Duration::from_secs(1), receiver.recv_from(&mut received))
                    .await
                    .expect("receive UDP datagram before deadline")
                    .expect("receive UDP datagram");
            assert_eq!(&received[..read], payload);
            runtime
                .fairness()
                .retire_vm(generation)
                .expect("retire UDP socket-step generation");
        });
    }

    #[test]
    fn send_admission_reserves_command_and_protocol_bytes_atomically() {
        let resources = ResourceLedger::root(
            "udp-send-owner-test",
            [
                (
                    ResourceClass::HandleCommands,
                    ResourceLimit::new(2, "test.maxHandleCommands"),
                ),
                (
                    ResourceClass::HandleCommandBytes,
                    ResourceLimit::new(4, "test.maxHandleCommandBytes"),
                ),
                (
                    ResourceClass::BufferedBytes,
                    ResourceLimit::new(4, "test.maxBufferedBytes"),
                ),
                (
                    ResourceClass::UdpBytes,
                    ResourceLimit::new(4, "test.maxUdpBytes"),
                ),
            ],
        );
        let payload =
            reserve_udp_send_payload(&resources, b"1234").expect("reserve one bounded UDP send");
        assert_eq!(resources.usage(ResourceClass::HandleCommands).used, 1);
        assert_eq!(resources.usage(ResourceClass::HandleCommandBytes).used, 4);
        assert_eq!(resources.usage(ResourceClass::BufferedBytes).used, 4);
        assert_eq!(resources.usage(ResourceClass::UdpBytes).used, 4);

        let error = reserve_udp_send_payload(&resources, b"x")
            .expect_err("second UDP send must fail before queueing bytes");
        assert!(error.to_string().contains("resource=handleCommandBytes"));
        assert_eq!(
            resources.usage(ResourceClass::HandleCommands).used,
            1,
            "failed multi-resource admission must roll back its command slot"
        );
        drop(payload);
        assert!(resources.is_zero());
    }

    #[test]
    fn receive_admission_pauses_before_recv_and_resumes_with_one_coalesced_wake() {
        let process_runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create UDP owner test runtime");
        let resources = Arc::new(ResourceLedger::child(
            "udp-owner-test",
            [
                (
                    ResourceClass::BufferedBytes,
                    ResourceLimit::new(64, "test.maxBufferedBytes"),
                ),
                (
                    ResourceClass::Datagrams,
                    ResourceLimit::new(1, "test.maxDatagrams"),
                ),
                (
                    ResourceClass::UdpBytes,
                    ResourceLimit::new(64, "test.maxUdpBytes"),
                ),
                (
                    ResourceClass::UdpDatagrams,
                    ResourceLimit::new(1, "test.maxUdpDatagrams"),
                ),
            ],
            Arc::clone(process_runtime.context().resources()),
        ));
        let runtime = process_runtime
            .context()
            .scoped_for_vm(Arc::clone(&resources), 7001);
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind owner socket");
        let owner_address = socket.local_addr().expect("owner address");
        let read_event_notify = Arc::new(tokio::sync::Notify::new());
        let wake_pending = Arc::new(AtomicBool::new(false));
        let fairness_identity = Arc::new(OnceLock::new());
        fairness_identity
            .set((8001, 7001))
            .expect("commit test fairness identity");
        let limits = reactor_io_limits(&crate::limits::VmLimits::default());
        let commands = {
            let _runtime_guard = runtime.handle().enter();
            spawn_native_udp_owner(
                &runtime,
                socket,
                NativeUdpOwnerRegistration {
                    family: JavascriptUdpFamily::Ipv4,
                    resources: Arc::clone(&resources),
                    limits,
                    fairness_identity,
                    fairness_identity_committed: Arc::new(tokio::sync::Notify::new()),
                    event_pusher: SocketReadinessSubscribers::new(&resources),
                    read_event_notify: Arc::clone(&read_event_notify),
                    wake_pending: Arc::clone(&wake_pending),
                },
            )
            .expect("spawn UDP owner")
        };
        let sender = UdpSocket::bind("127.0.0.1:0").expect("bind UDP sender");

        runtime.handle().block_on(async {
            let first_ready = read_event_notify.notified();
            sender
                .send_to(b"first", owner_address)
                .expect("send first datagram");
            sender
                .send_to(b"second", owner_address)
                .expect("send second datagram");
            tokio::time::timeout(Duration::from_secs(2), first_ready)
                .await
                .expect("first coalesced receive wake");

            let (first_completion, first_response) = tokio::sync::oneshot::channel();
            commands
                .try_send(NativeUdpCommand::Poll {
                    _command_reservation: reserve_udp_command(&resources)
                        .expect("reserve first poll command"),
                    completion: first_completion,
                })
                .expect("submit first poll");
            let first = first_response
                .await
                .expect("first poll completion")
                .expect("first poll success")
                .expect("first queued datagram");
            let JavascriptUdpSocketEvent::Message { data, .. } = &first else {
                panic!("first UDP event was not a datagram");
            };
            assert_eq!(data, b"first");
            assert_eq!(resources.usage(ResourceClass::UdpDatagrams).used, 1);

            let (blocked_completion, blocked_response) = tokio::sync::oneshot::channel();
            commands
                .try_send(NativeUdpCommand::Poll {
                    _command_reservation: reserve_udp_command(&resources)
                        .expect("reserve blocked poll command"),
                    completion: blocked_completion,
                })
                .expect("submit blocked poll");
            assert!(blocked_response
                .await
                .expect("blocked poll completion")
                .expect("blocked poll success")
                .is_none());

            let second_ready = read_event_notify.notified();
            drop(first);
            tokio::time::timeout(Duration::from_secs(2), second_ready)
                .await
                .expect("capacity release must admit second datagram");
            assert!(wake_pending.load(Ordering::Acquire));

            let (second_completion, second_response) = tokio::sync::oneshot::channel();
            commands
                .try_send(NativeUdpCommand::Poll {
                    _command_reservation: reserve_udp_command(&resources)
                        .expect("reserve second poll command"),
                    completion: second_completion,
                })
                .expect("submit second poll");
            let second = second_response
                .await
                .expect("second poll completion")
                .expect("second poll success")
                .expect("second queued datagram");
            let JavascriptUdpSocketEvent::Message { data, .. } = &second else {
                panic!("second UDP event was not a datagram");
            };
            assert_eq!(data, b"second");
            assert!(!wake_pending.load(Ordering::Acquire));
            drop(second);
        });

        drop(commands);
        runtime.handle().block_on(async {
            tokio::time::timeout(Duration::from_secs(2), async {
                while !resources.is_zero() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("UDP owner must release reservations after mailbox closure");
        });
        assert!(
            resources.is_zero(),
            "UDP owner leaked resource reservations"
        );
    }
}
