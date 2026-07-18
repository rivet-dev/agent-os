use super::super::*;
use agentos_execution::host::{
    ManagedTcpEndpoint, ManagedUnixAddress, NetworkOperation as HostNetworkOperation,
};

/// Runtime-neutral inputs required by the stateful managed endpoint lifecycle.
/// Executor adapters are responsible only for decoding compatibility payloads into
/// [`HostNetworkOperation`]; endpoint state and kernel/capability mutations
/// remain sidecar-owned here for every executor.
pub(in crate::execution) struct ManagedEndpointServiceContext<'a, B> {
    pub(in crate::execution) bridge: &'a SharedBridge<B>,
    pub(in crate::execution) vm_id: &'a str,
    pub(in crate::execution) dns: &'a VmDnsConfig,
    pub(in crate::execution) socket_paths: &'a SocketPathContext,
    pub(in crate::execution) kernel: &'a mut SidecarKernel,
    pub(in crate::execution) kernel_readiness: KernelSocketReadinessRegistry,
    pub(in crate::execution) process: &'a mut ActiveProcess,
    pub(in crate::execution) capabilities: CapabilityRegistry,
    pub(in crate::execution) call_id: u64,
}

pub(in crate::execution) fn service_managed_endpoint_operation<B>(
    context: ManagedEndpointServiceContext<'_, B>,
    operation: HostNetworkOperation,
) -> Result<HostServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let response = match operation {
        HostNetworkOperation::ManagedBindUnix { address } => bind_unix_endpoint(context, address)?,
        HostNetworkOperation::ManagedBindConnectedUnix { socket_id, address } => {
            bind_connected_unix_endpoint(context, socket_id.as_str(), address)?
        }
        HostNetworkOperation::ManagedReserveTcpPort { host, port } => {
            reserve_tcp_port(context, host.as_ref().map(|host| host.as_str()), port)?
        }
        HostNetworkOperation::ManagedReleaseTcpPort { reservation_id } => {
            context
                .process
                .tcp_port_reservations
                .remove(reservation_id.as_str());
            Value::Null
        }
        HostNetworkOperation::ManagedConnect { endpoint } => {
            return connect_endpoint(context, endpoint);
        }
        HostNetworkOperation::ManagedListen { endpoint } => listen_endpoint(context, endpoint)?,
        other => {
            return Err(SidecarError::host(
                "EINVAL",
                format!("managed endpoint reactor received unsupported operation: {other:?}"),
            ))
        }
    };
    Ok(HostServiceResponse::Json(response))
}

