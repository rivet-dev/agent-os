use super::*;

pub(super) trait DeferredResponseSettlement<T> {
    fn settle(self, value: T);
}

impl<T> DeferredResponseSettlement<T> for tokio::sync::oneshot::Sender<T> {
    fn settle(self, value: T) {
        if self.send(value).is_err() {
            eprintln!(
                "INFO_AGENTOS_STALE_DEFERRED_COMPLETION: deferred RPC waiter was dropped before settlement"
            );
        }
    }
}

pub(super) fn validate_guest_network_capability_alias(
    process: &ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<(), SidecarError> {
    if !(request.method.starts_with("net.")
        || request.method.starts_with("dgram.")
        || request.method.starts_with("tls."))
    {
        return Ok(());
    }

    if let Some(local_id) = request.args.first().and_then(Value::as_str) {
        for (key, kind) in [
            (
                NativeCapabilityKey::TcpSocket(local_id.to_owned()),
                CapabilityKind::TcpSocket,
            ),
            (
                NativeCapabilityKey::UnixSocket(local_id.to_owned()),
                CapabilityKind::UnixSocket,
            ),
            (
                NativeCapabilityKey::UdpSocket(local_id.to_owned()),
                CapabilityKind::UdpSocket,
            ),
            (
                NativeCapabilityKey::TcpListener(local_id.to_owned()),
                CapabilityKind::TcpListener,
            ),
            (
                NativeCapabilityKey::UnixListener(local_id.to_owned()),
                CapabilityKind::UnixListener,
            ),
            (
                NativeCapabilityKey::TlsSocket(local_id.to_owned()),
                CapabilityKind::TlsTransport,
            ),
        ] {
            if process.capability_leases.contains_key(&key) {
                process.validate_capability_alias(&key, kind)?;
            }
        }
    }

    let Some(id) = request.args.first().and_then(Value::as_u64) else {
        return Ok(());
    };
    let process_key = NativeCapabilityKey::HttpServer(id);
    if process.capability_leases.contains_key(&process_key) {
        process.validate_capability_alias(&process_key, CapabilityKind::TcpListener)?;
    }

    let state = process
        .http2
        .shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?;
    let generation = process.runtime_context.vm_generation().ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "ERR_AGENTOS_CAPABILITY_SESSION: process runtime is not VM-generation scoped",
        ))
    })?;
    for (key, kind) in [
        (
            NativeCapabilityKey::Http2Server(id),
            CapabilityKind::TcpListener,
        ),
        (
            NativeCapabilityKey::Http2Session(id),
            CapabilityKind::Http2Connection,
        ),
        (
            NativeCapabilityKey::Http2Stream(id),
            CapabilityKind::Http2Stream,
        ),
    ] {
        if let Some(lease) = state.capability_leases.get(&key) {
            lease
                .validate(generation, kind)
                .map_err(SidecarError::from)?;
        }
    }
    Ok(())
}

pub(super) fn closed_javascript_event_channel(message: &str) -> bool {
    message == "guest JavaScript event channel closed unexpectedly"
}

pub(super) fn closed_python_event_channel(message: &str) -> bool {
    message == "guest Python event channel closed unexpectedly"
}

pub(super) fn closed_wasm_event_channel(message: &str) -> bool {
    message == WasmExecutionError::EventChannelClosed.to_string()
}

pub(super) fn missing_vm_error(vm_id: &str) -> SidecarError {
    SidecarError::InvalidState(format!("VM {vm_id} is no longer active"))
}

pub(super) fn missing_process_error(vm_id: &str, process_id: &str) -> SidecarError {
    SidecarError::InvalidState(format!(
        "VM {vm_id} no longer has active process {process_id}"
    ))
}

/// Map a shared guest-kernel-call dispatcher error into a sidecar error,
/// preserving POSIX errno codes (`ECODE: message`) as kernel errors so guest
/// callers observe Linux-faithful failures, mirroring the filesystem path.
fn guest_kernel_core_error(error: agentos_native_sidecar_core::SidecarCoreError) -> SidecarError {
    let message = error.to_string();
    let is_errno = message.split_once(':').is_some_and(|(code, _)| {
        code.len() >= 2
            && code.starts_with('E')
            && code[1..]
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    });
    if is_errno {
        SidecarError::Kernel(message)
    } else {
        SidecarError::InvalidState(message)
    }
}

pub(super) fn is_broken_pipe_error(error: &SidecarError) -> bool {
    matches!(error, SidecarError::Execution(message) if message.contains("Broken pipe") || message.contains("os error 32") || message.contains("EPIPE"))
}

