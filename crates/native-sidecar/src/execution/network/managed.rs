use super::super::*;
use crate::state::DeferredRpcError;

fn managed_would_block_value() -> Value {
    json!({ "kind": "wouldBlock" })
}

pub(in crate::execution) struct ManagedNetworkServiceContext<'a> {
    pub(in crate::execution) vm_id: &'a str,
    pub(in crate::execution) socket_paths: &'a SocketPathContext,
    pub(in crate::execution) kernel: &'a mut SidecarKernel,
    pub(in crate::execution) kernel_readiness: KernelSocketReadinessRegistry,
    pub(in crate::execution) process: &'a mut ActiveProcess,
    pub(in crate::execution) capabilities: CapabilityRegistry,
}

pub(in crate::execution) fn service_managed_network_operation(
    context: ManagedNetworkServiceContext<'_>,
    operation: agentos_execution::host::NetworkOperation,
) -> Result<HostServiceResponse, SidecarError> {
    match operation {
        agentos_execution::host::NetworkOperation::ManagedPoll { socket_id, wait_ms } => {
            managed_socket_poll(context, socket_id.as_str(), wait_ms)
        }
        agentos_execution::host::NetworkOperation::ManagedRead {
            socket_id,
            max_bytes,
            peek,
            wait_ms,
        } => managed_socket_read(context, socket_id.as_str(), max_bytes, peek, wait_ms),
        agentos_execution::host::NetworkOperation::ManagedWaitConnect { socket_id } => {
            let info = if let Some(socket) = context.process.tcp_sockets.get(socket_id.as_str()) {
                socket.socket_info()
            } else {
                context
                    .process
                    .unix_sockets
                    .get(socket_id.as_str())
                    .ok_or_else(|| {
                        SidecarError::host(
                            "EBADF",
                            format!("unknown net socket {}", socket_id.as_str()),
                        )
                    })?
                    .socket_info()
            };
            serde_json::to_string(&info)
                .map(Value::String)
                .map(HostServiceResponse::Json)
                .map_err(|error| SidecarError::host("EIO", format!("encode socket info: {error}")))
        }
        agentos_execution::host::NetworkOperation::ManagedWrite { socket_id, bytes } => {
            managed_socket_write(context, socket_id.as_str(), bytes.as_slice())
        }
        agentos_execution::host::NetworkOperation::ManagedDestroy { socket_id } => {
            if let Some(socket) = context.process.tcp_sockets.remove(socket_id.as_str()) {
                release_tcp_socket_handle(
                    context.process,
                    socket_id.as_str(),
                    socket,
                    context.kernel,
                    &context.kernel_readiness,
                );
            } else if let Some(socket) = context.process.unix_sockets.remove(socket_id.as_str()) {
                release_unix_socket_handle(
                    context.process,
                    socket_id.as_str(),
                    socket,
                    &context.socket_paths.unix_bound_addresses,
                );
            }
            Ok(Value::Null.into())
        }
        agentos_execution::host::NetworkOperation::ManagedTlsUpgrade {
            socket_id,
            options_json,
        } => managed_tls_upgrade(context, socket_id.as_str(), options_json.as_str()),
        agentos_execution::host::NetworkOperation::ManagedCloseListener { listener_id } => {
            managed_close_listener(context, listener_id.as_str())
        }
        agentos_execution::host::NetworkOperation::ManagedAccept { listener_id } => {
            managed_accept(context, listener_id.as_str())
        }
        other => Err(SidecarError::host(
            "EINVAL",
            format!("managed reactor received unsupported operation: {other:?}"),
        )),
    }
}

fn managed_socket_poll(
    mut context: ManagedNetworkServiceContext<'_>,
    socket_id: &str,
    _wait_ms: u64,
) -> Result<HostServiceResponse, SidecarError> {
    let read_state = prime_managed_socket_read_state(&mut context, socket_id)?;
    let state = read_state.lock().map_err(|_| {
        SidecarError::host(
            "ERR_AGENTOS_SOCKET_READ_STATE_POISONED",
            format!("managed socket {socket_id} read state lock poisoned"),
        )
    })?;
    if !state.bytes.is_empty() {
        return Ok(json!({ "readable": true }).into());
    }
    match state.terminal.as_ref() {
        Some(SocketReadTerminal::End) => Ok(json!({ "type": "end" }).into()),
        Some(SocketReadTerminal::Closed { had_error }) => Ok(json!({
            "type": "close",
            "hadError": had_error,
        })
        .into()),
        Some(SocketReadTerminal::Error { code, message }) => Err(SidecarError::host(
            code.as_deref().unwrap_or("EIO"),
            message.clone(),
        )),
        None => Ok(Value::Null.into()),
    }
}