fn bind_unix_endpoint<B>(
    context: ManagedEndpointServiceContext<'_, B>,
    address: ManagedUnixAddress,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let (path, abstract_hex, autobind) = unix_address_parts(&address);
    context.bridge.require_network_access(
        context.vm_id,
        agentos_kernel::permissions::NetworkOperation::Listen,
        format_unix_socket_resource(path, abstract_hex, autobind),
    )?;
    let pending = reserve_capability(&context.capabilities, CapabilityKind::UnixListener)?;
    let listener_id = context.process.allocate_unix_listener_id();
    let registry_binding_id = guest_unix_binding_id(context.process.kernel_pid, &listener_id);
    let mut listener = match address {
        ManagedUnixAddress::Autobind => {
            let mut bound = None;
            for nonce in 0..4096 {
                let guest_name =
                    guest_autobind_unix_name(context.process.kernel_pid, &listener_id, nonce);
                let host_name = host_abstract_unix_name(context.socket_paths, &guest_name);
                register_guest_unix_binding(
                    &context.socket_paths.unix_bound_addresses,
                    &registry_binding_id,
                    &abstract_unix_host_address_key(&host_name),
                    GuestUnixAddress {
                        path: abstract_unix_node_path(&guest_name),
                        abstract_path_hex: Some(abstract_unix_name_hex(&guest_name)),
                    },
                    None,
                    None,
                )?;
                match ActiveUnixListener::bind_abstract_unlistened(
                    &host_name,
                    &guest_name,
                    registry_binding_id.clone(),
                    context.process.runtime_context.clone(),
                ) {
                    Ok(listener) => {
                        bound = Some(listener);
                        break;
                    }
                    Err(error) => {
                        rollback_guest_unix_binding(
                            &context.socket_paths.unix_bound_addresses,
                            &registry_binding_id,
                        )?;
                        if guest_error_code(&error) != Some("EADDRINUSE") {
                            return Err(error);
                        }
                    }
                }
            }
            bound.ok_or_else(|| {
                SidecarError::host(
                    "EADDRINUSE",
                    "Linux AF_UNIX autobind namespace exhausted after 4096 attempts",
                )
            })?
        }
        ManagedUnixAddress::AbstractHex(hex) => {
            let guest_name = decode_abstract_unix_name(hex.as_str())?;
            let host_name = host_abstract_unix_name(context.socket_paths, &guest_name);
            register_guest_unix_binding(
                &context.socket_paths.unix_bound_addresses,
                &registry_binding_id,
                &abstract_unix_host_address_key(&host_name),
                GuestUnixAddress {
                    path: abstract_unix_node_path(&guest_name),
                    abstract_path_hex: Some(abstract_unix_name_hex(&guest_name)),
                },
                None,
                None,
            )?;
            match ActiveUnixListener::bind_abstract_unlistened(
                &host_name,
                &guest_name,
                registry_binding_id.clone(),
                context.process.runtime_context.clone(),
            ) {
                Ok(listener) => listener,
                Err(error) => {
                    rollback_guest_unix_binding(
                        &context.socket_paths.unix_bound_addresses,
                        &registry_binding_id,
                    )?;
                    return Err(error);
                }
            }
        }
        ManagedUnixAddress::Path(path) => {
            let path = path.as_str();
            let (candidate_path, reported_path) = resolve_guest_unix_path(context.process, path)?;
            reject_host_mounted_unix_socket_path(context.socket_paths, &candidate_path)?;
            let canonical_candidate = context
                .kernel
                .resolve_unix_socket_bind_target_for_process(
                    EXECUTION_DRIVER_NAME,
                    context.process.kernel_pid,
                    &context.process.guest_cwd,
                    path,
                )
                .map_err(kernel_error)?;
            reject_host_mounted_unix_socket_path(context.socket_paths, &canonical_candidate)?;
            let node = context
                .kernel
                .bind_unix_socket_path_for_process(
                    EXECUTION_DRIVER_NAME,
                    context.process.kernel_pid,
                    &context.process.guest_cwd,
                    path,
                )
                .map_err(kernel_error)?;
            let guest_path = node.canonical_path;
            let host_path = allocate_guest_socket_host_path(
                context.socket_paths,
                context.process.kernel_pid,
                &listener_id,
                &guest_path,
            );
            if let Err(error) = register_guest_unix_binding(
                &context.socket_paths.unix_bound_addresses,
                &registry_binding_id,
                &pathname_unix_host_address_key(&host_path),
                GuestUnixAddress {
                    path: reported_path.clone(),
                    abstract_path_hex: None,
                },
                Some((node.stat.dev, node.stat.ino)),
                Some(host_path.clone()),
            ) {
                if let Err(rollback_error) = context.kernel.remove_file(&guest_path) {
                    return Err(SidecarError::Execution(format!(
                        "{error}; failed to roll back Unix socket node {guest_path}: {}",
                        kernel_error(rollback_error)
                    )));
                }
                return Err(error);
            }
            match ActiveUnixListener::bind_unlistened(
                &host_path,
                &reported_path,
                registry_binding_id.clone(),
                context.process.runtime_context.clone(),
            ) {
                Ok(mut listener) => {
                    listener.guest_node_path = Some(guest_path);
                    listener
                }
                Err(error) => {
                    rollback_guest_unix_path_binding(
                        &context.socket_paths.unix_bound_addresses,
                        &registry_binding_id,
                        context.kernel,
                        &guest_path,
                        &host_path,
                    )?;
                    return Err(error);
                }
            }
        }
    };
    listener
        .registry_binding_id
        .clone_from(&registry_binding_id);
    let local_path = listener.path.clone();
    let local_abstract_path_hex = listener.abstract_path_hex.clone();
    let capability_key = NativeCapabilityKey::UnixListener(listener_id.clone());
    let identity = commit_process_capability(
        context.process,
        pending,
        capability_key.clone(),
        listener_id.clone(),
        None,
    )?;
    listener.retain_description_lease(
        context
            .process
            .shared_capability_lease(&capability_key)
            .expect("committed Unix listener capability lease"),
    );
    listener.set_event_pusher(
        context
            .process
            .execution
            .execution_wake_handle(context.process.kernel_handle.runtime_identity()),
        Some(identity),
        Arc::clone(&context.process.process_event_notify),
    );
    context
        .process
        .unix_listeners
        .insert(listener_id.clone(), listener);
    Ok(json!({
        "serverId": listener_id,
        "capabilityId": identity.0,
        "capabilityGeneration": identity.1,
        "localPath": local_path,
        "localAbstractPathHex": local_abstract_path_hex,
    }))
}