pub(super) fn javascript_child_process_gone_error(
    process_id: &str,
    child_path: &[&str],
) -> SidecarError {
    let child_label = if child_path.is_empty() {
        process_id.to_owned()
    } else {
        format!("{process_id}/{}", child_path.join("/"))
    };
    SidecarError::Execution(format!(
        "ECHILD: child_process {child_label} is no longer available"
    ))
}

pub(super) fn is_javascript_child_process_gone_error(error: &SidecarError) -> bool {
    matches!(
        error,
        SidecarError::Execution(message) if guest_errno_code(message) == Some("ECHILD")
    )
}

pub(super) fn missing_javascript_child_cleanup_result(
    next_child_process_id: usize,
    child_process_id: &str,
    operation: &str,
) -> Result<(), SidecarError> {
    let previously_allocated = child_process_id
        .strip_prefix("child-")
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|sequence| {
            sequence != 0
                && sequence <= next_child_process_id
                && child_process_id == format!("child-{sequence}")
        });
    if previously_allocated {
        return Ok(());
    }
    Err(SidecarError::InvalidState(format!(
        "unknown child process {child_process_id} during {operation}"
    )))
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod child_kill_result_tests {
    use super::missing_javascript_child_cleanup_result;

    #[test]
    fn cleanup_kill_ignores_reaped_child_but_rejects_unknown_id() {
        missing_javascript_child_cleanup_result(1, "child-1", "kill")
            .expect("a previously allocated child is confirmed gone");
        missing_javascript_child_cleanup_result(1, "child-1", "stdin close")
            .expect("closing stdin after a child exits is idempotent");
        assert!(
            missing_javascript_child_cleanup_result(1, "child-2", "kill")
                .expect_err("a never-allocated child must remain an error")
                .to_string()
                .contains("unknown child process child-2")
        );
        assert!(
            missing_javascript_child_cleanup_result(1, "child-01", "stdin close")
                .expect_err("a non-canonical child id must remain an error")
                .to_string()
                .contains("unknown child process child-01")
        );
    }
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) async fn resize_pty(
        &mut self,
        request: &RequestFrame,
        payload: ResizePtyRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        // Signal registrations are execution events. Consume them before the
        // resize so a handler installed immediately before the host request is
        // visible when the kernel-generated SIGWINCH is delivered below.
        self.drain_root_signal_state_events(&vm_id, &payload.process_id)?;

        let foreground_pgid = {
            let vm = self
                .vms
                .get_mut(&vm_id)
                .ok_or_else(|| missing_vm_error(&vm_id))?;
            let process = vm
                .active_processes
                .get_mut(&payload.process_id)
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "VM {vm_id} has no active process {}",
                        payload.process_id
                    ))
                })?;
            let Some(writer_fd) = process.kernel_stdin_writer_fd else {
                return Err(SidecarError::InvalidState(format!(
                    "process {} does not have a PTY",
                    payload.process_id
                )));
            };
            let foreground_pgid = vm
                .kernel
                .tcgetpgrp(EXECUTION_DRIVER_NAME, process.kernel_pid, writer_fd)
                .map_err(kernel_error)?;
            vm.kernel
                .pty_resize(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    writer_fd,
                    payload.cols,
                    payload.rows,
                )
                .map_err(kernel_error)?;
            foreground_pgid
        };

        self.deliver_kernel_process_group_signal_to_tracked_runtimes(
            &vm_id,
            foreground_pgid,
            "SIGWINCH",
        )?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::PtyResized(PtyResizedResponse {
                    process_id: payload.process_id,
                    cols: payload.cols,
                    rows: payload.rows,
                }),
            ),
            events: Vec::new(),
        })
    }

    fn drain_root_signal_state_events(
        &mut self,
        vm_id: &str,
        process_id: &str,
    ) -> Result<(), SidecarError> {
        let mut deferred = VecDeque::new();
        loop {
            let event = {
                let Some(vm) = self.vms.get_mut(vm_id) else {
                    break;
                };
                let Some(process) = vm.active_processes.get_mut(process_id) else {
                    break;
                };
                if let Some(event) = process.lease_pending_execution_event() {
                    Some(event)
                } else {
                    match process.try_poll_execution_event() {
                        Ok(event) => event,
                        Err(SidecarError::Execution(message))
                            if (process.runtime == GuestRuntimeKind::JavaScript
                                && closed_javascript_event_channel(&message))
                                || (process.runtime == GuestRuntimeKind::Python
                                    && closed_python_event_channel(&message))
                                || (process.runtime == GuestRuntimeKind::WebAssembly
                                    && closed_wasm_event_channel(&message)) =>
                        {
                            None
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            let Some(event) = event else {
                break;
            };
            match event.event() {
                ActiveExecutionEvent::SignalState {
                    signal,
                    registration,
                } => {
                    let signal = *signal;
                    let registration = registration.clone();
                    drop(event);
                    if let Some(vm) = self.vms.get_mut(vm_id) {
                        apply_process_signal_state_update(
                            &mut vm.signal_states,
                            process_id,
                            signal,
                            registration,
                        );
                    }
                }
                _ => deferred.push_back(event),
            }
        }

        if let Some(process) = self
            .vms
            .get_mut(vm_id)
            .and_then(|vm| vm.active_processes.get_mut(process_id))
        {
            for event in deferred.into_iter().rev() {
                process.requeue_pending_execution_event(event)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn write_stdin(
        &mut self,
        request: &RequestFrame,
        payload: WriteStdinRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self
            .vms
            .get_mut(&vm_id)
            .ok_or_else(|| missing_vm_error(&vm_id))?;
        let process = vm
            .active_processes
            .get_mut(&payload.process_id)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "VM {vm_id} has no active process {}",
                    payload.process_id
                ))
            })?;
        // For a TTY JavaScript process, host stdin must go ONLY to the kernel PTY
        // master (so line discipline + echo apply); feeding the in-process local
        // stdin bridge as well would double-deliver the input. Non-TTY JS (piped
        // stdin) still uses the local bridge; wasm/python always take the
        // streaming/no-op `write_stdin` path plus the kernel master write below.
        let tty_js =
            process.runtime == GuestRuntimeKind::JavaScript && process.tty_master_fd.is_some();
        if !tty_js {
            process.execution.write_stdin(&payload.chunk)?;
        }
        write_kernel_process_stdin(&mut vm.kernel, process, &payload.chunk)?;

        Ok(DispatchResult {
            response: stdin_written_response(
                request,
                payload.process_id,
                payload.chunk.len() as u64,
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

        let vm = self
            .vms
            .get_mut(&vm_id)
            .ok_or_else(|| missing_vm_error(&vm_id))?;
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
            response: stdin_closed_response(request, payload.process_id),
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
        require_vm_inspection_permission(
            &self.bridge,
            &vm_id,
            "network.inspect",
            "network",
            &socket_query_resource(SocketQueryKind::TcpListener, &payload),
        )?;

        let listener =
            find_socket_state_entry(self.vms.get(&vm_id), SocketQueryKind::TcpListener, &payload)?;

        Ok(DispatchResult {
            response: listener_snapshot_response(request, listener),
            events: Vec::new(),
        })
    }

    pub(crate) async fn get_process_snapshot(
        &mut self,
        request: &RequestFrame,
        _payload: GetProcessSnapshotRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        require_vm_inspection_permission(
            &self.bridge,
            &vm_id,
            "process.inspect",
            "process",
            "process://snapshot",
        )?;

        let processes = self
            .vms
            .get_mut(&vm_id)
            .map(|vm| {
                prune_exited_process_snapshots(vm);
                snapshot_vm_processes(vm)
            })
            .unwrap_or_default();

        Ok(DispatchResult {
            response: process_snapshot_response(request, processes),
            events: Vec::new(),
        })
    }

    pub(crate) async fn guest_kernel_call(
        &mut self,
        request: &RequestFrame,
        payload: GuestKernelCallRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("VM {vm_id} no longer exists for guest kernel call"))
        })?;
        let kernel_pid = vm
            .active_processes
            .get(&payload.execution_id)
            .map(|process| process.kernel_pid)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "VM {vm_id} has no active process {} for guest kernel call",
                    payload.execution_id
                ))
            })?;

        let response = agentos_native_sidecar_core::handle_guest_kernel_call(
            &mut vm.kernel,
            kernel_pid,
            EXECUTION_DRIVER_NAME,
            &payload.operation,
            &payload.payload,
        )
        .map_err(guest_kernel_core_error)?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::GuestKernelResult(GuestKernelResultResponse { payload: response }),
            ),
            events: Vec::new(),
        })
    }

    pub(crate) async fn get_resource_snapshot(
        &mut self,
        request: &RequestFrame,
        _payload: GetResourceSnapshotRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        require_vm_inspection_permission(
            &self.bridge,
            &vm_id,
            "process.inspect",
            "process",
            "process://resources",
        )?;

        let snapshot = self
            .vms
            .get(&vm_id)
            .map(|vm| vm.kernel.resource_snapshot())
            .unwrap_or_default();
        let queue_snapshots = queue_tracker::queue_snapshot()
            .into_iter()
            .map(|queue| QueueSnapshotEntry {
                name: queue.name.as_str().to_owned(),
                category: queue.category.as_str().to_owned(),
                depth: queue.depth as u64,
                high_water: queue.high_water as u64,
                capacity: queue.capacity as u64,
                fill_percent: queue.fill_percent as u64,
            })
            .collect();

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::ResourceSnapshot(ResourceSnapshotResponse {
                    running_processes: snapshot.running_processes as u64,
                    exited_processes: snapshot.exited_processes as u64,
                    fd_tables: snapshot.fd_tables as u64,
                    open_fds: snapshot.open_fds as u64,
                    pipes: snapshot.pipes as u64,
                    pipe_buffered_bytes: snapshot.pipe_buffered_bytes as u64,
                    ptys: snapshot.ptys as u64,
                    pty_buffered_input_bytes: snapshot.pty_buffered_input_bytes as u64,
                    pty_buffered_output_bytes: snapshot.pty_buffered_output_bytes as u64,
                    sockets: snapshot.sockets as u64,
                    socket_listeners: snapshot.socket_listeners as u64,
                    socket_connections: snapshot.socket_connections as u64,
                    socket_buffered_bytes: snapshot.socket_buffered_bytes as u64,
                    socket_datagram_queue_len: snapshot.socket_datagram_queue_len as u64,
                    queue_snapshots,
                }),
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
        require_vm_inspection_permission(
            &self.bridge,
            &vm_id,
            "network.inspect",
            "network",
            &socket_query_resource(SocketQueryKind::UdpBound, &lookup_request),
        )?;
        let socket = find_socket_state_entry(
            self.vms.get(&vm_id),
            SocketQueryKind::UdpBound,
            &lookup_request,
        )?;

        Ok(DispatchResult {
            response: bound_udp_snapshot_response(request, socket),
            events: Vec::new(),
        })
    }

    pub(crate) async fn vm_fetch(
        &mut self,
        request: &RequestFrame,
        payload: VmFetchRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let stream_operation = payload.stream_operation.clone();
        if matches!(stream_operation.as_deref(), Some("read" | "cancel")) {
            let stream_id = payload.stream_id.as_deref().ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "vm.fetch stream read/cancel requires stream_id",
                ))
            })?;
            let vm = self
                .vms
                .get_mut(&vm_id)
                .ok_or_else(|| SidecarError::InvalidState(String::from("unknown sidecar VM")))?;
            let response_json = if stream_operation.as_deref() == Some("read") {
                read_kernel_http_fetch_stream(
                    &self.bridge,
                    &vm_id,
                    vm,
                    stream_id,
                    payload.max_bytes.unwrap_or(64 * 1024) as usize,
                )
                .await?
            } else {
                cancel_kernel_http_fetch_stream(&self.bridge, &vm_id, vm, stream_id).await?
            };
            let response = self.respond(
                request,
                ResponsePayload::VmFetchResult(VmFetchResponse { response_json }),
            );
            ensure_vm_fetch_response_frame_within_limit(&response, self.config.max_frame_bytes)?;
            return Ok(DispatchResult {
                response,
                events: Vec::new(),
            });
        }
        if let Some(operation) = stream_operation.as_deref() {
            if operation != "start" {
                return Err(SidecarError::InvalidState(format!(
                    "unknown vm.fetch stream operation {operation:?}; expected start, read, or cancel"
                )));
            }
        }

        let vm = self
            .vms
            .get_mut(&vm_id)
            .ok_or_else(|| SidecarError::InvalidState(String::from("unknown sidecar VM")))?;
        // HTTP origin-form has exactly one leading slash. Normalizing at the
        // sidecar boundary keeps VM fetch behavior stable even when an
        // upstream router hands us a network-path-style `//foo` URL.
        let target_path = format!("/{}", payload.path.trim_start_matches('/'));
        let request_url = Url::parse(&format!("http://127.0.0.1:{}{target_path}", payload.port))
            .map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid vm.fetch target {target_path:?}: {error}"
                ))
            })?;
        let header_values: BTreeMap<String, Value> = serde_json::from_str(&payload.headers_json)
            .map_err(|error| {
                SidecarError::InvalidState(format!(
                    "vm.fetch headers_json must be valid JSON: {error}"
                ))
            })?;
        if payload.body.is_some() && payload.body_base64.is_some() {
            return Err(SidecarError::InvalidState(String::from(
                "vm.fetch accepts either body or body_base64, not both",
            )));
        }
        let body_bytes = payload
            .body_base64
            .as_deref()
            .map(|body| {
                base64::engine::general_purpose::STANDARD
                    .decode(body)
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "vm.fetch body_base64 must be valid base64: {error}"
                        ))
                    })
            })
            .transpose()?;
        let options = JavascriptHttpRequestOptions {
            method: Some(payload.method),
            headers: header_values,
            body: payload.body,
            reject_unauthorized: None,
        };
        let headers = parse_http_header_collection(&options.headers, "vm.fetch headers")?;
        let target_process_id = find_kernel_http_listener_process(vm, payload.port);
        if let Some(target_process_id) = target_process_id {
            let max_fetch_response_bytes = vm.limits.http.max_fetch_response_bytes;
            let fetch_result = if stream_operation.as_deref() == Some("start") {
                start_kernel_http_fetch_stream(
                    &self.bridge,
                    &vm_id,
                    vm,
                    &target_process_id,
                    payload.port,
                    &target_path,
                    &options,
                    &headers,
                    body_bytes.as_deref(),
                    max_fetch_response_bytes,
                )
                .await
            } else {
                dispatch_kernel_http_fetch(
                    &self.bridge,
                    &vm_id,
                    vm,
                    &target_process_id,
                    payload.port,
                    &target_path,
                    &options,
                    &headers,
                    body_bytes.as_deref(),
                    max_fetch_response_bytes,
                )
                .await
            };
            let response_json = match fetch_result {
                Ok(response_json) => response_json,
                Err(error) => {
                    if let Some(exit_code) = kernel_http_fetch_target_exit_code(&error) {
                        let _ = vm;
                        self.finish_active_process_exit(&vm_id, &target_process_id, exit_code)?;
                    }
                    return Err(error);
                }
            };
            let response = self.respond(
                request,
                ResponsePayload::VmFetchResult(VmFetchResponse { response_json }),
            );
            ensure_vm_fetch_response_frame_within_limit(&response, self.config.max_frame_bytes)?;

            return Ok(DispatchResult {
                response,
                events: Vec::new(),
            });
        }

        let Some((target_process_id, server_id)) =
            vm.active_processes
                .iter()
                .find_map(|(process_id, process)| {
                    process
                        .http_servers
                        .iter()
                        .find(|(_, server)| server.guest_local_addr.port() == payload.port)
                        .map(|(server_id, _)| (process_id.clone(), *server_id))
                })
        else {
            return Err(SidecarError::Execution(format!(
                "vm.fetch could not find a guest HTTP listener on port {}",
                payload.port
            )));
        };
        if stream_operation.as_deref() == Some("start") {
            return Err(SidecarError::InvalidState(String::from(
                "vm.fetch streaming requires a kernel-backed HTTP listener",
            )));
        }
        if body_bytes.is_some() {
            return Err(SidecarError::InvalidState(String::from(
                "binary vm.fetch bodies require a kernel-backed HTTP listener",
            )));
        }
        let socket_paths = build_javascript_socket_path_context(vm)?;
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let capabilities = vm.capabilities.clone();
        let process = vm
            .active_processes
            .get_mut(&target_process_id)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "vm.fetch target process disappeared: {target_process_id}"
                ))
            })?;
        let request_json = serialize_http_loopback_request(&request_url, &options, &headers)?;
        let response_json = dispatch_loopback_http_request(LoopbackHttpDispatchRequest {
            bridge: &self.bridge,
            vm_id: &vm_id,
            dns: &vm.dns,
            socket_paths: &socket_paths,
            kernel: &mut vm.kernel,
            kernel_readiness,
            process,
            server_id,
            request_json: &request_json,
            capabilities,
        })
        .await?;

        let response = self.respond(
            request,
            ResponsePayload::VmFetchResult(VmFetchResponse { response_json }),
        );
        ensure_vm_fetch_response_frame_within_limit(&response, self.config.max_frame_bytes)?;

        Ok(DispatchResult {
            response,
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

        self.drain_root_signal_state_events(&vm_id, &payload.process_id)?;

        let handlers = self
            .vms
            .get(&vm_id)
            .and_then(|vm| vm.signal_states.get(&payload.process_id))
            .cloned()
            .unwrap_or_default();

        Ok(DispatchResult {
            response: signal_state_response(request, payload.process_id, handlers),
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
            response: zombie_timer_count_response(request, count),
            events: Vec::new(),
        })
    }
}