fn managed_socket_read(
    mut context: ManagedNetworkServiceContext<'_>,
    socket_id: &str,
    max_bytes: u64,
    peek: bool,
    _wait_ms: u64,
) -> Result<HostServiceResponse, SidecarError> {
    let maximum = usize::try_from(max_bytes).map_err(|_| {
        SidecarError::host(
            "EOVERFLOW",
            format!("managed socket read length {max_bytes} exceeds usize"),
        )
    })?;
    if maximum == 0 {
        return Ok(HostServiceResponse::Raw(Vec::new()));
    }

    let read_state = prime_managed_socket_read_state(&mut context, socket_id)?;
    let mut state = read_state.lock().map_err(|_| {
        SidecarError::host(
            "ERR_AGENTOS_SOCKET_READ_STATE_POISONED",
            format!("managed socket {socket_id} read state lock poisoned"),
        )
    })?;
    if !state.bytes.is_empty() {
        let count = maximum.min(state.bytes.len());
        let payload = state.bytes.iter().take(count).copied().collect::<Vec<_>>();
        let source_reservations = state.source_reservations.clone();
        if !peek {
            state.bytes.drain(..count);
            if state.bytes.is_empty() {
                state.source_reservations.clear();
            }
        }
        return Ok(HostServiceResponse::SourceBackedRaw {
            payload,
            source_reservations,
        });
    }
    match state.terminal.as_ref() {
        Some(SocketReadTerminal::Error { code, message }) => Err(SidecarError::host(
            code.as_deref().unwrap_or("EIO"),
            message.clone(),
        )),
        Some(SocketReadTerminal::End | SocketReadTerminal::Closed { .. }) => Ok(Value::Null.into()),
        None => Ok(HostServiceResponse::Json(managed_would_block_value())),
    }
}

fn prime_managed_socket_read_state(
    context: &mut ManagedNetworkServiceContext<'_>,
    socket_id: &str,
) -> Result<Arc<Mutex<SocketReadState>>, SidecarError> {
    let trace_enabled = net_tcp_trace_enabled(&context.process.env);
    let (read_state, event) = if let Some(socket) = context.process.tcp_sockets.get_mut(socket_id) {
        let read_state = Arc::clone(&socket.read_state);
        let already_ready = {
            let state = read_state.lock().map_err(|_| {
                SidecarError::host(
                    "ERR_AGENTOS_SOCKET_READ_STATE_POISONED",
                    format!("managed socket {socket_id} read state lock poisoned"),
                )
            })?;
            !state.bytes.is_empty() || state.terminal.is_some()
        };
        if already_ready {
            return Ok(read_state);
        }
        socket.set_application_read_interest(true)?;
        let event = socket.poll(
            context.kernel,
            context.process.kernel_pid,
            Duration::ZERO,
            trace_enabled,
        )?;
        (read_state, event)
    } else if let Some(socket) = context.process.unix_sockets.get_mut(socket_id) {
        let read_state = Arc::clone(&socket.read_state);
        let already_ready = {
            let state = read_state.lock().map_err(|_| {
                SidecarError::host(
                    "ERR_AGENTOS_SOCKET_READ_STATE_POISONED",
                    format!("managed socket {socket_id} read state lock poisoned"),
                )
            })?;
            !state.bytes.is_empty() || state.terminal.is_some()
        };
        if already_ready {
            return Ok(read_state);
        }
        socket.set_application_read_interest(true)?;
        let event = socket.poll(Duration::ZERO)?;
        (read_state, event)
    } else {
        return Err(SidecarError::host(
            "EBADF",
            format!("unknown net socket {socket_id}"),
        ));
    };

    let Some(event) = event else {
        return Ok(read_state);
    };
    let mut state = read_state.lock().map_err(|_| {
        SidecarError::host(
            "ERR_AGENTOS_SOCKET_READ_STATE_POISONED",
            format!("managed socket {socket_id} read state lock poisoned"),
        )
    })?;
    match event {
        TcpSocketEvent::Data {
            bytes,
            reservation,
            mut source_reservations,
        } => {
            state.bytes.extend(bytes);
            source_reservations.push(reservation);
            state.source_reservations.extend(source_reservations);
        }
        TcpSocketEvent::End => state.terminal = Some(SocketReadTerminal::End),
        TcpSocketEvent::Close { had_error } => {
            state.terminal = Some(SocketReadTerminal::Closed { had_error });
        }
        TcpSocketEvent::Error { code, message } => {
            state.terminal = Some(SocketReadTerminal::Error { code, message });
        }
    }
    drop(state);
    Ok(read_state)
}