fn bind_connected_unix_endpoint<B>(
    context: ManagedEndpointServiceContext<'_, B>,
    socket_id: &str,
    address: ManagedUnixAddress,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let (path, abstract_hex, autobind) = unix_address_parts(&address);
    context.bridge.require_network_access(
        context.vm_id,
        agentos_kernel::permissions::NetworkOperation::Listen,
        format_unix_socket_resource(path, abstract_hex, autobind),
    )?;
    let binding_id = guest_unix_binding_id(
        context.process.kernel_pid,
        &format!("connected:{socket_id}"),
    );
    let socket =
        context.process.unix_sockets.get(socket_id).ok_or_else(|| {
            SidecarError::host("EBADF", format!("unknown Unix socket {socket_id}"))
        })?;
    if socket.local_registry_binding_id.is_some() {
        return Err(sidecar_net_error(std::io::Error::from_raw_os_error(
            libc::EINVAL,
        )));
    }
    let remote_registry_binding_id = socket.remote_registry_binding_id.clone();
    let peer_can_observe_late_bind =
        guest_unix_connection_peer_open(socket.connection_state.as_ref());

    match address {
        ManagedUnixAddress::Autobind | ManagedUnixAddress::AbstractHex(_) => {
            let explicit_name = match &address {
                ManagedUnixAddress::AbstractHex(hex) => {
                    Some(decode_abstract_unix_name(hex.as_str())?)
                }
                ManagedUnixAddress::Autobind => None,
                ManagedUnixAddress::Path(_) => unreachable!(),
            };
            let attempts = if explicit_name.is_some() { 1 } else { 4096 };
            let mut bound_name = None;
            for nonce in 0..attempts {
                let guest_name = explicit_name.clone().unwrap_or_else(|| {
                    guest_autobind_unix_name(context.process.kernel_pid, &binding_id, nonce)
                        .to_vec()
                });
                let host_name = host_abstract_unix_name(context.socket_paths, &guest_name);
                register_guest_unix_binding(
                    &context.socket_paths.unix_bound_addresses,
                    &binding_id,
                    &abstract_unix_host_address_key(&host_name),
                    GuestUnixAddress {
                        path: abstract_unix_node_path(&guest_name),
                        abstract_path_hex: Some(abstract_unix_name_hex(&guest_name)),
                    },
                    None,
                    None,
                )?;
                if peer_can_observe_late_bind {
                    let target_binding_id = remote_registry_binding_id
                        .as_deref()
                        .expect("tracked Unix connection has a target binding");
                    if let Err(error) = queue_guest_unix_peer(
                        &context.socket_paths.unix_bound_addresses,
                        &binding_id,
                        target_binding_id,
                    ) {
                        rollback_guest_unix_binding(
                            &context.socket_paths.unix_bound_addresses,
                            &binding_id,
                        )?;
                        return Err(error);
                    }
                }
                let result = context
                    .process
                    .unix_sockets
                    .get_mut(socket_id)
                    .expect("validated Unix socket remains registered")
                    .bind_abstract(&host_name, &guest_name, &binding_id);
                match result {
                    Ok(()) => {
                        bound_name = Some(guest_name);
                        break;
                    }
                    Err(error) => {
                        rollback_guest_unix_binding(
                            &context.socket_paths.unix_bound_addresses,
                            &binding_id,
                        )?;
                        if explicit_name.is_some() || guest_error_code(&error) != Some("EADDRINUSE")
                        {
                            return Err(error);
                        }
                    }
                }
            }
            let guest_name = bound_name.ok_or_else(|| {
                sidecar_net_error(std::io::Error::from_raw_os_error(libc::EADDRINUSE))
            })?;
            Ok(json!({
                "localPath": abstract_unix_node_path(&guest_name),
                "localAbstractPathHex": abstract_unix_name_hex(&guest_name),
            }))
        }
        ManagedUnixAddress::Path(path) => {
            let path = path.as_str();
            let (candidate_path, reported_path) = resolve_guest_unix_path(context.process, path)?;
            reject_host_mounted_unix_socket_path(context.socket_paths, &candidate_path)?;
            let canonical_candidate = context
                .kernel
                .resolve_unix_socket_bind_target_for_process(
                    EXECUTION_DRIVER_NAME,
                    context.process.kernel_pid,
                    &context.process.guest_cwd,
                    path,
                )
                .map_err(kernel_error)?;
            reject_host_mounted_unix_socket_path(context.socket_paths, &canonical_candidate)?;
            let node = context
                .kernel
                .bind_unix_socket_path_for_process(
                    EXECUTION_DRIVER_NAME,
                    context.process.kernel_pid,
                    &context.process.guest_cwd,
                    path,
                )
                .map_err(kernel_error)?;
            let guest_path = node.canonical_path;
            let host_path = allocate_guest_socket_host_path(
                context.socket_paths,
                context.process.kernel_pid,
                &binding_id,
                &guest_path,
            );
            if let Err(error) = register_guest_unix_binding(
                &context.socket_paths.unix_bound_addresses,
                &binding_id,
                &pathname_unix_host_address_key(&host_path),
                GuestUnixAddress {
                    path: reported_path.clone(),
                    abstract_path_hex: None,
                },
                Some((node.stat.dev, node.stat.ino)),
                Some(host_path.clone()),
            ) {
                if let Err(rollback_error) = context.kernel.remove_file(&guest_path) {
                    return Err(SidecarError::Execution(format!(
                        "{error}; failed to roll back Unix socket node {guest_path}: {}",
                        kernel_error(rollback_error)
                    )));
                }
                return Err(error);
            }
            if peer_can_observe_late_bind {
                let target_binding_id = remote_registry_binding_id
                    .as_deref()
                    .expect("tracked Unix connection has a target binding");
                if let Err(error) = queue_guest_unix_peer(
                    &context.socket_paths.unix_bound_addresses,
                    &binding_id,
                    target_binding_id,
                ) {
                    rollback_guest_unix_path_binding(
                        &context.socket_paths.unix_bound_addresses,
                        &binding_id,
                        context.kernel,
                        &guest_path,
                        &host_path,
                    )?;
                    return Err(error);
                }
            }
            if let Err(error) = context
                .process
                .unix_sockets
                .get_mut(socket_id)
                .expect("validated Unix socket remains registered")
                .bind_path(&host_path, &reported_path, &binding_id)
            {
                rollback_guest_unix_path_binding(
                    &context.socket_paths.unix_bound_addresses,
                    &binding_id,
                    context.kernel,
                    &guest_path,
                    &host_path,
                )?;
                return Err(error);
            }
            Ok(json!({ "localPath": reported_path }))
        }
    }
}

fn reserve_tcp_port<B>(
    context: ManagedEndpointServiceContext<'_, B>,
    host: Option<&str>,
    requested_port: Option<u16>,
) -> Result<Value, SidecarError> {
    let (family, _bind_host, guest_host) = normalize_tcp_listen_host(host)?;
    let port = allocate_guest_listen_port(
        requested_port.unwrap_or(0),
        family,
        &context.socket_paths.used_tcp_guest_ports,
        context.socket_paths.listen_policy,
    )?;
    let reservation_id = context.process.allocate_tcp_port_reservation_id();
    context
        .process
        .tcp_port_reservations
        .insert(reservation_id.clone(), (family, port));
    Ok(json!({
        "reservationId": reservation_id,
        "localAddress": guest_host,
        "localPort": port,
        "family": match family {
            SocketFamily::Ipv4 => "IPv4",
            SocketFamily::Ipv6 => "IPv6",
        },
    }))
}

fn connect_endpoint<B>(
    context: ManagedEndpointServiceContext<'_, B>,
    endpoint: ManagedTcpEndpoint,
) -> Result<HostServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if let Some(address) = endpoint.unix {
        return connect_unix_endpoint(context, address, endpoint.bound_server_id);
    }

    let port = endpoint.port.ok_or_else(|| {
        SidecarError::host(
            "EINVAL",
            "net.connect requires either a Unix address or port",
        )
    })?;
    let host = endpoint
        .host
        .as_ref()
        .map_or("localhost", |host| host.as_str());
    let is_http_loopback_target = is_loopback_socket_host(host)
        && [SocketFamily::Ipv4, SocketFamily::Ipv6]
            .iter()
            .any(|family| {
                context
                    .socket_paths
                    .http_loopback_target(*family, port)
                    .is_some()
            });
    if !is_http_loopback_target {
        context.bridge.require_network_access(
            context.vm_id,
            agentos_kernel::permissions::NetworkOperation::Http,
            format_tcp_resource(host, port),
        )?;
        let resolved = resolve_tcp_connect_addr(
            context.bridge,
            context.kernel,
            context.vm_id,
            context.dns,
            host,
            port,
            None,
            context.socket_paths,
        )?;
        if !resolved.use_kernel_loopback {
            let pending = reserve_capability(&context.capabilities, CapabilityKind::TcpSocket)?;
            return defer_native_tcp_connect(
                context.process,
                context.call_id,
                pending,
                resolved,
                endpoint
                    .local_reservation
                    .map(|reservation| reservation.into_string()),
            );
        }
    }

    let pending = reserve_capability(&context.capabilities, CapabilityKind::TcpSocket)?;
    let local_reservation_id = endpoint
        .local_reservation
        .as_ref()
        .map(|reservation| reservation.as_str());
    let local_reservation = local_reservation_id.and_then(|id| {
        context
            .process
            .tcp_port_reservations
            .remove(id)
            .map(|reservation| (id.to_owned(), reservation))
    });

    if is_loopback_socket_host(host) {
        let families = [SocketFamily::Ipv4, SocketFamily::Ipv6];
        if let Some((family, target)) = families.iter().find_map(|family| {
            context
                .socket_paths
                .http_loopback_target(*family, port)
                .map(|target| (*family, target))
        }) {
            if let Some((reservation_id, reservation)) = local_reservation {
                context
                    .process
                    .tcp_port_reservations
                    .insert(reservation_id, reservation);
            }
            drop(pending);
            let remote_address = match family {
                SocketFamily::Ipv4 => "127.0.0.1",
                SocketFamily::Ipv6 => "::1",
            };
            return Ok(json!({
                "loopbackHttpTarget": {
                    "processId": target.process_id.clone(),
                    "serverId": target.server_id,
                    "host": remote_address,
                    "port": port,
                },
                "localAddress": remote_address,
                "localPort": endpoint.local_port.unwrap_or(0),
                "remoteAddress": remote_address,
                "remotePort": port,
                "remoteFamily": match family {
                    SocketFamily::Ipv4 => "IPv4",
                    SocketFamily::Ipv6 => "IPv6",
                },
            })
            .into());
        }
    }

    let connect_result = ActiveTcpSocket::connect(ActiveTcpConnectRequest {
        bridge: context.bridge,
        kernel: context.kernel,
        kernel_pid: context.process.kernel_pid,
        vm_id: context.vm_id,
        dns: context.dns,
        host,
        port,
        family: None,
        local_address: endpoint
            .local_address
            .as_ref()
            .map(|address| address.as_str()),
        local_port: endpoint.local_port,
        local_reservation: local_reservation
            .as_ref()
            .map(|(_, reservation)| *reservation),
        context: context.socket_paths,
        resources: context.capabilities.resources(),
        runtime_context: context.process.runtime_context.clone(),
        reactor_limits: reactor_io_limits(&context.process.limits),
    });
    let socket = match connect_result {
        Ok(socket) => socket,
        Err(error) => {
            if let Some((reservation_id, reservation)) = local_reservation {
                context
                    .process
                    .tcp_port_reservations
                    .insert(reservation_id, reservation);
            }
            return Err(error);
        }
    };
    let socket_id = context.process.allocate_tcp_socket_id();
    let local_addr = socket.guest_local_addr;
    let remote_addr = socket.guest_remote_addr;
    let capability_key = NativeCapabilityKey::TcpSocket(socket_id.clone());
    let identity = match commit_process_capability(
        context.process,
        pending,
        capability_key.clone(),
        socket_id.clone(),
        socket.kernel_socket_id,
    ) {
        Ok(identity) => identity,
        Err(error) => {
            if let Err(cleanup_error) = socket.close(context.kernel, context.process.kernel_pid) {
                eprintln!(
                    "ERR_AGENTOS_SOCKET_CLEANUP: failed to close connected socket after capability commit failure: {cleanup_error}"
                );
            }
            return Err(error);
        }
    };
    socket.set_event_pusher(
        context
            .process
            .execution
            .execution_wake_handle(context.process.kernel_handle.runtime_identity()),
        Some(identity),
        Arc::clone(&context.process.process_event_notify),
    );
    socket.set_fairness_identity(
        context
            .process
            .capability_fairness_identity(&capability_key),
    )?;
    socket.retain_description_lease(
        context
            .process
            .shared_capability_lease(&capability_key)
            .expect("committed socket capability lease"),
    );
    register_kernel_readiness_target(
        &context.kernel_readiness,
        socket.kernel_socket_id,
        context
            .process
            .execution
            .execution_wake_handle(context.process.kernel_handle.runtime_identity()),
        Some(Arc::clone(&socket.read_event_notify)),
        context
            .process
            .capability_readiness_identity(&capability_key),
        socket_id.clone(),
        KernelSocketReadinessEvent::Data,
    );
    context
        .process
        .tcp_sockets
        .insert(socket_id.clone(), socket);
    Ok(json!({
        "socketId": socket_id,
        "capabilityId": identity.0,
        "capabilityGeneration": identity.1,
        "localAddress": local_addr.ip().to_string(),
        "localPort": local_addr.port(),
        "remoteAddress": remote_addr.ip().to_string(),
        "remotePort": remote_addr.port(),
        "remoteFamily": socket_addr_family(&remote_addr),
    })
    .into())
}