/// Re-probe one connected managed description without consuming guest-visible
/// bytes. Transport events are folded into the description-owned read state,
/// so a later read observes exactly the level that made poll return readable.
pub(in crate::execution) fn probe_managed_socket_readable(
    context: &mut ManagedNetworkServiceContext<'_>,
    socket_id: &str,
) -> Result<bool, SidecarError> {
    let read_state = prime_managed_socket_read_state(context, socket_id)?;
    let state = read_state.lock().map_err(|_| {
        SidecarError::host(
            "ERR_AGENTOS_SOCKET_READ_STATE_POISONED",
            format!("managed socket {socket_id} read state lock poisoned"),
        )
    })?;
    Ok(!state.bytes.is_empty() || state.terminal.is_some())
}

fn managed_socket_write(
    context: ManagedNetworkServiceContext<'_>,
    socket_id: &str,
    bytes: &[u8],
) -> Result<HostServiceResponse, SidecarError> {
    if let Some(socket) = context.process.tcp_sockets.get(socket_id) {
        let receiver = if socket.tls_mode.load(Ordering::SeqCst) {
            Some((
                socket.begin_tls_write(bytes)?,
                agentos_runtime::TaskClass::Tls,
            ))
        } else if socket.kernel_socket_id.is_none() {
            Some((
                socket.begin_plain_write(bytes)?,
                agentos_runtime::TaskClass::Socket,
            ))
        } else {
            None
        };
        if let Some((receiver, task_class)) = receiver {
            return Ok(HostServiceResponse::Deferred {
                receiver,
                timeout: (task_class == agentos_runtime::TaskClass::Tls)
                    .then_some(reactor_io_limits(&context.process.limits).operation_deadline),
                task_class,
            });
        }
        return context
            .process
            .tcp_sockets
            .get(socket_id)
            .expect("validated TCP socket remains registered")
            .write_all(context.kernel, context.process.kernel_pid, bytes)
            .map(|written| json!(written).into());
    }
    let socket =
        context.process.unix_sockets.get(socket_id).ok_or_else(|| {
            SidecarError::host("EBADF", format!("unknown net socket {socket_id}"))
        })?;
    Ok(HostServiceResponse::Deferred {
        receiver: socket.begin_plain_write(bytes)?,
        timeout: None,
        task_class: agentos_runtime::TaskClass::Socket,
    })
}

fn managed_tls_upgrade(
    context: ManagedNetworkServiceContext<'_>,
    socket_id: &str,
    options_json: &str,
) -> Result<HostServiceResponse, SidecarError> {
    let options: TlsBridgeOptions = serde_json::from_str(options_json)
        .map_err(|error| SidecarError::host("EINVAL", format!("invalid TLS options: {error}")))?;
    if context
        .process
        .capability_leases
        .contains_key(&NativeCapabilityKey::TlsSocket(socket_id.to_owned()))
    {
        return Err(SidecarError::host(
            "EALREADY",
            format!("TCP socket {socket_id} is already upgraded to TLS"),
        ));
    }
    let pending = reserve_capability(&context.capabilities, CapabilityKind::TlsTransport)?;
    let socket =
        context.process.tcp_sockets.get(socket_id).ok_or_else(|| {
            SidecarError::host("EBADF", format!("unknown TCP socket {socket_id}"))
        })?;
    let receiver = socket.upgrade_tls(
        context.vm_id,
        context.kernel,
        context.process.kernel_pid,
        options,
    )?;
    let kernel_socket_id = socket.kernel_socket_id;
    commit_process_capability(
        context.process,
        pending,
        NativeCapabilityKey::TlsSocket(socket_id.to_owned()),
        format!("tls-{socket_id}"),
        kernel_socket_id,
    )?;
    Ok(HostServiceResponse::Deferred {
        receiver,
        timeout: Some(reactor_io_limits(&context.process.limits).operation_deadline),
        task_class: agentos_runtime::TaskClass::Tls,
    })
}