fn connect_unix_endpoint<B>(
    context: ManagedEndpointServiceContext<'_, B>,
    address: ManagedUnixAddress,
    bound_server_id: Option<agentos_execution::host::BoundedString>,
) -> Result<HostServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let (path, abstract_hex, autobind) = unix_address_parts(&address);
    if autobind {
        return Err(SidecarError::host(
            "EINVAL",
            "net.connect does not accept an autobind remote address",
        ));
    }
    context.bridge.require_network_access(
        context.vm_id,
        agentos_kernel::permissions::NetworkOperation::Http,
        format_unix_socket_resource(path, abstract_hex, false),
    )?;
    let (target, target_binding_id, remote_address) = if let Some(hex) = abstract_hex {
        let guest_name = decode_abstract_unix_name(hex)?;
        let host_name = host_abstract_unix_name(context.socket_paths, &guest_name);
        let target = guest_unix_binding_for_host_key(
            &context.socket_paths.unix_bound_addresses,
            &abstract_unix_host_address_key(&host_name),
        )?
        .ok_or_else(|| sidecar_net_error(std::io::Error::from_raw_os_error(libc::ECONNREFUSED)))?;
        (
            NativeUnixConnectTarget::Abstract(host_name.to_vec()),
            target.0,
            target.1,
        )
    } else {
        let path = path.expect("validated Unix path");
        let (candidate_path, _) = resolve_guest_unix_path(context.process, path)?;
        reject_host_mounted_unix_socket_path(context.socket_paths, &candidate_path)?;
        let node = context
            .kernel
            .resolve_unix_socket_connect_target_for_process(
                EXECUTION_DRIVER_NAME,
                context.process.kernel_pid,
                &context.process.guest_cwd,
                path,
            )
            .map_err(kernel_error)?;
        reject_host_mounted_unix_socket_path(context.socket_paths, &node.canonical_path)?;
        let (host_path, binding_id, address) =
            guest_unix_path_target(context.socket_paths, (node.stat.dev, node.stat.ino))?
                .ok_or_else(|| {
                    sidecar_net_error(std::io::Error::from_raw_os_error(libc::ECONNREFUSED))
                })?;
        (
            NativeUnixConnectTarget::Path(host_path),
            binding_id,
            address,
        )
    };
    let pending = reserve_capability(&context.capabilities, CapabilityKind::UnixSocket)?;
    let bound_listener = if let Some(listener_id) = bound_server_id {
        let listener_id = listener_id.into_string();
        let listener = context
            .process
            .unix_listeners
            .remove(&listener_id)
            .ok_or_else(|| {
                SidecarError::host("EBADF", format!("unknown bound Unix socket {listener_id}"))
            })?;
        if listener.acceptor_started || listener.bound_socket.is_none() {
            context.process.unix_listeners.insert(listener_id, listener);
            return Err(sidecar_net_error(std::io::Error::from_raw_os_error(
                libc::EINVAL,
            )));
        }
        Some((listener_id, listener))
    } else {
        None
    };
    defer_native_unix_connect(
        context.process,
        context.call_id,
        pending,
        target,
        remote_address,
        Arc::clone(&context.socket_paths.unix_bound_addresses),
        target_binding_id,
        bound_listener,
    )
}

fn listen_endpoint<B>(
    context: ManagedEndpointServiceContext<'_, B>,
    endpoint: ManagedTcpEndpoint,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if let Some(listener_id) = endpoint.bound_server_id.as_ref() {
        if endpoint.unix.is_some() || endpoint.host.is_some() || endpoint.port.is_some() {
            return Err(SidecarError::host(
                "EINVAL",
                "net.listen boundServerId cannot be combined with an address",
            ));
        }
        return listen_bound_unix_endpoint(context, listener_id.as_str(), endpoint.backlog);
    }

    if let Some(address) = endpoint.unix {
        // The typed bind operation is the single implementation of Unix
        // namespace registration. Promote its unlistened socket immediately;
        // no executor-visible state exists between these two owner-thread
        // mutations.
        let bound = bind_unix_endpoint(
            ManagedEndpointServiceContext {
                bridge: context.bridge,
                vm_id: context.vm_id,
                dns: context.dns,
                socket_paths: context.socket_paths,
                kernel: &mut *context.kernel,
                kernel_readiness: context.kernel_readiness.clone(),
                process: &mut *context.process,
                capabilities: context.capabilities.clone(),
                call_id: context.call_id,
            },
            address,
        )?;
        let listener_id = bound
            .get("serverId")
            .and_then(Value::as_str)
            .ok_or_else(|| SidecarError::host("EIO", "Unix bind omitted serverId"))?
            .to_owned();
        let mut listened = listen_bound_unix_endpoint(context, &listener_id, endpoint.backlog)?;
        if let Some(fields) = listened.as_object_mut() {
            if let Some(local_path) = fields.get("localPath").cloned() {
                fields.insert("path".to_owned(), local_path);
            }
        }
        return Ok(listened);
    }

    let pending = reserve_capability(&context.capabilities, CapabilityKind::TcpListener)?;
    let host = endpoint.host.as_ref().map(|host| host.as_str());
    let (family, bind_host, guest_host) = normalize_tcp_listen_host(host)?;
    let requested_port = endpoint.port.unwrap_or(0);
    context.bridge.require_network_access(
        context.vm_id,
        agentos_kernel::permissions::NetworkOperation::Listen,
        format_tcp_resource(bind_host, requested_port),
    )?;
    let local_reservation_id = endpoint
        .local_reservation
        .as_ref()
        .map(|reservation| reservation.as_str());
    let local_reservation = local_reservation_id.and_then(|id| {
        context
            .process
            .tcp_port_reservations
            .remove(id)
            .map(|reservation| (id.to_owned(), reservation))
    });
    let port = if requested_port != 0
        && local_reservation
            .as_ref()
            .map(|(_, reservation)| *reservation)
            == Some((family, requested_port))
    {
        requested_port
    } else {
        allocate_guest_listen_port(
            requested_port,
            family,
            &context.socket_paths.used_tcp_guest_ports,
            context.socket_paths.listen_policy,
        )?
    };
    let listener = match ActiveTcpListener::bind_kernel(
        context.kernel,
        context.process.kernel_pid,
        guest_host,
        port,
        endpoint.backlog,
    ) {
        Ok(listener) => listener,
        Err(error) => {
            if let Some((reservation_id, reservation)) = local_reservation {
                context
                    .process
                    .tcp_port_reservations
                    .insert(reservation_id, reservation);
            }
            return Err(error);
        }
    };
    let listener_id = context.process.allocate_tcp_listener_id();
    let local_addr = listener.guest_local_addr();
    let capability_key = NativeCapabilityKey::TcpListener(listener_id.clone());
    let identity = match commit_process_capability(
        context.process,
        pending,
        capability_key.clone(),
        listener_id.clone(),
        listener.kernel_socket_id,
    ) {
        Ok(identity) => identity,
        Err(error) => {
            if let Err(cleanup_error) = listener.close(context.kernel, context.process.kernel_pid) {
                eprintln!(
                    "ERR_AGENTOS_SOCKET_CLEANUP: failed to close listener after capability commit failure: {cleanup_error}"
                );
            }
            return Err(error);
        }
    };
    listener.retain_description_lease(
        context
            .process
            .shared_capability_lease(&capability_key)
            .expect("committed TCP listener capability lease"),
    );
    register_kernel_readiness_target(
        &context.kernel_readiness,
        listener.kernel_socket_id,
        context
            .process
            .execution
            .execution_wake_handle(context.process.kernel_handle.runtime_identity()),
        None,
        context
            .process
            .capability_readiness_identity(&capability_key),
        listener_id.clone(),
        KernelSocketReadinessEvent::Accept,
    );
    context
        .process
        .tcp_listeners
        .insert(listener_id.clone(), listener);
    Ok(json!({
        "serverId": listener_id,
        "capabilityId": identity.0,
        "capabilityGeneration": identity.1,
        "localAddress": local_addr.ip().to_string(),
        "localPort": local_addr.port(),
        "family": socket_addr_family(&local_addr),
    }))
}