fn managed_close_listener(
    context: ManagedNetworkServiceContext<'_>,
    listener_id: &str,
) -> Result<HostServiceResponse, SidecarError> {
    if let Some(listener) = context.process.tcp_listeners.remove(listener_id) {
        release_tcp_listener_handle(
            context.process,
            listener_id,
            listener,
            context.kernel,
            &context.kernel_readiness,
        )?;
        return Ok(Value::Null.into());
    }
    let listener = context
        .process
        .unix_listeners
        .remove(listener_id)
        .ok_or_else(|| {
            SidecarError::host("EBADF", format!("unknown net listener {listener_id}"))
        })?;
    release_unix_listener_capability(context.process, listener_id, &listener)?;
    if !listener.is_final_description_handle() {
        return Ok(Value::Null.into());
    }
    for socket in context
        .process
        .unix_sockets
        .values_mut()
        .filter(|socket| socket.listener_id.as_deref() == Some(listener_id))
    {
        socket.cache_remote_peer_metadata(&context.socket_paths.unix_bound_addresses)?;
    }
    close_pending_guest_unix_connections(
        &context.socket_paths.unix_bound_addresses,
        &listener.registry_binding_id,
    )?;
    release_guest_unix_binding(
        &context.socket_paths.unix_bound_addresses,
        &listener.registry_binding_id,
    )?;
    purge_guest_unix_target(
        &context.socket_paths.unix_bound_addresses,
        &listener.registry_binding_id,
    )?;
    let completion = listener.close();
    let deadline = reactor_io_limits(&context.process.limits).operation_deadline;
    let listener_id = listener_id.to_owned();
    let (respond_to, receiver) = tokio::sync::oneshot::channel();
    context
        .process
        .runtime_context
        .spawn(agentos_runtime::TaskClass::Listener, async move {
            let result = match crate::execution::operation_deadline_timeout(
                "Unix listener close",
                deadline,
                completion,
            )
            .await
            {
                Ok(Ok(())) => Ok(Value::Null),
                Ok(Err(_)) => Err(DeferredRpcError {
                    code: "ERR_AGENTOS_LISTENER_CLOSE".to_owned(),
                    message: format!(
                        "Unix listener {listener_id} close task ended without acknowledgement"
                    ),
                    details: None,
                }),
                Err(_) => Err(DeferredRpcError {
                    code: "ETIMEDOUT".to_owned(),
                    message: format!(
                        "Unix listener {listener_id} close exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                        deadline.as_millis()
                    ),
                    details: None,
                }),
            };
            if respond_to.send(result).is_err() {
                eprintln!(
                    "ERR_AGENTOS_LISTENER_CLOSE_COMPLETION_DROPPED: caller stopped waiting for Unix listener {listener_id}"
                );
            }
        })
        .map_err(SidecarError::from)?;
    Ok(HostServiceResponse::Deferred {
        receiver,
        timeout: None,
        task_class: agentos_runtime::TaskClass::Listener,
    })
}