fn listen_bound_unix_endpoint<B>(
    context: ManagedEndpointServiceContext<'_, B>,
    listener_id: &str,
    backlog: Option<u32>,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let listener = context
        .process
        .unix_listeners
        .remove(listener_id)
        .ok_or_else(|| {
            SidecarError::host("EBADF", format!("unknown bound Unix socket {listener_id}"))
        })?;
    let local_path = listener.path.clone();
    let local_abstract_path_hex = listener.abstract_path_hex.clone();
    let listener = match listener.listen_bound(
        context.socket_paths.clone(),
        backlog,
        context.capabilities.clone(),
        context.process.runtime_context.clone(),
        reactor_io_limits(&context.process.limits),
    ) {
        Ok(listener) => listener,
        Err(error) => {
            context
                .process
                .release_capability_if_present(&NativeCapabilityKey::UnixListener(
                    listener_id.to_owned(),
                ));
            return Err(error);
        }
    };
    let capability_key = NativeCapabilityKey::UnixListener(listener_id.to_owned());
    let identity = context
        .process
        .capability_readiness_identity(&capability_key)
        .ok_or_else(|| {
            SidecarError::host(
                "EIO",
                format!("missing capability for bound Unix socket {listener_id}"),
            )
        })?;
    listener.set_event_pusher(
        context
            .process
            .execution
            .execution_wake_handle(context.process.kernel_handle.runtime_identity()),
        Some(identity),
        Arc::clone(&context.process.process_event_notify),
    );
    context
        .process
        .unix_listeners
        .insert(listener_id.to_owned(), listener);
    Ok(json!({
        "serverId": listener_id,
        "capabilityId": identity.0,
        "capabilityGeneration": identity.1,
        "localPath": local_path,
        "localAbstractPathHex": local_abstract_path_hex,
    }))
}

pub(in crate::execution) fn relisten_managed_unix_endpoint<B>(
    context: ManagedEndpointServiceContext<'_, B>,
    listener_id: &str,
    backlog: u32,
) -> Result<HostServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let reactor_limits = reactor_io_limits(&context.process.limits);
    let (local_path, local_abstract_path_hex) = {
        let listener = context
            .process
            .unix_listeners
            .get_mut(listener_id)
            .ok_or_else(|| {
                SidecarError::host("EBADF", format!("unknown Unix listener {listener_id}"))
            })?;
        listener.relisten(
            &context.socket_paths.unix_bound_addresses,
            backlog,
            reactor_limits,
        )?;
        (listener.path.clone(), listener.abstract_path_hex.clone())
    };
    let capability_key = NativeCapabilityKey::UnixListener(listener_id.to_owned());
    let identity = context
        .process
        .capability_readiness_identity(&capability_key)
        .ok_or_else(|| {
            SidecarError::host(
                "EIO",
                format!("missing capability for Unix listener {listener_id}"),
            )
        })?;
    Ok(HostServiceResponse::Json(json!({
        "serverId": listener_id,
        "capabilityId": identity.0,
        "capabilityGeneration": identity.1,
        "localPath": local_path,
        "localAbstractPathHex": local_abstract_path_hex,
    })))
}

fn unix_address_parts(address: &ManagedUnixAddress) -> (Option<&str>, Option<&str>, bool) {
    match address {
        ManagedUnixAddress::Path(path) => (Some(path.as_str()), None, false),
        ManagedUnixAddress::AbstractHex(hex) => (None, Some(hex.as_str()), false),
        ManagedUnixAddress::Autobind => (None, None, true),
    }
}