fn managed_accept(
    context: ManagedNetworkServiceContext<'_>,
    listener_id: &str,
) -> Result<HostServiceResponse, SidecarError> {
    let trace_enabled = net_tcp_trace_enabled(&context.process.env);
    if let Some(listener) = context.process.tcp_listeners.get_mut(listener_id) {
        let pending_capability =
            reserve_capability(&context.capabilities, CapabilityKind::TcpSocket)?;
        return match listener.poll(
            context.kernel,
            context.process.kernel_pid,
            Duration::ZERO,
            trace_enabled,
        )? {
            Some(TcpListenerEvent::Connection(pending)) => {
                let PendingTcpSocket {
                    stream,
                    kernel_socket_id,
                    guest_local_addr,
                    guest_remote_addr,
                } = pending;
                let mut info = tcp_socket_info_value(&guest_local_addr, &guest_remote_addr);
                let mut socket = if let Some(stream) = stream {
                    ActiveTcpSocket::from_stream(
                        stream,
                        Some(listener_id.to_owned()),
                        guest_local_addr,
                        guest_remote_addr,
                        context.capabilities.resources(),
                        context.process.runtime_context.clone(),
                        reactor_io_limits(&context.process.limits),
                    )?
                } else {
                    ActiveTcpSocket::from_kernel(
                        kernel_socket_id.ok_or_else(|| {
                            SidecarError::host("EIO", "kernel TCP accept missing socket id")
                        })?,
                        Some(listener_id.to_owned()),
                        guest_local_addr,
                        guest_remote_addr,
                        context.capabilities.resources(),
                        context.process.runtime_context.clone(),
                        reactor_io_limits(&context.process.limits),
                    )
                };
                let socket_id = context.process.allocate_tcp_socket_id();
                let capability_key = NativeCapabilityKey::TcpSocket(socket_id.clone());
                let identity = match commit_process_capability(
                    context.process,
                    pending_capability,
                    capability_key.clone(),
                    socket_id.clone(),
                    socket.kernel_socket_id,
                ) {
                    Ok(identity) => identity,
                    Err(error) => {
                        if let Err(cleanup_error) =
                            socket.close(context.kernel, context.process.kernel_pid)
                        {
                            eprintln!(
                                "ERR_AGENTOS_SOCKET_CLEANUP: failed to close accepted socket after capability commit failure: {cleanup_error}"
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
                if let Value::Object(fields) = &mut info {
                    fields.insert("capabilityId".to_owned(), json!(identity.0));
                    fields.insert("capabilityGeneration".to_owned(), json!(identity.1));
                }
                socket.set_fairness_identity(
                    context
                        .process
                        .capability_fairness_identity(&capability_key),
                )?;
                socket.retain_description_lease(
                    context
                        .process
                        .shared_capability_lease(&capability_key)
                        .expect("committed TCP capability lease"),
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
                if let Some(listener) = context.process.tcp_listeners.get_mut(listener_id) {
                    socket.listener_connection_retirement =
                        Some(listener.register_connection(&socket_id));
                }
                context
                    .process
                    .tcp_sockets
                    .insert(socket_id.clone(), socket);
                encode_net_json_string(
                    json!({ "socketId": socket_id, "info": info }),
                    "net.server_accept",
                )
                .map(Into::into)
            }
            Some(TcpListenerEvent::Error { code, message }) => Err(SidecarError::host(
                code.as_deref().unwrap_or("EIO"),
                message,
            )),
            None => Ok(HostServiceResponse::Json(managed_would_block_value())),
        };
    }

    let target_binding_id = context
        .process
        .unix_listeners
        .get(listener_id)
        .ok_or_else(|| SidecarError::host("EBADF", format!("unknown net listener {listener_id}")))?
        .registry_binding_id
        .clone();
    let event = context
        .process
        .unix_listeners
        .get_mut(listener_id)
        .expect("validated Unix listener remains registered")
        .poll(Duration::ZERO)?;
    match event {
        Some(UnixListenerEvent::Connection {
            socket: mut pending,
            capability: pending_capability,
        }) => {
            let mut info = json!({
                "localPath": pending.local_path.clone(),
                "remotePath": pending.remote_path.clone(),
                "localAbstractPathHex": pending.local_abstract_path_hex.clone(),
                "remoteAbstractPathHex": pending.remote_abstract_path_hex.clone(),
            });
            let mut socket = ActiveUnixSocket::from_stream_with_metadata(
                pending.stream,
                Some(listener_id.to_owned()),
                pending.local_path,
                pending.remote_path,
                pending.local_abstract_path_hex,
                pending.remote_abstract_path_hex,
                None,
                None,
                context.capabilities.resources(),
                context.process.runtime_context.clone(),
                reactor_io_limits(&context.process.limits),
            )?;
            socket.connection_state = pending.connection_guard.state.take();
            socket.remote_registry_binding_id = Some(target_binding_id);
            let socket_id = context.process.allocate_unix_socket_id();
            let capability_key = NativeCapabilityKey::UnixSocket(socket_id.clone());
            let identity = commit_process_capability(
                context.process,
                pending_capability,
                capability_key.clone(),
                socket_id.clone(),
                None,
            )?;
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
                    .expect("committed Unix capability lease"),
            );
            if let Value::Object(fields) = &mut info {
                fields.insert("capabilityId".to_owned(), json!(identity.0));
                fields.insert("capabilityGeneration".to_owned(), json!(identity.1));
            }
            if let Some(listener) = context.process.unix_listeners.get_mut(listener_id) {
                socket.listener_connection_retirement =
                    Some(listener.register_connection(&socket_id));
            }
            context
                .process
                .unix_sockets
                .insert(socket_id.clone(), socket);
            encode_net_json_string(
                json!({ "socketId": socket_id, "info": info }),
                "net.server_accept",
            )
            .map(Into::into)
        }
        Some(UnixListenerEvent::Error { code, message }) => Err(SidecarError::host(
            code.as_deref().unwrap_or("EIO"),
            message,
        )),
        None => Ok(HostServiceResponse::Json(managed_would_block_value())),
    }
}
