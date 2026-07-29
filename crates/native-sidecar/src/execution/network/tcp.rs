use super::super::*;
use crate::state::SocketFairnessRetirement;

pub(in crate::execution) struct NetTcpTraceCounters {
    pub(in crate::execution) socket_read_calls: AtomicU64,
    pub(in crate::execution) socket_read_zero_wait_calls: AtomicU64,
    pub(in crate::execution) socket_read_data_events: AtomicU64,
    pub(in crate::execution) socket_read_bytes: AtomicU64,
    pub(in crate::execution) socket_read_kernel_us: AtomicU64,
    pub(in crate::execution) socket_read_end_events: AtomicU64,
    pub(in crate::execution) socket_read_eagain: AtomicU64,
    pub(in crate::execution) socket_read_errors: AtomicU64,
    pub(in crate::execution) socket_read_push_attempts: AtomicU64,
    pub(in crate::execution) socket_read_push_sent: AtomicU64,
    pub(in crate::execution) socket_read_push_missing: AtomicU64,
    pub(in crate::execution) socket_read_push_errors: AtomicU64,
    pub(in crate::execution) socket_write_calls: AtomicU64,
    pub(in crate::execution) socket_write_bytes: AtomicU64,
    pub(in crate::execution) socket_write_kernel_us: AtomicU64,
    pub(in crate::execution) socket_write_errors: AtomicU64,
    pub(in crate::execution) server_accept_calls: AtomicU64,
    pub(in crate::execution) server_accept_zero_wait_calls: AtomicU64,
    pub(in crate::execution) server_accept_connections: AtomicU64,
    pub(in crate::execution) server_accept_eagain: AtomicU64,
    pub(in crate::execution) server_accept_errors: AtomicU64,
    pub(in crate::execution) kernel_poll_targets: AtomicU64,
    pub(in crate::execution) kernel_poll_zero_wait_calls: AtomicU64,
    pub(in crate::execution) kernel_poll_wait_us: AtomicU64,
    pub(in crate::execution) kernel_poll_elapsed_us: AtomicU64,
    pub(in crate::execution) kernel_poll_empty: AtomicU64,
    pub(in crate::execution) kernel_poll_ready: AtomicU64,
    pub(in crate::execution) kernel_poll_revents_read: AtomicU64,
    pub(in crate::execution) kernel_poll_revents_hup: AtomicU64,
    pub(in crate::execution) kernel_poll_revents_err: AtomicU64,
    pub(in crate::execution) kernel_poll_revents_bits_or: AtomicU64,
}

impl NetTcpTraceCounters {
    const fn new() -> Self {
        Self {
            socket_read_calls: AtomicU64::new(0),
            socket_read_zero_wait_calls: AtomicU64::new(0),
            socket_read_data_events: AtomicU64::new(0),
            socket_read_bytes: AtomicU64::new(0),
            socket_read_kernel_us: AtomicU64::new(0),
            socket_read_end_events: AtomicU64::new(0),
            socket_read_eagain: AtomicU64::new(0),
            socket_read_errors: AtomicU64::new(0),
            socket_read_push_attempts: AtomicU64::new(0),
            socket_read_push_sent: AtomicU64::new(0),
            socket_read_push_missing: AtomicU64::new(0),
            socket_read_push_errors: AtomicU64::new(0),
            socket_write_calls: AtomicU64::new(0),
            socket_write_bytes: AtomicU64::new(0),
            socket_write_kernel_us: AtomicU64::new(0),
            socket_write_errors: AtomicU64::new(0),
            server_accept_calls: AtomicU64::new(0),
            server_accept_zero_wait_calls: AtomicU64::new(0),
            server_accept_connections: AtomicU64::new(0),
            server_accept_eagain: AtomicU64::new(0),
            server_accept_errors: AtomicU64::new(0),
            kernel_poll_targets: AtomicU64::new(0),
            kernel_poll_zero_wait_calls: AtomicU64::new(0),
            kernel_poll_wait_us: AtomicU64::new(0),
            kernel_poll_elapsed_us: AtomicU64::new(0),
            kernel_poll_empty: AtomicU64::new(0),
            kernel_poll_ready: AtomicU64::new(0),
            kernel_poll_revents_read: AtomicU64::new(0),
            kernel_poll_revents_hup: AtomicU64::new(0),
            kernel_poll_revents_err: AtomicU64::new(0),
            kernel_poll_revents_bits_or: AtomicU64::new(0),
        }
    }
}

pub(in crate::execution) static NET_TCP_TRACE_COUNTERS: NetTcpTraceCounters =
    NetTcpTraceCounters::new();

pub(in crate::execution) struct ActiveTcpConnectRequest<'a, B> {
    pub(in crate::execution) bridge: &'a SharedBridge<B>,
    pub(in crate::execution) kernel: &'a mut SidecarKernel,
    pub(in crate::execution) kernel_pid: u32,
    pub(in crate::execution) vm_id: &'a str,
    pub(in crate::execution) dns: &'a VmDnsConfig,
    pub(in crate::execution) host: &'a str,
    pub(in crate::execution) port: u16,
    pub(in crate::execution) family: Option<u8>,
    pub(in crate::execution) local_address: Option<&'a str>,
    pub(in crate::execution) local_port: Option<u16>,
    pub(in crate::execution) local_reservation: Option<(JavascriptSocketFamily, u16)>,
    pub(in crate::execution) context: &'a JavascriptSocketPathContext,
    pub(in crate::execution) resources: Arc<ResourceLedger>,
    pub(in crate::execution) runtime_context: agentos_runtime::RuntimeContext,
    pub(in crate::execution) reactor_limits: ReactorIoLimits,
}

impl ActiveTcpSocket {
    pub(in crate::execution) fn set_fairness_identity(
        &self,
        identity: Option<(u64, u64)>,
    ) -> Result<(), SidecarError> {
        let identity = identity.ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_FAIRNESS_IDENTITY: TCP socket capability was committed outside a VM runtime scope",
            ))
        })?;
        self.fairness_identity.set(identity).map_err(|_| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_FAIRNESS_IDENTITY: TCP socket capability identity was committed more than once",
            ))
        })?;
        self.fairness_identity_committed.notify_waiters();
        Ok(())
    }

    pub(in crate::execution) fn connect<B>(
        request: ActiveTcpConnectRequest<'_, B>,
    ) -> Result<Self, SidecarError>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        let ActiveTcpConnectRequest {
            bridge,
            kernel,
            kernel_pid,
            vm_id,
            dns,
            host,
            port,
            family,
            local_address,
            local_port,
            local_reservation,
            context,
            resources,
            runtime_context,
            reactor_limits,
        } = request;
        let resolved =
            resolve_tcp_connect_addr(bridge, kernel, vm_id, dns, host, port, family, context)?;
        if resolved.use_kernel_loopback {
            return Self::connect_kernel_loopback(
                kernel,
                kernel_pid,
                resolved,
                local_address,
                local_port,
                local_reservation,
                context,
                resources,
                runtime_context,
                reactor_limits,
            );
        }

        let stream =
            TcpStream::connect_timeout(&resolved.actual_addr, reactor_limits.operation_deadline)
                .map_err(sidecar_net_error)?;
        let guest_local_addr = stream.local_addr().map_err(sidecar_net_error)?;
        Self::from_stream(
            stream,
            None,
            guest_local_addr,
            resolved.guest_remote_addr,
            resources,
            runtime_context,
            reactor_limits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::execution) fn connect_kernel_loopback(
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        resolved: ResolvedTcpConnectAddr,
        local_address: Option<&str>,
        local_port: Option<u16>,
        local_reservation: Option<(JavascriptSocketFamily, u16)>,
        context: &JavascriptSocketPathContext,
        resources: Arc<ResourceLedger>,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        debug_assert!(resolved.use_kernel_loopback);
        let family = JavascriptSocketFamily::from_ip(resolved.guest_remote_addr.ip());
        let requested_local_port = local_port.unwrap_or(0);
        let local_port = if requested_local_port != 0
            && local_reservation == Some((family, requested_local_port))
        {
            requested_local_port
        } else {
            allocate_guest_listen_port(
                requested_local_port,
                family,
                &context.used_tcp_guest_ports,
                context.listen_policy,
            )?
        };
        let local_ip = match (family, local_address) {
            (JavascriptSocketFamily::Ipv4, Some("0.0.0.0")) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            (JavascriptSocketFamily::Ipv4, Some("127.0.0.1") | Some("localhost") | None) => {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            }
            (JavascriptSocketFamily::Ipv6, Some("::")) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            (JavascriptSocketFamily::Ipv6, Some("::1") | Some("localhost") | None) => {
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            }
            (JavascriptSocketFamily::Ipv4, Some(other)) => {
                return Err(SidecarError::Execution(format!(
                    "EACCES: TCP sockets must bind to loopback or unspecified addresses, got {other}"
                )));
            }
            (JavascriptSocketFamily::Ipv6, Some(other)) => {
                return Err(SidecarError::Execution(format!(
                    "EACCES: TCP sockets must bind to loopback or unspecified addresses, got {other}"
                )));
            }
        };
        let local_addr = SocketAddr::new(local_ip, local_port);
        let spec = match family {
            JavascriptSocketFamily::Ipv4 => SocketSpec::tcp(),
            JavascriptSocketFamily::Ipv6 => {
                SocketSpec::new(SocketDomain::Inet6, SocketType::Stream)
            }
        };
        let socket_id = kernel
            .socket_create(EXECUTION_DRIVER_NAME, kernel_pid, spec)
            .map_err(kernel_error)?;
        kernel
            .socket_bind_inet(
                EXECUTION_DRIVER_NAME,
                kernel_pid,
                socket_id,
                InetSocketAddress::new(local_ip.to_string(), local_port),
            )
            .map_err(kernel_error)?;
        kernel
            .socket_connect_inet_loopback(
                EXECUTION_DRIVER_NAME,
                kernel_pid,
                socket_id,
                InetSocketAddress::new(
                    resolved.guest_remote_addr.ip().to_string(),
                    resolved.guest_remote_addr.port(),
                ),
            )
            .map_err(kernel_error)?;
        Ok(Self::from_kernel(
            socket_id,
            None,
            local_addr,
            resolved.guest_remote_addr,
            resources,
            runtime_context,
            reactor_limits,
        ))
    }

    pub(in crate::execution) fn from_stream(
        stream: TcpStream,
        listener_id: Option<String>,
        guest_local_addr: SocketAddr,
        guest_remote_addr: SocketAddr,
        resources: Arc<ResourceLedger>,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        let read_stream = stream.try_clone().map_err(sidecar_net_error)?;
        let write_stream = stream.try_clone().map_err(sidecar_net_error)?;
        let fairness_identity = Arc::new(OnceLock::new());
        let fairness_identity_committed = Arc::new(tokio::sync::Notify::new());
        let fairness_retirement =
            SocketFairnessRetirement::new(Arc::clone(&fairness_identity), runtime_context.clone());
        let plain_commands = spawn_tcp_plain_socket_transport(
            &runtime_context,
            write_stream,
            &resources,
            reactor_limits,
            Arc::clone(&fairness_identity),
            Arc::clone(&fairness_identity_committed),
        )?;
        let stream = Arc::new(Mutex::new(stream));
        let pending_read_stream = Arc::new(Mutex::new(Some(read_stream)));
        let (sender, events) = async_completion_channel(
            runtime_context.clone(),
            socket_completion_capacity(reactor_limits),
        );
        let read_event_notify = Arc::new(tokio::sync::Notify::new());
        let application_read_interest = Arc::new(AtomicBool::new(false));
        let application_read_notify = Arc::new(tokio::sync::Notify::new());
        let event_pusher = SocketReadinessSubscribers::new(&resources);
        let tls_mode = Arc::new(AtomicBool::new(false));
        let tls_state = Arc::new(Mutex::new(None));
        let saw_local_shutdown = Arc::new(AtomicBool::new(false));
        let saw_remote_end = Arc::new(AtomicBool::new(false));
        let close_notified = Arc::new(AtomicBool::new(false));

        Ok(Self {
            runtime_context,
            reactor_limits,
            fairness_identity,
            fairness_identity_committed,
            fairness_retirement,
            description_lease: Arc::new(SocketDescriptionLease::default()),
            stream: Some(stream),
            pending_read_stream: Some(pending_read_stream),
            plain_reader_running: Arc::new(AtomicBool::new(false)),
            plain_reader_stopped: Arc::new(tokio::sync::Notify::new()),
            events: Some(Arc::new(Mutex::new(events))),
            event_sender: Some(sender),
            read_event_notify,
            event_pusher: Arc::clone(&event_pusher),
            readiness_registration: SocketReadinessRegistration::new(
                event_pusher,
                Some(Arc::clone(&application_read_interest)),
                Some(Arc::clone(&application_read_notify)),
            ),
            application_read_interest,
            application_read_notify,
            kernel_socket_id: None,
            no_delay: false,
            keep_alive: false,
            keep_alive_initial_delay_secs: None,
            guest_local_addr,
            guest_remote_addr,
            listener_id,
            tls_mode,
            native_tls_commands: Arc::new(Mutex::new(None)),
            plain_commands: Some(plain_commands),
            tls_state,
            saw_local_shutdown,
            saw_remote_end,
            close_notified,
            pending_read_event: Arc::new(Mutex::new(None)),
            read_buffer: Arc::new(Mutex::new(VecDeque::new())),
            description_handles: Arc::new(()),
            listener_connection_retirement: None,
            kernel_transfer_guard: None,
            resources,
        })
    }

    pub(in crate::execution) fn from_kernel(
        socket_id: SocketId,
        listener_id: Option<String>,
        guest_local_addr: SocketAddr,
        guest_remote_addr: SocketAddr,
        resources: Arc<ResourceLedger>,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Self {
        let (sender, events) = async_completion_channel(
            runtime_context.clone(),
            socket_completion_capacity(reactor_limits),
        );
        let fairness_identity = Arc::new(OnceLock::new());
        let fairness_retirement =
            SocketFairnessRetirement::new(Arc::clone(&fairness_identity), runtime_context.clone());
        let application_read_interest = Arc::new(AtomicBool::new(false));
        let application_read_notify = Arc::new(tokio::sync::Notify::new());
        let event_pusher = SocketReadinessSubscribers::new(&resources);
        Self {
            runtime_context,
            reactor_limits,
            fairness_identity,
            fairness_identity_committed: Arc::new(tokio::sync::Notify::new()),
            fairness_retirement,
            description_lease: Arc::new(SocketDescriptionLease::default()),
            stream: None,
            pending_read_stream: None,
            plain_reader_running: Arc::new(AtomicBool::new(false)),
            plain_reader_stopped: Arc::new(tokio::sync::Notify::new()),
            events: Some(Arc::new(Mutex::new(events))),
            event_sender: Some(sender),
            read_event_notify: Arc::new(tokio::sync::Notify::new()),
            event_pusher: Arc::clone(&event_pusher),
            readiness_registration: SocketReadinessRegistration::new(
                event_pusher,
                Some(Arc::clone(&application_read_interest)),
                Some(Arc::clone(&application_read_notify)),
            ),
            application_read_interest,
            application_read_notify,
            kernel_socket_id: Some(socket_id),
            no_delay: false,
            keep_alive: false,
            keep_alive_initial_delay_secs: None,
            guest_local_addr,
            guest_remote_addr,
            listener_id,
            tls_mode: Arc::new(AtomicBool::new(false)),
            native_tls_commands: Arc::new(Mutex::new(None)),
            plain_commands: None,
            tls_state: Arc::new(Mutex::new(None)),
            saw_local_shutdown: Arc::new(AtomicBool::new(false)),
            saw_remote_end: Arc::new(AtomicBool::new(false)),
            close_notified: Arc::new(AtomicBool::new(false)),
            pending_read_event: Arc::new(Mutex::new(None)),
            read_buffer: Arc::new(Mutex::new(VecDeque::new())),
            description_handles: Arc::new(()),
            listener_connection_retirement: None,
            kernel_transfer_guard: None,
            resources,
        }
    }

    pub(in crate::execution) fn clone_for_fd_transfer(&self) -> Self {
        Self {
            runtime_context: self.runtime_context.clone(),
            reactor_limits: self.reactor_limits,
            fairness_identity: Arc::clone(&self.fairness_identity),
            fairness_identity_committed: Arc::clone(&self.fairness_identity_committed),
            fairness_retirement: Arc::clone(&self.fairness_retirement),
            description_lease: Arc::clone(&self.description_lease),
            stream: self.stream.as_ref().map(Arc::clone),
            pending_read_stream: self.pending_read_stream.as_ref().map(Arc::clone),
            plain_reader_running: Arc::clone(&self.plain_reader_running),
            plain_reader_stopped: Arc::clone(&self.plain_reader_stopped),
            events: self.events.as_ref().map(Arc::clone),
            event_sender: self.event_sender.clone(),
            read_event_notify: Arc::clone(&self.read_event_notify),
            event_pusher: Arc::clone(&self.event_pusher),
            readiness_registration: SocketReadinessRegistration::new(
                Arc::clone(&self.event_pusher),
                Some(Arc::clone(&self.application_read_interest)),
                Some(Arc::clone(&self.application_read_notify)),
            ),
            application_read_interest: Arc::clone(&self.application_read_interest),
            application_read_notify: Arc::clone(&self.application_read_notify),
            kernel_socket_id: self.kernel_socket_id,
            no_delay: self.no_delay,
            keep_alive: self.keep_alive,
            keep_alive_initial_delay_secs: self.keep_alive_initial_delay_secs,
            guest_local_addr: self.guest_local_addr,
            guest_remote_addr: self.guest_remote_addr,
            listener_id: self.listener_id.clone(),
            tls_mode: Arc::clone(&self.tls_mode),
            native_tls_commands: Arc::clone(&self.native_tls_commands),
            plain_commands: self.plain_commands.clone(),
            tls_state: Arc::clone(&self.tls_state),
            saw_local_shutdown: Arc::clone(&self.saw_local_shutdown),
            saw_remote_end: Arc::clone(&self.saw_remote_end),
            close_notified: Arc::clone(&self.close_notified),
            pending_read_event: Arc::clone(&self.pending_read_event),
            read_buffer: Arc::clone(&self.read_buffer),
            description_handles: Arc::clone(&self.description_handles),
            listener_connection_retirement: self.listener_connection_retirement.clone(),
            kernel_transfer_guard: self.kernel_transfer_guard.clone(),
            resources: Arc::clone(&self.resources),
        }
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

    pub(in crate::execution) fn set_event_pusher(
        &self,
        session: Option<V8SessionHandle>,
        identity: Option<(
            agentos_runtime::capability::CapabilityId,
            agentos_runtime::capability::CapabilityGeneration,
        )>,
    ) {
        let (Some(session), Some((capability_id, capability_generation))) = (session, identity)
        else {
            return;
        };
        self.readiness_registration.register(
            Some(session),
            Some((capability_id, capability_generation)),
            agentos_runtime::readiness::ReadyFlags::READABLE,
        );
    }

    pub(in crate::execution) fn set_application_read_interest(
        &self,
        enabled: bool,
    ) -> Result<(), SidecarError> {
        let aggregate = self
            .readiness_registration
            .set_application_read_interest(enabled)?;
        if aggregate {
            self.ensure_tcp_reader()?;
        }
        Ok(())
    }

    pub(in crate::execution) fn poll(
        &mut self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        _wait: Duration,
        trace_enabled: bool,
    ) -> Result<Option<JavascriptTcpSocketEvent>, SidecarError> {
        if let Some(event) = self
            .pending_read_event
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("TCP pending read event lock poisoned"))
            })?
            .take()
        {
            return Ok(Some(event));
        }
        if self.tls_mode.load(Ordering::SeqCst) {
            self.ensure_tcp_reader()?;
            return match self
                .events
                .as_ref()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from("TCP socket event channel missing"))
                })?
                .lock()
                .map_err(|_| {
                    SidecarError::InvalidState(String::from(
                        "TCP socket event channel lock poisoned",
                    ))
                })?
                .try_recv()
            {
                Ok(event) => Ok(Some(event)),
                Err(TokioTryRecvError::Empty | TokioTryRecvError::Disconnected) => Ok(None),
            };
        }

        if let Some(socket_id) = self.kernel_socket_id {
            let poll_started = Instant::now();
            let result = kernel
                .poll_targets(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    vec![PollTargetEntry::socket(
                        socket_id,
                        POLLIN | POLLHUP | POLLERR,
                    )],
                    0,
                )
                .map_err(kernel_error)?;
            let poll_elapsed = poll_started.elapsed();
            let revents = result
                .targets
                .first()
                .map(|entry| entry.revents)
                .unwrap_or_else(PollEvents::empty);
            record_net_tcp_kernel_poll(trace_enabled, Duration::ZERO, poll_elapsed, revents);
            if revents.is_empty() {
                return Ok(None);
            }
            if revents.intersects(POLLIN) {
                const READ_QUANTUM: usize = 64 * 1024;
                let mut reservation = self
                    .resources
                    .reserve(ResourceClass::BufferedBytes, READ_QUANTUM)
                    .map_err(SidecarError::from)?;
                let read_started = Instant::now();
                let read_result =
                    kernel.socket_read(EXECUTION_DRIVER_NAME, kernel_pid, socket_id, READ_QUANTUM);
                if trace_enabled {
                    NET_TCP_TRACE_COUNTERS.socket_read_kernel_us.fetch_add(
                        duration_micros_u64(read_started.elapsed()),
                        Ordering::Relaxed,
                    );
                }
                return match read_result {
                    Ok(Some(bytes)) if !bytes.is_empty() => {
                        if trace_enabled {
                            NET_TCP_TRACE_COUNTERS
                                .socket_read_data_events
                                .fetch_add(1, Ordering::Relaxed);
                            NET_TCP_TRACE_COUNTERS.socket_read_bytes.fetch_add(
                                u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                                Ordering::Relaxed,
                            );
                        }
                        let unused = READ_QUANTUM.saturating_sub(bytes.len());
                        drop(reservation.split(unused));
                        Ok(Some(JavascriptTcpSocketEvent::Data {
                            bytes,
                            reservation: SharedReservation::new(reservation),
                            source_reservations: Vec::new(),
                        }))
                    }
                    Ok(Some(_)) => {
                        if trace_enabled {
                            NET_TCP_TRACE_COUNTERS
                                .socket_read_data_events
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        drop(reservation.split(READ_QUANTUM));
                        Ok(Some(JavascriptTcpSocketEvent::Data {
                            bytes: Vec::new(),
                            reservation: SharedReservation::new(reservation),
                            source_reservations: Vec::new(),
                        }))
                    }
                    Ok(None) => {
                        if trace_enabled {
                            NET_TCP_TRACE_COUNTERS
                                .socket_read_end_events
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        self.saw_remote_end.store(true, Ordering::SeqCst);
                        Ok(Some(JavascriptTcpSocketEvent::End))
                    }
                    Err(error) if error.code() == "EAGAIN" => {
                        if trace_enabled {
                            NET_TCP_TRACE_COUNTERS
                                .socket_read_eagain
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(None)
                    }
                    Err(error) => {
                        if trace_enabled {
                            NET_TCP_TRACE_COUNTERS
                                .socket_read_errors
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(Some(JavascriptTcpSocketEvent::Error {
                            code: Some(error.code().to_string()),
                            message: error.to_string(),
                        }))
                    }
                };
            }
            if revents.intersects(POLLHUP) {
                self.saw_remote_end.store(true, Ordering::SeqCst);
                return Ok(Some(JavascriptTcpSocketEvent::End));
            }
            if revents.intersects(POLLERR) {
                return Ok(Some(JavascriptTcpSocketEvent::Error {
                    code: Some(String::from("EPIPE")),
                    message: String::from("kernel TCP socket reported POLLERR"),
                }));
            }
            return Ok(None);
        }

        self.ensure_tcp_reader()?;
        match self
            .events
            .as_ref()
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from("TCP socket event channel missing"))
            })?
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("TCP socket event channel lock poisoned"))
            })?
            .try_recv()
        {
            Ok(event) => Ok(Some(event)),
            Err(TokioTryRecvError::Empty | TokioTryRecvError::Disconnected) => Ok(None),
        }
    }

    fn ensure_tcp_reader(&self) -> Result<(), SidecarError> {
        if self.kernel_socket_id.is_some() {
            return Ok(());
        }
        if self.tls_mode.load(Ordering::SeqCst) {
            return Ok(());
        }
        let read_stream = self
            .pending_read_stream
            .as_ref()
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from("TCP socket reader handle missing"))
            })?
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("TCP socket reader lock poisoned"))
            })?
            .take();
        if let Some(read_stream) = read_stream {
            self.plain_reader_running.store(true, Ordering::Release);
            let spawn_result = spawn_tcp_socket_reader(
                self.runtime_context.clone(),
                read_stream,
                self.event_sender
                    .as_ref()
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from("TCP socket event sender missing"))
                    })?
                    .clone(),
                Arc::clone(&self.read_event_notify),
                Arc::clone(&self.event_pusher),
                Arc::clone(&self.application_read_interest),
                Arc::clone(&self.application_read_notify),
                Arc::clone(&self.tls_mode),
                Arc::clone(&self.saw_local_shutdown),
                Arc::clone(&self.saw_remote_end),
                Arc::clone(&self.close_notified),
                Arc::clone(&self.plain_reader_running),
                Arc::clone(&self.plain_reader_stopped),
                Arc::clone(&self.resources),
                self.reactor_limits,
                Arc::clone(&self.fairness_identity),
                Arc::clone(&self.fairness_identity_committed),
            );
            if let Err(error) = spawn_result {
                self.plain_reader_running.store(false, Ordering::Release);
                self.plain_reader_stopped.notify_waiters();
                return Err(error);
            }
        }
        Ok(())
    }

    pub(in crate::execution) fn socket_info(&self) -> Value {
        tcp_socket_info_value(&self.guest_local_addr, &self.guest_remote_addr)
    }

    pub(in crate::execution) fn set_no_delay(&mut self, enable: bool) -> Result<(), SidecarError> {
        self.no_delay = enable;
        if self.kernel_socket_id.is_some() {
            return Ok(());
        }
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| SidecarError::InvalidState(String::from("TCP socket stream missing")))?
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        stream.set_nodelay(enable).map_err(sidecar_net_error)
    }

    pub(in crate::execution) fn set_keep_alive(
        &mut self,
        enable: bool,
        initial_delay_secs: Option<u64>,
    ) -> Result<(), SidecarError> {
        self.keep_alive = enable;
        self.keep_alive_initial_delay_secs = initial_delay_secs;
        if self.kernel_socket_id.is_some() {
            return Ok(());
        }
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| SidecarError::InvalidState(String::from("TCP socket stream missing")))?
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        let socket = SockRef::from(&*stream);
        socket.set_keepalive(enable).map_err(sidecar_net_error)?;
        if enable {
            if let Some(delay_secs) = initial_delay_secs.filter(|delay_secs| *delay_secs > 0) {
                socket
                    .set_tcp_keepalive(
                        &TcpKeepalive::new().with_time(Duration::from_secs(delay_secs)),
                    )
                    .map_err(sidecar_net_error)?;
            }
        }
        Ok(())
    }

    pub(in crate::execution) fn upgrade_tls(
        &self,
        vm_id: &str,
        kernel: &mut SidecarKernel,
        options: JavascriptTlsBridgeOptions,
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>,
        SidecarError,
    > {
        if self.tls_mode.load(Ordering::SeqCst) {
            return Err(SidecarError::Execution(String::from(
                "EALREADY: socket is already upgraded to TLS",
            )));
        }
        let fairness_identity = self.fairness_identity.get().copied().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_FAIRNESS_IDENTITY: TLS transport has no committed TCP capability identity",
            ))
        })?;

        let client_hello = if options.is_server {
            self.peek_tls_client_hello(vm_id, kernel)?
        } else {
            None
        };

        let tls_state = ActiveTlsState {
            client_hello,
            local_certificates: tls_local_certificates(&options)?,
            peer_certificates: Vec::new(),
            protocol: None,
            cipher: None,
            session_reused: false,
        };
        {
            let mut state = self
                .tls_state
                .lock()
                .map_err(|_| SidecarError::InvalidState(String::from("TLS state lock poisoned")))?;
            *state = Some(tls_state);
        }

        let default_ca_bundle = vm_default_ca_bundle_for_tls_options(kernel, &options)?;
        if self.kernel_socket_id.is_none() {
            let role = native_tls_role(&options, &default_ca_bundle)?;
            self.tls_mode.store(true, Ordering::SeqCst);
            self.application_read_notify.notify_waiters();
            self.pending_read_stream
                .as_ref()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from("TCP socket reader handle missing"))
                })?
                .lock()
                .map_err(|_| {
                    SidecarError::InvalidState(String::from("TCP socket reader lock poisoned"))
                })?
                .take();
            let stream = self
                .stream
                .as_ref()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from("TCP socket stream missing"))
                })?
                .lock()
                .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?
                .try_clone()
                .map_err(sidecar_net_error)?;
            let (commands, handshake) = spawn_native_tls_transport(
                self.runtime_context.clone(),
                stream,
                role,
                Arc::clone(&self.tls_state),
                self.event_sender
                    .as_ref()
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from("TCP socket event sender missing"))
                    })?
                    .clone(),
                Arc::clone(&self.event_pusher),
                Arc::clone(&self.application_read_interest),
                Arc::clone(&self.application_read_notify),
                Arc::clone(&self.plain_reader_running),
                Arc::clone(&self.plain_reader_stopped),
                Arc::clone(&self.saw_local_shutdown),
                Arc::clone(&self.saw_remote_end),
                Arc::clone(&self.close_notified),
                Arc::clone(&self.resources),
                self.reactor_limits,
                fairness_identity,
            )?;
            *self.native_tls_commands.lock().map_err(|_| {
                SidecarError::InvalidState(String::from("native TLS command lock poisoned"))
            })? = Some(commands);
            return Ok(handshake);
        }

        let socket_id = self.kernel_socket_id.ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "native TLS upgrade did not select its transport task",
            ))
        })?;
        // Kernel loopback connect/accept installs both peer identities before
        // returning the socket.  A missing peer is therefore a lifecycle error,
        // not a condition that can become true after polling this immutable
        // request snapshot.
        let peer_socket_id = kernel
            .socket_get(socket_id)
            .and_then(|record| record.peer_socket_id())
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "ERR_AGENTOS_LOOPBACK_PEER_MISSING: kernel-backed loopback socket {socket_id} has no connected peer for TLS upgrade"
                ))
            })?;
        let endpoint = loopback_tls_endpoint(
            vm_id,
            socket_id,
            peer_socket_id,
            Arc::clone(&self.resources),
        )?;
        let role = native_tls_role(&options, &default_ca_bundle)?;
        self.tls_mode.store(true, Ordering::SeqCst);
        self.application_read_notify.notify_waiters();
        let (commands, handshake) = spawn_loopback_tls_transport(
            self.runtime_context.clone(),
            endpoint,
            role,
            Arc::clone(&self.tls_state),
            self.event_sender
                .as_ref()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from("TCP socket event sender missing"))
                })?
                .clone(),
            Arc::clone(&self.event_pusher),
            Arc::clone(&self.application_read_interest),
            Arc::clone(&self.application_read_notify),
            Arc::clone(&self.saw_local_shutdown),
            Arc::clone(&self.saw_remote_end),
            Arc::clone(&self.close_notified),
            Arc::clone(&self.resources),
            self.reactor_limits,
            fairness_identity,
        )?;
        self.runtime_context
            .spawn(agentos_runtime::TaskClass::Tls, async move {
                match handshake.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => eprintln!(
                        "ERR_AGENTOS_TLS_HANDSHAKE: loopback TLS handshake failed after upgrade admission: {}: {}",
                        error.code, error.message
                    ),
                    Err(_) => eprintln!(
                        "ERR_AGENTOS_TLS_HANDSHAKE: loopback TLS handshake completion channel closed"
                    ),
                }
            })
            .map_err(SidecarError::from)?;
        *self.native_tls_commands.lock().map_err(|_| {
            SidecarError::InvalidState(String::from("native TLS command lock poisoned"))
        })? = Some(commands);
        let (completion, response) = tokio::sync::oneshot::channel();
        send_oneshot_or_log(
            completion,
            Ok(Value::Null),
            "loopback TLS upgrade admission",
        );
        Ok(response)
    }

    fn peek_tls_client_hello(
        &self,
        vm_id: &str,
        kernel: &SidecarKernel,
    ) -> Result<Option<JavascriptTlsClientHello>, SidecarError> {
        if let Some(socket_id) = self.kernel_socket_id {
            let Some(peer_socket_id) = kernel
                .socket_get(socket_id)
                .and_then(|record| record.peer_socket_id())
            else {
                return Ok(None);
            };
            return peek_loopback_tls_client_hello(vm_id, socket_id, peer_socket_id);
        }

        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| SidecarError::InvalidState(String::from("TCP socket stream missing")))?
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
        parse_tls_client_hello_from_bytes(&buffer[..bytes])
    }

    pub(in crate::execution) fn tls_client_hello_json(
        &self,
        vm_id: &str,
        kernel: &SidecarKernel,
    ) -> Result<Value, SidecarError> {
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
            serde_json::to_value(
                self.peek_tls_client_hello(vm_id, kernel)?
                    .unwrap_or_default(),
            )
            .map_err(|error| {
                SidecarError::InvalidState(format!("failed to serialize TLS client hello: {error}"))
            })?,
            "net.socket_get_tls_client_hello",
        )
    }

    pub(in crate::execution) fn tls_query(
        &self,
        query: &str,
        detailed: bool,
    ) -> Result<Value, SidecarError> {
        let state = self
            .tls_state
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TLS state lock poisoned")))?
            .clone();
        let has_transport = self
            .native_tls_commands
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("native TLS command lock poisoned"))
            })?
            .is_some();
        if self.tls_mode.load(Ordering::SeqCst) && !has_transport {
            return Err(SidecarError::InvalidState(String::from(
                "TLS transport task is missing for upgraded socket",
            )));
        }
        let payload = match query {
            "getSession" => tls_bridge_undefined_value(),
            "isSessionReused" => Value::Bool(
                state
                    .as_ref()
                    .is_some_and(|tls_state| tls_state.session_reused),
            ),
            "getPeerCertificate" => state
                .as_ref()
                .and_then(|tls_state| tls_state.peer_certificates.first())
                .map(|certificate| tls_certificate_bridge_value(certificate, detailed))
                .unwrap_or_else(tls_bridge_undefined_value),
            "getCertificate" => state
                .as_ref()
                .and_then(|tls_state| tls_state.local_certificates.first())
                .map(|certificate| tls_certificate_bridge_value(certificate, detailed))
                .unwrap_or_else(tls_bridge_undefined_value),
            "getProtocol" => state
                .as_ref()
                .and_then(|tls_state| tls_state.protocol.clone())
                .map(Value::String)
                .unwrap_or(Value::Null),
            "getCipher" => state
                .as_ref()
                .and_then(|tls_state| tls_state.cipher.clone())
                .unwrap_or_else(tls_bridge_undefined_value),
            other => {
                return Err(SidecarError::InvalidState(format!(
                    "unsupported TLS query {other}"
                )));
            }
        };
        javascript_net_json_string(payload, "net.socket_tls_query")
    }

    pub(in crate::execution) fn begin_tls_write(
        &self,
        contents: &[u8],
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>,
        SidecarError,
    > {
        let commands = self
            .native_tls_commands
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("native TLS command lock poisoned"))
            })?
            .clone()
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "TLS transport task is missing for upgraded socket",
                ))
            })?;
        let loopback_handshake_pending = self.kernel_socket_id.is_some()
            && self
                .tls_state
                .lock()
                .map_err(|_| SidecarError::InvalidState(String::from("TLS state lock poisoned")))?
                .as_ref()
                .is_some_and(|state| state.protocol.is_none());
        let payload = reserve_tls_write_payload(&self.resources, contents)?;
        let (completion, response) = tokio::sync::oneshot::channel();
        let (transport_completion, admission_completion) = if loopback_handshake_pending {
            (None, Some(completion))
        } else {
            (Some(completion), None)
        };
        commands
            .try_send(NativeTlsCommand::Write {
                payload,
                completion: transport_completion,
            })
            .map_err(|error| {
                tls_command_admission_error(error, self.reactor_limits.max_handle_commands)
            })?;
        if let Some(completion) = admission_completion {
            send_oneshot_or_log(
                completion,
                Ok(Value::from(contents.len())),
                "loopback TLS pre-handshake write admission",
            );
        }
        Ok(response)
    }

    pub(in crate::execution) fn begin_tls_shutdown(
        &self,
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>,
        SidecarError,
    > {
        let commands = self
            .native_tls_commands
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("native TLS command lock poisoned"))
            })?
            .clone()
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "TLS transport task is missing for upgraded socket",
                ))
            })?;
        let command_reservation = reserve_tls_command(&self.resources)?;
        let (completion, response) = tokio::sync::oneshot::channel();
        match commands.try_send(NativeTlsCommand::Shutdown {
            _command_reservation: command_reservation,
            completion,
        }) {
            Ok(()) => Ok(response),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
                if self.saw_remote_end.load(Ordering::SeqCst) =>
            {
                self.saw_local_shutdown.store(true, Ordering::SeqCst);
                let (completion, response) = tokio::sync::oneshot::channel();
                send_oneshot_or_log(
                    completion,
                    Ok(Value::Null),
                    "TLS shutdown after remote close",
                );
                Ok(response)
            }
            Err(error) => Err(tls_command_admission_error(
                error,
                self.reactor_limits.max_handle_commands,
            )),
        }
    }

    pub(in crate::execution) fn begin_plain_write(
        &self,
        contents: &[u8],
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>,
        SidecarError,
    > {
        let commands = self.plain_commands.as_ref().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "plain TCP transport task is unavailable for this socket",
            ))
        })?;
        let payload = reserve_plain_socket_write_payload(&self.resources, contents)?;
        let (completion, response) = tokio::sync::oneshot::channel();
        commands
            .try_send(NativePlainSocketCommand::Write {
                payload,
                completion,
            })
            .map_err(plain_socket_command_admission_error)?;
        Ok(response)
    }

    pub(in crate::execution) fn begin_plain_shutdown(
        &self,
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>,
        SidecarError,
    > {
        let commands = self.plain_commands.as_ref().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "plain TCP transport task is unavailable for this socket",
            ))
        })?;
        let reservation = reserve_plain_socket_command(&self.resources)?;
        let (completion, response) = tokio::sync::oneshot::channel();
        self.saw_local_shutdown.store(true, Ordering::SeqCst);
        commands
            .try_send(NativePlainSocketCommand::Shutdown {
                _command_reservation: reservation,
                completion,
            })
            .map_err(plain_socket_command_admission_error)?;
        if self.saw_remote_end.load(Ordering::SeqCst)
            && !self.close_notified.swap(true, Ordering::SeqCst)
            && self.event_sender.as_ref().is_some_and(|sender| {
                sender
                    .try_send(JavascriptTcpSocketEvent::Close { had_error: false })
                    .is_ok()
            })
        {
            push_socket_event(&self.event_pusher, "close");
        }
        Ok(response)
    }

    pub(in crate::execution) fn write_all(
        &self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        contents: &[u8],
    ) -> Result<usize, SidecarError> {
        if self.tls_mode.load(Ordering::SeqCst) {
            return Err(SidecarError::InvalidState(String::from(
                "TLS writes must use the deferred transport completion path",
            )));
        }
        if let Some(socket_id) = self.kernel_socket_id {
            return kernel
                .socket_write(EXECUTION_DRIVER_NAME, kernel_pid, socket_id, contents)
                .map_err(kernel_error);
        }

        let mut stream = self
            .stream
            .as_ref()
            .ok_or_else(|| SidecarError::InvalidState(String::from("TCP socket stream missing")))?
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        write_all_nonblocking(&mut *stream, contents, self.reactor_limits)?;
        Ok(contents.len())
    }

    pub(in crate::execution) fn shutdown_write(
        &self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
    ) -> Result<(), SidecarError> {
        if self.tls_mode.load(Ordering::SeqCst) {
            return Err(SidecarError::InvalidState(String::from(
                "TLS shutdown must use the deferred transport completion path",
            )));
        }
        if let Some(socket_id) = self.kernel_socket_id {
            self.saw_local_shutdown.store(true, Ordering::SeqCst);
            match kernel.socket_shutdown(
                EXECUTION_DRIVER_NAME,
                kernel_pid,
                socket_id,
                KernelSocketShutdown::Write,
            ) {
                Ok(()) => {}
                Err(error) if error.code() == "ENOENT" => {}
                Err(error) => return Err(kernel_error(error)),
            }
            return Ok(());
        }
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| SidecarError::InvalidState(String::from("TCP socket stream missing")))?
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        self.saw_local_shutdown.store(true, Ordering::SeqCst);
        match stream.shutdown(Shutdown::Write) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {}
            Err(error) => return Err(sidecar_net_error(error)),
        }
        if self.saw_remote_end.load(Ordering::SeqCst)
            && !self.close_notified.swap(true, Ordering::SeqCst)
        {
            if let Err(error) = self
                .event_sender
                .as_ref()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from("TCP socket event sender missing"))
                })?
                .try_send(JavascriptTcpSocketEvent::Close { had_error: false })
            {
                eprintln!(
                    "ERR_AGENTOS_SOCKET_EVENT_DROPPED: TCP close event was not admitted: {error}"
                );
            }
        }
        Ok(())
    }

    pub(in crate::execution) fn close(
        &self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
    ) -> Result<(), SidecarError> {
        if self.tls_mode.load(Ordering::SeqCst) {
            let native_commands = self
                .native_tls_commands
                .lock()
                .map_err(|_| {
                    SidecarError::InvalidState(String::from("native TLS command lock poisoned"))
                })?
                .take();
            if let Some(commands) = native_commands {
                let command_reservation = reserve_tls_command(&self.resources)?;
                match commands.try_send(NativeTlsCommand::Close {
                    _command_reservation: command_reservation,
                }) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        return Err(SidecarError::Execution(format!(
                            "ERR_AGENTOS_TLS_COMMAND_LIMIT: TLS command queue exceeded {}; raise limits.reactor.maxHandleCommands",
                            self.reactor_limits.max_handle_commands,
                        )));
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
                }
            }
        }
        if let Some(socket_id) = self.kernel_socket_id {
            return close_kernel_socket_idempotent(kernel, kernel_pid, socket_id);
        }
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| SidecarError::InvalidState(String::from("TCP socket stream missing")))?
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        match stream.shutdown(Shutdown::Both) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotConnected => Ok(()),
            Err(error) => Err(sidecar_net_error(error)),
        }
    }

    pub(in crate::execution) fn poll_limited(
        &mut self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        wait: Duration,
        trace_enabled: bool,
        max_bytes: usize,
    ) -> Result<Option<JavascriptTcpSocketEvent>, SidecarError> {
        let pending = self
            .pending_read_event
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("TCP pending read event lock poisoned"))
            })?
            .take();
        let event = match pending {
            Some(event) => Some(event),
            None => self.poll(kernel, kernel_pid, wait, trace_enabled)?,
        };
        let (event, remainder) = limit_tcp_socket_event(event, max_bytes);
        if let Some(remainder) = remainder {
            *self.pending_read_event.lock().map_err(|_| {
                SidecarError::InvalidState(String::from("TCP pending read event lock poisoned"))
            })? = Some(remainder);
        }
        Ok(event)
    }
}

pub(in crate::execution) fn limit_tcp_socket_event(
    event: Option<JavascriptTcpSocketEvent>,
    max_bytes: usize,
) -> (
    Option<JavascriptTcpSocketEvent>,
    Option<JavascriptTcpSocketEvent>,
) {
    let Some(JavascriptTcpSocketEvent::Data {
        mut bytes,
        reservation,
        source_reservations,
    }) = event
    else {
        return (event, None);
    };
    if bytes.len() <= max_bytes {
        return (
            Some(JavascriptTcpSocketEvent::Data {
                bytes,
                reservation,
                source_reservations,
            }),
            None,
        );
    }

    let remainder_bytes = bytes.split_off(max_bytes);
    let remainder = JavascriptTcpSocketEvent::Data {
        bytes: remainder_bytes,
        reservation: reservation.clone(),
        source_reservations: source_reservations.clone(),
    };
    (
        Some(JavascriptTcpSocketEvent::Data {
            bytes,
            reservation,
            source_reservations,
        }),
        Some(remainder),
    )
}

pub(in crate::execution) fn close_kernel_socket_idempotent(
    kernel: &mut SidecarKernel,
    kernel_pid: u32,
    socket_id: SocketId,
) -> Result<(), SidecarError> {
    match kernel.socket_close(EXECUTION_DRIVER_NAME, kernel_pid, socket_id) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == "ENOENT" => Ok(()),
        Err(error) => Err(kernel_error(error)),
    }
}

pub(in crate::execution) fn register_kernel_readiness_target(
    registry: &KernelSocketReadinessRegistry,
    kernel_socket_id: Option<SocketId>,
    session: Option<V8SessionHandle>,
    notify: Option<Arc<tokio::sync::Notify>>,
    capability: Option<(
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::CapabilityGeneration,
    )>,
    target_id: String,
    event: KernelSocketReadinessEvent,
) {
    let Some(kernel_socket_id) = kernel_socket_id else {
        return;
    };
    if session.is_none() && notify.is_none() {
        return;
    }
    let Some((capability_id, capability_generation)) = capability else {
        eprintln!(
            "ERR_AGENTOS_KERNEL_READINESS_CAPABILITY_MISSING: socket={kernel_socket_id} target={target_id}"
        );
        return;
    };
    let target = KernelSocketReadinessTarget {
        session,
        notify,
        capability_id,
        capability_generation,
        target_id,
        event,
    };
    if let Err(error) = registry.register(kernel_socket_id, target.clone()) {
        eprintln!("{error}");
        return;
    }
    if let Some(notify) = &target.notify {
        notify.notify_one();
    }
    if let Some(session) = &target.session {
        let flags = match target.event {
            KernelSocketReadinessEvent::Data => agentos_runtime::readiness::ReadyFlags::READABLE,
            KernelSocketReadinessEvent::Datagram => {
                agentos_runtime::readiness::ReadyFlags::DATAGRAM
            }
            KernelSocketReadinessEvent::Accept => agentos_runtime::readiness::ReadyFlags::ACCEPT,
        };
        if let Err(error) =
            session.publish_readiness(target.capability_id, target.capability_generation, flags)
        {
            eprintln!(
                "ERR_AGENTOS_KERNEL_READINESS_WAKE: failed registration replay capability={} generation={} target={}: {error}",
                target.capability_id, target.capability_generation, target.target_id
            );
        }
    }
}

pub(in crate::execution) fn unregister_kernel_readiness_target(
    registry: &KernelSocketReadinessRegistry,
    kernel_socket_id: Option<SocketId>,
    capability: Option<(
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::CapabilityGeneration,
    )>,
) {
    let (Some(kernel_socket_id), Some(capability)) = (kernel_socket_id, capability) else {
        return;
    };
    registry.unregister(kernel_socket_id, capability);
}

pub(in crate::execution) fn release_tcp_socket_handle(
    process: &mut ActiveProcess,
    socket_id: &str,
    socket: ActiveTcpSocket,
    kernel: &mut SidecarKernel,
    kernel_readiness: &KernelSocketReadinessRegistry,
) {
    let identity = process
        .capability_readiness_identity(&NativeCapabilityKey::TcpSocket(socket_id.to_owned()));
    unregister_kernel_readiness_target(kernel_readiness, socket.kernel_socket_id, identity);
    if socket.is_final_description_handle() {
        if let Err(error) = socket.close(kernel, process.kernel_pid) {
            eprintln!("ERR_AGENTOS_TCP_SOCKET_CLOSE: {error}");
        }
    }
    if let Err(error) = process.release_description_capability(
        &NativeCapabilityKey::TcpSocket(socket_id.to_owned()),
        socket.fairness_identity.get().copied(),
        &socket.description_lease,
    ) {
        eprintln!("ERR_AGENTOS_CAPABILITY_RELEASE: {error}");
    }
    process.release_capability_if_present(&NativeCapabilityKey::TlsSocket(socket_id.to_owned()));
}

pub(in crate::execution) fn release_tcp_listener_handle(
    process: &mut ActiveProcess,
    listener_id: &str,
    listener: ActiveTcpListener,
    kernel: &mut SidecarKernel,
    kernel_readiness: &KernelSocketReadinessRegistry,
) -> Result<(), SidecarError> {
    let identity = process
        .capability_readiness_identity(&NativeCapabilityKey::TcpListener(listener_id.to_owned()));
    unregister_kernel_readiness_target(kernel_readiness, listener.kernel_socket_id, identity);
    if listener.is_final_description_handle() {
        listener.close(kernel, process.kernel_pid)?;
    }
    process.release_description_capability(
        &NativeCapabilityKey::TcpListener(listener_id.to_owned()),
        None,
        &listener.description_lease,
    )
}

pub(in crate::execution) fn release_unix_socket_handle(
    process: &mut ActiveProcess,
    socket_id: &str,
    mut socket: ActiveUnixSocket,
    unix_bound_addresses: &GuestUnixAddressRegistry,
) {
    if socket.is_final_description_handle() {
        if let Err(error) = socket.cache_remote_peer_metadata(unix_bound_addresses) {
            eprintln!("ERR_AGENTOS_UNIX_SOCKET_METADATA: {error}");
        }
        if let Some(binding_id) = socket.local_registry_binding_id.as_deref() {
            if let Err(error) = release_guest_unix_binding(unix_bound_addresses, binding_id) {
                eprintln!("ERR_AGENTOS_UNIX_SOCKET_METADATA: {error}");
            }
        }
        if let Err(error) = socket.close() {
            eprintln!("ERR_AGENTOS_UNIX_SOCKET_CLOSE: {error}");
        }
    }
    if let Err(error) = process.release_description_capability(
        &NativeCapabilityKey::UnixSocket(socket_id.to_owned()),
        socket.fairness_identity.get().copied(),
        &socket.description_lease,
    ) {
        eprintln!("ERR_AGENTOS_CAPABILITY_RELEASE: {error}");
    }
}

// ActiveTcpListener moved to crate::state

// Unix socket types moved to crate::state

pub(in crate::execution) fn deferred_connect_error(
    error: SidecarError,
) -> crate::state::DeferredRpcError {
    crate::state::DeferredRpcError {
        code: javascript_sync_rpc_error_code(&error),
        message: javascript_sync_rpc_error_message(&error),
    }
}

pub(in crate::execution) fn defer_native_tcp_connect(
    process: &mut ActiveProcess,
    request_id: u64,
    pending_capability: PendingCapability,
    resolved: ResolvedTcpConnectAddr,
    local_reservation_id: Option<String>,
) -> Result<JavascriptSyncRpcServiceResponse, SidecarError> {
    let socket_id = process.allocate_tcp_socket_id();
    let runtime = process.runtime_context.clone();
    let task_runtime = runtime.clone();
    let resources = Arc::clone(process.runtime_context.resources());
    let limits = reactor_io_limits(&process.limits);
    let connected = Arc::new(Mutex::new(PendingJavascriptNetConnectState::default()));
    let task_connected = Arc::clone(&connected);
    if process
        .pending_javascript_net_connects
        .contains_key(&request_id)
    {
        return Err(SidecarError::InvalidState(format!(
            "ERR_AGENTOS_SOCKET_CONNECT_STATE: request {request_id} already has a pending connect"
        )));
    }
    process
        .pending_javascript_net_connects
        .insert(request_id, Arc::clone(&connected));
    let (respond_to, receiver) = tokio::sync::oneshot::channel();
    let spawn = runtime.spawn(agentos_runtime::TaskClass::Socket, async move {
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
                    Ok(socket) => {
                        task_connected
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .connected = Some(PendingJavascriptNetConnect::Tcp {
                            socket_id,
                            socket: Box::new(socket),
                            pending_capability,
                            local_reservation_id,
                        });
                        Ok(Value::Null)
                    }
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
        if respond_to.send(result).is_err() {
            eprintln!("ERR_AGENTOS_SOCKET_COMPLETION_DROPPED: TCP connect caller stopped waiting");
        }
    });
    if let Err(error) = spawn {
        process.pending_javascript_net_connects.remove(&request_id);
        return Err(SidecarError::from(error));
    }
    Ok(JavascriptSyncRpcServiceResponse::Deferred {
        receiver,
        timeout: None,
        task_class: agentos_runtime::TaskClass::Socket,
    })
}

impl ActiveTcpListener {
    pub(in crate::execution) fn bind(
        bind_host: &str,
        guest_host: &str,
        guest_port: u16,
        backlog: Option<u32>,
    ) -> Result<Self, SidecarError> {
        let bind_addr = resolve_tcp_bind_addr(bind_host, 0)?;
        let guest_addr = resolve_tcp_bind_addr(guest_host, guest_port)?;
        let listener = TcpListener::bind(bind_addr).map_err(sidecar_net_error)?;
        listener.set_nonblocking(true).map_err(sidecar_net_error)?;
        let local_addr = listener.local_addr().map_err(sidecar_net_error)?;
        Ok(Self {
            listener: Some(listener),
            kernel_socket_id: None,
            local_addr: Some(local_addr),
            guest_local_addr: guest_addr,
            backlog: usize::try_from(backlog.unwrap_or(DEFAULT_JAVASCRIPT_NET_BACKLOG))
                .expect("default backlog fits within usize"),
            active_connection_ids: Arc::new(Mutex::new(BTreeSet::new())),
            description_handles: Arc::new(()),
            description_lease: Arc::new(SocketDescriptionLease::default()),
            kernel_transfer_guard: None,
        })
    }

    pub(in crate::execution) fn bind_kernel(
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        guest_host: &str,
        guest_port: u16,
        backlog: Option<u32>,
    ) -> Result<Self, SidecarError> {
        let guest_addr = resolve_tcp_bind_addr(guest_host, guest_port)?;
        let spec = match guest_addr {
            SocketAddr::V4(_) => SocketSpec::tcp(),
            SocketAddr::V6(_) => SocketSpec::new(SocketDomain::Inet6, SocketType::Stream),
        };
        let socket_id = kernel
            .socket_create(EXECUTION_DRIVER_NAME, kernel_pid, spec)
            .map_err(kernel_error)?;
        kernel
            .socket_bind_inet(
                EXECUTION_DRIVER_NAME,
                kernel_pid,
                socket_id,
                InetSocketAddress::new(guest_addr.ip().to_string(), guest_addr.port()),
            )
            .map_err(kernel_error)?;
        kernel
            .socket_listen(
                EXECUTION_DRIVER_NAME,
                kernel_pid,
                socket_id,
                usize::try_from(backlog.unwrap_or(DEFAULT_JAVASCRIPT_NET_BACKLOG))
                    .expect("default backlog fits within usize"),
            )
            .map_err(kernel_error)?;
        Ok(Self {
            listener: None,
            kernel_socket_id: Some(socket_id),
            local_addr: Some(guest_addr),
            guest_local_addr: guest_addr,
            backlog: usize::try_from(backlog.unwrap_or(DEFAULT_JAVASCRIPT_NET_BACKLOG))
                .expect("default backlog fits within usize"),
            active_connection_ids: Arc::new(Mutex::new(BTreeSet::new())),
            description_handles: Arc::new(()),
            description_lease: Arc::new(SocketDescriptionLease::default()),
            kernel_transfer_guard: None,
        })
    }

    pub(in crate::execution) fn clone_for_fd_transfer(&self) -> Result<Self, SidecarError> {
        Ok(Self {
            listener: self
                .listener
                .as_ref()
                .map(TcpListener::try_clone)
                .transpose()
                .map_err(sidecar_net_error)?,
            kernel_socket_id: self.kernel_socket_id,
            local_addr: self.local_addr,
            guest_local_addr: self.guest_local_addr,
            backlog: self.backlog,
            active_connection_ids: Arc::clone(&self.active_connection_ids),
            description_handles: Arc::clone(&self.description_handles),
            description_lease: Arc::clone(&self.description_lease),
            kernel_transfer_guard: self.kernel_transfer_guard.clone(),
        })
    }

    pub(in crate::execution) fn is_final_description_handle(&self) -> bool {
        Arc::strong_count(&self.description_handles) == 1
    }

    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_addr.unwrap_or(self.guest_local_addr)
    }

    pub(in crate::execution) fn guest_local_addr(&self) -> SocketAddr {
        self.guest_local_addr
    }

    pub(in crate::execution) fn poll(
        &mut self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
        wait: Duration,
        trace_enabled: bool,
    ) -> Result<Option<JavascriptTcpListenerEvent>, SidecarError> {
        if let Some(socket_id) = self.kernel_socket_id {
            let poll_started = Instant::now();
            let result = kernel
                .poll_targets(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    vec![PollTargetEntry::socket(socket_id, POLLIN)],
                    i32::try_from(wait.as_millis()).unwrap_or(i32::MAX),
                )
                .map_err(kernel_error)?;
            let poll_elapsed = poll_started.elapsed();
            let revents = result
                .targets
                .first()
                .map(|entry| entry.revents)
                .unwrap_or_else(PollEvents::empty);
            record_net_tcp_kernel_poll(trace_enabled, wait, poll_elapsed, revents);
            if revents.is_empty() {
                return Ok(None);
            }
            let accepted_socket_id =
                match kernel.socket_accept(EXECUTION_DRIVER_NAME, kernel_pid, socket_id) {
                    Ok(accepted_socket_id) => accepted_socket_id,
                    Err(error) if error.code() == "EAGAIN" => {
                        if trace_enabled {
                            NET_TCP_TRACE_COUNTERS
                                .server_accept_eagain
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        return Ok(None);
                    }
                    Err(error) => {
                        if trace_enabled {
                            NET_TCP_TRACE_COUNTERS
                                .server_accept_errors
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        return Ok(Some(JavascriptTcpListenerEvent::Error {
                            code: Some(error.code().to_string()),
                            message: error.to_string(),
                        }));
                    }
                };
            let accepted = kernel.socket_get(accepted_socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "accepted kernel TCP socket {accepted_socket_id} is missing"
                ))
            })?;
            let local_addr = accepted.local_address().ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "accepted kernel TCP socket {accepted_socket_id} missing local address"
                ))
            })?;
            let remote_addr = accepted.peer_address().ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "accepted kernel TCP socket {accepted_socket_id} missing peer address"
                ))
            })?;
            if trace_enabled {
                NET_TCP_TRACE_COUNTERS
                    .server_accept_connections
                    .fetch_add(1, Ordering::Relaxed);
            }
            return Ok(Some(JavascriptTcpListenerEvent::Connection(
                PendingTcpSocket {
                    stream: None,
                    kernel_socket_id: Some(accepted_socket_id),
                    guest_local_addr: resolve_tcp_bind_addr(local_addr.host(), local_addr.port())?,
                    guest_remote_addr: resolve_tcp_bind_addr(
                        remote_addr.host(),
                        remote_addr.port(),
                    )?,
                },
            )));
        }

        let deadline = Instant::now() + wait;
        loop {
            match self
                .listener
                .as_ref()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from("TCP listener socket missing"))
                })?
                .accept()
            {
                Ok((stream, remote_addr)) => {
                    if self
                        .active_connection_ids
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .len()
                        >= self.backlog
                    {
                        let _ = stream.shutdown(Shutdown::Both);
                        if wait.is_zero() || Instant::now() >= deadline {
                            return Ok(None);
                        }
                        continue;
                    }
                    return Ok(Some(JavascriptTcpListenerEvent::Connection(
                        PendingTcpSocket {
                            stream: Some(stream),
                            kernel_socket_id: None,
                            guest_local_addr: self.guest_local_addr,
                            guest_remote_addr: remote_addr,
                        },
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if wait.is_zero() || Instant::now() >= deadline {
                        return Ok(None);
                    }
                    if !wait_fd_readable_until(
                        self.listener
                            .as_ref()
                            .expect("TCP listener checked before accept")
                            .as_fd(),
                        deadline,
                    ) {
                        return Ok(None);
                    }
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

    pub(in crate::execution) fn close(
        &self,
        kernel: &mut SidecarKernel,
        kernel_pid: u32,
    ) -> Result<(), SidecarError> {
        if let Some(socket_id) = self.kernel_socket_id {
            close_kernel_socket_idempotent(kernel, kernel_pid, socket_id)?;
        }
        Ok(())
    }

    pub(in crate::execution) fn active_connection_count(&self) -> usize {
        self.active_connection_ids
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }

    pub(in crate::execution) fn register_connection(
        &self,
        socket_id: &str,
    ) -> Arc<ListenerConnectionRetirement> {
        self.active_connection_ids
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(socket_id.to_string());
        ListenerConnectionRetirement::new(&self.active_connection_ids, socket_id.to_string())
    }

    pub(in crate::execution) fn retain_description_lease(
        &self,
        lease: Arc<agentos_runtime::capability::CapabilityLease>,
    ) {
        self.description_lease.retain(lease);
    }
}

// UDP types moved to crate::state

pub(crate) fn build_javascript_socket_path_context(
    vm: &VmState,
) -> Result<JavascriptSocketPathContext, SidecarError> {
    let mut abstract_namespace_digest = Sha256::new();
    abstract_namespace_digest.update(b"agentos-vm-unix-abstract-v1\0");
    abstract_namespace_digest.update(vm.connection_id.as_bytes());
    abstract_namespace_digest.update(b"\0");
    abstract_namespace_digest.update(vm.session_id.as_bytes());
    let unix_abstract_namespace = abstract_namespace_digest.finalize().into();
    let mut loopback_exempt_ports = vm.create_loopback_exempt_ports.clone();
    loopback_exempt_ports.extend(vm.configuration.loopback_exempt_ports.iter().copied());
    let mut tcp_loopback_guest_to_host_ports = BTreeMap::new();
    let mut http_loopback_targets = BTreeMap::new();
    let mut udp_loopback_guest_to_host_ports = BTreeMap::new();
    let mut udp_loopback_host_to_guest_ports = BTreeMap::new();
    let mut used_tcp_guest_ports = BTreeMap::new();
    let mut used_udp_guest_ports = BTreeMap::new();
    for (process_id, process) in &vm.active_processes {
        collect_javascript_socket_port_state(
            &vm.kernel,
            process_id,
            process,
            &mut tcp_loopback_guest_to_host_ports,
            &mut http_loopback_targets,
            &mut udp_loopback_guest_to_host_ports,
            &mut udp_loopback_host_to_guest_ports,
            &mut used_tcp_guest_ports,
            &mut used_udp_guest_ports,
        );
    }
    Ok(JavascriptSocketPathContext {
        sandbox_root: vm.cwd.clone(),
        unix_abstract_namespace,
        unix_socket_host_dir: vm.unix_socket_host_dir.clone(),
        unix_bound_addresses: Arc::clone(&vm.unix_address_registry),
        host_net_transfer_descriptions: Arc::clone(&vm.host_net_transfer_descriptions),
        mounts: vm.configuration.mounts.clone(),
        listen_policy: vm.listen_policy,
        loopback_exempt_ports,
        tcp_loopback_guest_to_host_ports,
        http_loopback_targets,
        udp_loopback_guest_to_host_ports,
        udp_loopback_host_to_guest_ports,
        used_tcp_guest_ports,
        used_udp_guest_ports,
    })
}

pub(crate) fn finalize_javascript_net_connect(
    process: &mut ActiveProcess,
    kernel_readiness: &KernelSocketReadinessRegistry,
    connected: Arc<Mutex<PendingJavascriptNetConnectState>>,
) -> Result<Value, SidecarError> {
    let mut state = connected.lock().map_err(|_| {
        SidecarError::InvalidState(String::from(
            "ERR_AGENTOS_SOCKET_CONNECT_STATE: completion lock poisoned",
        ))
    })?;
    let connected = state.connected.take().ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "ERR_AGENTOS_SOCKET_CONNECT_STATE: successful connect had no socket",
        ))
    })?;
    let bound_unix_listener = state.bound_unix_listener.take();
    drop(state);
    match connected {
        PendingJavascriptNetConnect::Tcp {
            socket_id,
            socket,
            pending_capability,
            local_reservation_id,
        } => {
            let local_addr = socket.guest_local_addr;
            let remote_addr = socket.guest_remote_addr;
            let capability_key = NativeCapabilityKey::TcpSocket(socket_id.clone());
            let identity = commit_process_capability(
                process,
                pending_capability,
                capability_key.clone(),
                socket_id.clone(),
                socket.kernel_socket_id,
            )?;
            if let Some(reservation_id) = local_reservation_id {
                process.tcp_port_reservations.remove(&reservation_id);
            }
            socket.set_event_pusher(
                process.execution.javascript_v8_session_handle(),
                Some(identity),
            );
            socket.set_fairness_identity(process.capability_fairness_identity(&capability_key))?;
            socket.retain_description_lease(
                process
                    .shared_capability_lease(&capability_key)
                    .expect("committed TCP capability lease"),
            );
            register_kernel_readiness_target(
                kernel_readiness,
                socket.kernel_socket_id,
                process.execution.javascript_v8_session_handle(),
                Some(Arc::clone(&socket.read_event_notify)),
                process.capability_readiness_identity(&capability_key),
                socket_id.clone(),
                KernelSocketReadinessEvent::Data,
            );
            process.tcp_sockets.insert(socket_id.clone(), *socket);
            Ok(json!({
                "socketId": socket_id,
                "capabilityId": identity.0,
                "capabilityGeneration": identity.1,
                "localAddress": local_addr.ip().to_string(),
                "localPort": local_addr.port(),
                "remoteAddress": remote_addr.ip().to_string(),
                "remotePort": remote_addr.port(),
                "remoteFamily": socket_addr_family(&remote_addr),
            }))
        }
        PendingJavascriptNetConnect::Unix {
            socket_id,
            socket,
            pending_capability,
            remote_path,
            remote_abstract_path_hex,
        } => {
            if let Some((listener_id, mut listener)) = bound_unix_listener {
                process.release_capability(&NativeCapabilityKey::UnixListener(listener_id))?;
                // Ownership of the private host pathname moves to the connected
                // socket. Do not unlink it when the consumed listener drops.
                listener.private_host_path.take();
            }
            let capability_key = NativeCapabilityKey::UnixSocket(socket_id.clone());
            let local_path = socket.local_path.clone();
            let local_abstract_path_hex = socket.local_abstract_path_hex.clone();
            let identity = commit_process_capability(
                process,
                pending_capability,
                capability_key.clone(),
                socket_id.clone(),
                None,
            )?;
            socket.set_event_pusher(
                process.execution.javascript_v8_session_handle(),
                Some(identity),
            );
            socket.set_fairness_identity(process.capability_fairness_identity(&capability_key))?;
            socket.retain_description_lease(
                process
                    .shared_capability_lease(&capability_key)
                    .expect("committed Unix capability lease"),
            );
            process.unix_sockets.insert(socket_id.clone(), *socket);
            Ok(json!({
                "socketId": socket_id,
                "capabilityId": identity.0,
                "capabilityGeneration": identity.1,
                "localPath": local_path,
                "localAbstractPathHex": local_abstract_path_hex,
                "remotePath": remote_path,
                "remoteAbstractPathHex": remote_abstract_path_hex,
            }))
        }
    }
}

pub(crate) fn restore_pending_bound_unix_connect(
    process: &mut ActiveProcess,
    pending: &Arc<Mutex<PendingJavascriptNetConnectState>>,
) -> Result<(), SidecarError> {
    let bound = pending
        .lock()
        .map_err(|_| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_SOCKET_CONNECT_STATE: completion lock poisoned",
            ))
        })?
        .bound_unix_listener
        .take();
    if let Some((listener_id, listener)) = bound {
        process.unix_listeners.insert(listener_id, listener);
    }
    Ok(())
}

pub(in crate::execution) fn normalize_tcp_listen_host(
    host: Option<&str>,
) -> Result<(JavascriptSocketFamily, &'static str, &'static str), SidecarError> {
    match host.unwrap_or("127.0.0.1") {
        "127.0.0.1" | "localhost" => Ok((JavascriptSocketFamily::Ipv4, "127.0.0.1", "127.0.0.1")),
        "::1" => Ok((JavascriptSocketFamily::Ipv6, "::1", "::1")),
        "0.0.0.0" => Ok((JavascriptSocketFamily::Ipv4, "127.0.0.1", "0.0.0.0")),
        "::" => Ok((JavascriptSocketFamily::Ipv6, "::1", "::")),
        other => Err(SidecarError::Execution(format!(
            "EACCES: TCP listeners must bind to loopback or unspecified addresses, got {other}"
        ))),
    }
}

pub(in crate::execution) fn normalize_udp_bind_host(
    host: Option<&str>,
    family: JavascriptUdpFamily,
) -> Result<(&'static str, &'static str, JavascriptSocketFamily), SidecarError> {
    match (family, host) {
        (JavascriptUdpFamily::Ipv4, None) | (JavascriptUdpFamily::Ipv4, Some("0.0.0.0")) => {
            Ok(("127.0.0.1", "0.0.0.0", JavascriptSocketFamily::Ipv4))
        }
        (JavascriptUdpFamily::Ipv4, Some("127.0.0.1"))
        | (JavascriptUdpFamily::Ipv4, Some("localhost")) => {
            Ok(("127.0.0.1", "127.0.0.1", JavascriptSocketFamily::Ipv4))
        }
        (JavascriptUdpFamily::Ipv6, None) | (JavascriptUdpFamily::Ipv6, Some("::")) => {
            Ok(("::1", "::", JavascriptSocketFamily::Ipv6))
        }
        (JavascriptUdpFamily::Ipv6, Some("::1"))
        | (JavascriptUdpFamily::Ipv6, Some("localhost")) => {
            Ok(("::1", "::1", JavascriptSocketFamily::Ipv6))
        }
        (JavascriptUdpFamily::Ipv4, Some(other)) => Err(SidecarError::Execution(format!(
            "EACCES: udp4 sockets must bind to 127.0.0.1 or 0.0.0.0, got {other}"
        ))),
        (JavascriptUdpFamily::Ipv6, Some(other)) => Err(SidecarError::Execution(format!(
            "EACCES: udp6 sockets must bind to ::1 or ::, got {other}"
        ))),
    }
}

pub(in crate::execution) fn allocate_guest_listen_port(
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

pub(in crate::execution) fn socket_host_matches(requested: Option<&str>, actual: &str) -> bool {
    match requested {
        None => true,
        Some(requested) if requested == actual => true,
        Some(requested)
            if is_unspecified_socket_host(requested) && is_unspecified_socket_host(actual) =>
        {
            true
        }
        Some(requested) if is_unspecified_socket_host(requested) => is_loopback_socket_host(actual),
        Some(requested) if requested.eq_ignore_ascii_case("localhost") => {
            is_loopback_socket_host(actual)
        }
        _ => false,
    }
}

pub(in crate::execution) fn parse_proc_net_entries(
    table_path: &str,
) -> Result<Vec<ProcNetEntry>, SidecarError> {
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

pub(in crate::execution) fn resolve_tcp_bind_addr(
    host: &str,
    port: u16,
) -> Result<SocketAddr, SidecarError> {
    (host, port)
        .to_socket_addrs()
        .map_err(sidecar_net_error)?
        .next()
        .ok_or_else(|| {
            SidecarError::Execution(format!("failed to resolve TCP bind address {host}:{port}"))
        })
}

fn tls_command_admission_error(
    error: tokio::sync::mpsc::error::TrySendError<NativeTlsCommand>,
    limit: usize,
) -> SidecarError {
    match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => SidecarError::Execution(format!(
            "ERR_AGENTOS_TLS_COMMAND_LIMIT: TLS command queue exceeded {limit}; raise limits.reactor.maxHandleCommands"
        )),
        tokio::sync::mpsc::error::TrySendError::Closed(_) => SidecarError::Execution(
            String::from("EPIPE: TLS transport task is closed"),
        ),
    }
}

pub(in crate::execution) fn plain_socket_command_admission_error(
    error: tokio::sync::mpsc::error::TrySendError<NativePlainSocketCommand>,
) -> SidecarError {
    match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => SidecarError::Execution(String::from(
            "ERR_AGENTOS_HANDLE_COMMAND_LIMIT: socket command queue is full; raise runtime.resources.maxHandleCommands",
        )),
        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
            SidecarError::Execution(String::from("EPIPE: socket transport task is closed"))
        }
    }
}

pub(in crate::execution) fn reserve_plain_socket_write_payload(
    resources: &Arc<ResourceLedger>,
    contents: &[u8],
) -> Result<PlainSocketWritePayload, SidecarError> {
    let command = resources
        .reserve(ResourceClass::HandleCommands, 1)
        .map_err(SidecarError::from)?;
    let bytes = resources
        .reserve(ResourceClass::HandleCommandBytes, contents.len())
        .map_err(SidecarError::from)?;
    let buffered = resources
        .reserve(ResourceClass::BufferedBytes, contents.len())
        .map_err(SidecarError::from)?;
    Ok(PlainSocketWritePayload {
        bytes: contents.to_vec(),
        _command_reservation: SharedReservation::new(command),
        _bytes_reservation: SharedReservation::new(bytes),
        _buffered_reservation: SharedReservation::new(buffered),
    })
}

pub(in crate::execution) fn reserve_plain_socket_command(
    resources: &Arc<ResourceLedger>,
) -> Result<SharedReservation, SidecarError> {
    resources
        .reserve(ResourceClass::HandleCommands, 1)
        .map(SharedReservation::new)
        .map_err(SidecarError::from)
}

pub(in crate::execution) enum PlainSocketWriteStream {
    Tcp(tokio::net::TcpStream),
    Unix(tokio::net::UnixStream),
}

impl PlainSocketWriteStream {
    pub(in crate::execution) async fn writable(&self) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.writable().await,
            Self::Unix(stream) => stream.writable().await,
        }
    }

    fn try_write(&self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.try_write(bytes),
            Self::Unix(stream) => stream.try_write(bytes),
        }
    }

    fn try_shutdown_write(&self) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => SockRef::from(stream).shutdown(Shutdown::Write),
            Self::Unix(stream) => SockRef::from(stream).shutdown(Shutdown::Write),
        }
    }
}

pub(in crate::execution) async fn committed_socket_fairness_identity(
    identity: &OnceLock<(u64, u64)>,
    committed: &tokio::sync::Notify,
) -> (u64, u64) {
    loop {
        let notified = committed.notified();
        if let Some(identity) = identity.get().copied() {
            return identity;
        }
        notified.await;
    }
}

pub(in crate::execution) async fn acquire_plain_socket_fair_turn(
    runtime: &agentos_runtime::RuntimeContext,
    limits: ReactorIoLimits,
    identity: &OnceLock<(u64, u64)>,
    committed: &tokio::sync::Notify,
) -> Result<FairWorkTurn, SidecarError> {
    let (capability_id, vm_generation) =
        committed_socket_fairness_identity(identity, committed).await;
    runtime
        .fairness()
        .acquire(
            vm_generation,
            capability_id,
            FairBudget::new(limits.operation_quantum.max(1), limits.byte_quantum.max(1)),
        )
        .await
        .map_err(|error| SidecarError::Execution(error.to_string()))
}

async fn run_plain_socket_fair_step<F>(
    runtime: &agentos_runtime::RuntimeContext,
    limits: ReactorIoLimits,
    identity: &OnceLock<(u64, u64)>,
    committed: &tokio::sync::Notify,
    operation: F,
) -> Result<std::io::Result<()>, SidecarError>
where
    F: FnOnce() -> std::io::Result<()>,
{
    let turn = acquire_plain_socket_fair_turn(runtime, limits, identity, committed).await?;
    // The closure is deliberately synchronous: exactly one nonblocking socket
    // syscall runs under the process-global grant, which is settled before this
    // task can suspend again.
    let result = operation();
    turn.complete(FairBudget::new(1, 0), false)
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    Ok(result)
}

pub(in crate::execution) async fn run_plain_socket_transport(
    stream: PlainSocketWriteStream,
    mut commands: TokioReceiver<NativePlainSocketCommand>,
    runtime: agentos_runtime::RuntimeContext,
    limits: ReactorIoLimits,
    fairness_identity: Arc<OnceLock<(u64, u64)>>,
    fairness_identity_committed: Arc<tokio::sync::Notify>,
) {
    while let Some(command) = commands.recv().await {
        match command {
            NativePlainSocketCommand::Write {
                payload,
                completion,
            } => {
                let written = payload.bytes.len();
                let result = match tokio::time::timeout(limits.operation_deadline, async {
                    let mut offset = 0;
                    while offset < payload.bytes.len() {
                        stream.writable().await?;
                        let (capability_id, vm_generation) = committed_socket_fairness_identity(
                            &fairness_identity,
                            &fairness_identity_committed,
                        )
                        .await;
                        let turn = runtime
                            .fairness()
                            .acquire(
                                vm_generation,
                                capability_id,
                                FairBudget::new(
                                    limits.operation_quantum.max(1),
                                    limits.byte_quantum.max(1),
                                ),
                            )
                            .await
                            .map_err(std::io::Error::other)?;
                        let chunk_len = turn
                            .allowance()
                            .bytes
                            .min(limits.byte_quantum.max(1))
                            .min(payload.bytes.len() - offset)
                            .max(1);
                        match stream.try_write(&payload.bytes[offset..offset + chunk_len]) {
                            Ok(bytes) => {
                                turn.complete(FairBudget::new(1, bytes), false)
                                    .map_err(std::io::Error::other)?;
                                offset += bytes;
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                turn.complete(FairBudget::new(1, 0), false)
                                    .map_err(std::io::Error::other)?;
                            }
                            Err(error) => {
                                turn.complete(FairBudget::new(1, 0), false)
                                    .map_err(std::io::Error::other)?;
                                return Err(error);
                            }
                        }
                    }
                    Ok(())
                })
                .await
                {
                    Ok(Ok(())) => Ok(json!(written)),
                    Ok(Err(error)) => Err(deferred_rpc_error(
                        "ERR_AGENTOS_SOCKET_WRITE",
                        error.to_string(),
                    )),
                    Err(_) => Err(deferred_rpc_error(
                        "ETIMEDOUT",
                        format!(
                            "socket write exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                            limits.operation_deadline.as_millis()
                        ),
                    )),
                };
                if completion.send(result).is_err() {
                    eprintln!(
                        "ERR_AGENTOS_SOCKET_COMPLETION_DROPPED: plain socket write caller stopped waiting"
                    );
                }
            }
            NativePlainSocketCommand::Shutdown {
                _command_reservation: _,
                completion,
            } => {
                let result = match tokio::time::timeout(limits.operation_deadline, async {
                    run_plain_socket_fair_step(
                        &runtime,
                        limits,
                        &fairness_identity,
                        &fairness_identity_committed,
                        || stream.try_shutdown_write(),
                    )
                    .await
                    .map_err(std::io::Error::other)?
                })
                .await {
                    Ok(Ok(())) => Ok(Value::Null),
                    Ok(Err(error)) => Err(deferred_rpc_error(
                        "ERR_AGENTOS_SOCKET_SHUTDOWN",
                        error.to_string(),
                    )),
                    Err(_) => Err(deferred_rpc_error(
                        "ETIMEDOUT",
                        format!(
                            "socket shutdown exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                            limits.operation_deadline.as_millis()
                        ),
                    )),
                };
                if completion.send(result).is_err() {
                    eprintln!(
                        "ERR_AGENTOS_SOCKET_COMPLETION_DROPPED: plain socket shutdown caller stopped waiting"
                    );
                }
            }
        }
    }
}

pub(in crate::execution) fn plain_socket_command_capacity(
    resources: &ResourceLedger,
) -> Result<usize, SidecarError> {
    resources
        .usage(ResourceClass::HandleCommands)
        .limit
        .filter(|limit| *limit > 0)
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_HANDLE_COMMAND_UNBOUNDED: runtime.resources.maxHandleCommands must be non-zero",
            ))
        })
}

fn spawn_tcp_plain_socket_transport(
    runtime: &agentos_runtime::RuntimeContext,
    stream: TcpStream,
    resources: &Arc<ResourceLedger>,
    limits: ReactorIoLimits,
    fairness_identity: Arc<OnceLock<(u64, u64)>>,
    fairness_identity_committed: Arc<tokio::sync::Notify>,
) -> Result<TokioSender<NativePlainSocketCommand>, SidecarError> {
    stream.set_nonblocking(true).map_err(sidecar_net_error)?;
    let (commands, receiver) = tokio_channel(plain_socket_command_capacity(resources)?);
    let cancellation = runtime.clone();
    runtime
        .spawn(agentos_runtime::TaskClass::Socket, async move {
            let transport_runtime = cancellation.clone();
            let transport = async move {
                match tokio::net::TcpStream::from_std(stream) {
                    Ok(stream) => {
                        run_plain_socket_transport(
                            PlainSocketWriteStream::Tcp(stream),
                            receiver,
                            transport_runtime,
                            limits,
                            fairness_identity,
                            fairness_identity_committed,
                        )
                        .await
                    }
                    Err(error) => eprintln!("ERR_AGENTOS_SOCKET_TRANSPORT: {error}"),
                }
            };
            tokio::select! {
                () = cancellation.admission_closed() => {}
                () = transport => {}
            }
        })
        .map_err(SidecarError::from)?;
    Ok(commands)
}

pub(in crate::execution) fn deferred_rpc_error(
    code: &'static str,
    message: impl Into<String>,
) -> crate::state::DeferredRpcError {
    crate::state::DeferredRpcError {
        code: String::from(code),
        message: message.into(),
    }
}

pub(in crate::execution) fn send_oneshot_or_log<T>(
    sender: tokio::sync::oneshot::Sender<T>,
    value: T,
    context: &'static str,
) {
    if sender.send(value).is_err() {
        eprintln!(
            "ERR_AGENTOS_COMPLETION_DROPPED: {context} receiver was cancelled before delivery"
        );
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

pub(in crate::execution) fn filter_dns_safe_ip_addrs(
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

    if let Some((ip, cidr, label)) = blocked {
        return Err(blocked_dns_resolution_error(&resource, ip, cidr, label));
    }

    if allowed.is_empty() {
        return Err(SidecarError::Execution(format!(
            "failed to resolve DNS address for {hostname}"
        )));
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

    if let Some(error) = blocked {
        return Err(error);
    }

    if allowed.is_empty() {
        return Err(SidecarError::Execution(format!(
            "failed to resolve outbound network address {host}:{port}"
        )));
    }

    Ok(allowed)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::execution) fn resolve_tcp_connect_addr<B>(
    bridge: &SharedBridge<B>,
    kernel: &SidecarKernel,
    vm_id: &str,
    dns: &VmDnsConfig,
    host: &str,
    port: u16,
    family: Option<u8>,
    context: &JavascriptSocketPathContext,
) -> Result<ResolvedTcpConnectAddr, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let allowed = filter_tcp_connect_ip_addrs(
        filter_dns_ip_addrs(
            resolve_dns_ip_addrs(
                bridge,
                kernel,
                vm_id,
                dns,
                host,
                DnsLookupPolicy::SkipPermissions,
            )?,
            family,
        )?,
        host,
        port,
        context,
    )?;
    bridge.require_resolved_network_access(
        vm_id,
        NetworkOperation::Http,
        &format_tcp_resource(host, port),
        &allowed
            .iter()
            .map(|ip| format_tcp_resource(&ip.to_string(), port))
            .collect::<Vec<_>>(),
    )?;
    let ip = allowed
        .iter()
        .copied()
        .find(|candidate| {
            let family = JavascriptSocketFamily::from_ip(*candidate);
            context.translate_tcp_loopback_port(family, port).is_some()
        })
        // We do not implement Happy Eyeballs yet, so prefer IPv4 over a
        // verbatim IPv6-first DNS answer for general outbound TCP connects.
        .or_else(|| allowed.iter().copied().find(IpAddr::is_ipv4))
        .or_else(|| allowed.first().copied())
        .ok_or_else(|| {
            SidecarError::Execution(format!("failed to resolve TCP address {host}:{port}"))
        })?;
    let family = JavascriptSocketFamily::from_ip(ip);
    let translated_loopback_port = context.translate_tcp_loopback_port(family, port);
    let use_kernel_loopback = is_loopback_ip(ip) && translated_loopback_port == Some(port);
    let actual_port = if is_loopback_ip(ip) {
        translated_loopback_port.unwrap_or(port)
    } else {
        port
    };
    Ok(ResolvedTcpConnectAddr {
        actual_addr: SocketAddr::new(ip, actual_port),
        guest_remote_addr: SocketAddr::new(ip, port),
        use_kernel_loopback,
    })
}

pub(in crate::execution) fn resolve_dns_ip_addrs<B>(
    bridge: &SharedBridge<B>,
    kernel: &SidecarKernel,
    vm_id: &str,
    dns: &VmDnsConfig,
    hostname: &str,
    policy: DnsLookupPolicy,
) -> Result<Vec<IpAddr>, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let resolution = match kernel.resolve_dns(hostname, policy) {
        Ok(resolution) => resolution,
        Err(error) => {
            let sidecar_error = kernel_error(error.clone());
            if error.code() != "EACCES" {
                emit_dns_resolution_failure_event(bridge, vm_id, hostname, dns, &sidecar_error);
            }
            return Err(sidecar_error);
        }
    };
    emit_dns_resolution_event(
        bridge,
        vm_id,
        hostname,
        resolution.source(),
        resolution.addresses(),
        dns,
    );
    Ok(resolution.addresses().to_vec())
}

pub(in crate::execution) fn resolve_dns_records<B>(
    bridge: &SharedBridge<B>,
    kernel: &SidecarKernel,
    vm_id: &str,
    dns: &VmDnsConfig,
    hostname: &str,
    record_type: RecordType,
    policy: DnsLookupPolicy,
) -> Result<DnsRecordResolution, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let resolution = match kernel.resolve_dns_records(hostname, record_type, policy) {
        Ok(resolution) => resolution,
        Err(error) => {
            let sidecar_error = kernel_error(error.clone());
            if error.code() != "EACCES" {
                emit_dns_resolution_failure_event(bridge, vm_id, hostname, dns, &sidecar_error);
            }
            return Err(sidecar_error);
        }
    };
    emit_dns_record_resolution_event(bridge, vm_id, hostname, &resolution, dns);
    Ok(resolution)
}

pub(in crate::execution) fn filter_dns_ip_addrs(
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

pub(in crate::execution) fn resolve_udp_bind_addr(
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

pub(in crate::execution) fn resolve_udp_addr<B>(
    request: UdpRemoteAddrRequest<'_, B>,
) -> Result<SocketAddr, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let UdpRemoteAddrRequest {
        bridge,
        kernel,
        vm_id,
        dns,
        host,
        port,
        family,
        context,
    } = request;
    let allowed = filter_tcp_connect_ip_addrs(
        resolve_dns_ip_addrs(
            bridge,
            kernel,
            vm_id,
            dns,
            host,
            DnsLookupPolicy::SkipPermissions,
        )?,
        host,
        port,
        context,
    )?;
    bridge.require_resolved_network_access(
        vm_id,
        NetworkOperation::Http,
        &format_tcp_resource(host, port),
        &allowed
            .iter()
            .map(|ip| format_tcp_resource(&ip.to_string(), port))
            .collect::<Vec<_>>(),
    )?;
    allowed
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

pub(in crate::execution) fn javascript_net_timeout_value() -> Value {
    Value::String(String::from(JAVASCRIPT_NET_TIMEOUT_SENTINEL))
}

pub(in crate::execution) fn javascript_net_json_string(
    value: Value,
    label: &str,
) -> Result<Value, SidecarError> {
    serde_json::to_string(&value)
        .map(Value::String)
        .map_err(|error| {
            SidecarError::InvalidState(format!("failed to serialize {label} payload: {error}"))
        })
}

pub(in crate::execution) fn javascript_net_read_value(
    event: Option<JavascriptTcpSocketEvent>,
) -> Result<Value, SidecarError> {
    match event {
        Some(JavascriptTcpSocketEvent::Data { bytes, .. }) => Ok(Value::String(
            base64::engine::general_purpose::STANDARD.encode(bytes),
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

pub(in crate::execution) fn io_error_code(error: &std::io::Error) -> Option<String> {
    match error.raw_os_error() {
        Some(libc::EACCES) => Some(String::from("EACCES")),
        Some(libc::EADDRINUSE) => Some(String::from("EADDRINUSE")),
        Some(libc::EADDRNOTAVAIL) => Some(String::from("EADDRNOTAVAIL")),
        Some(libc::EBADF) => Some(String::from("EBADF")),
        Some(libc::ECONNREFUSED) => Some(String::from("ECONNREFUSED")),
        Some(libc::ECONNRESET) => Some(String::from("ECONNRESET")),
        Some(libc::EDESTADDRREQ) => Some(String::from("EDESTADDRREQ")),
        Some(libc::EINVAL) => Some(String::from("EINVAL")),
        Some(libc::ENOPROTOOPT) => Some(String::from("ENOPROTOOPT")),
        Some(libc::ENOTCONN) => Some(String::from("ENOTCONN")),
        Some(libc::EOPNOTSUPP) => Some(String::from("EOPNOTSUPP")),
        Some(libc::EPIPE) => Some(String::from("EPIPE")),
        Some(libc::ETIMEDOUT) => Some(String::from("ETIMEDOUT")),
        Some(libc::EHOSTUNREACH) => Some(String::from("EHOSTUNREACH")),
        Some(libc::ENETUNREACH) => Some(String::from("ENETUNREACH")),
        _ => None,
    }
}

pub(in crate::execution) fn sidecar_net_error(error: std::io::Error) -> SidecarError {
    let message = match io_error_code(&error) {
        Some(code) => format!("{code}: {error}"),
        None => error.to_string(),
    };
    SidecarError::Execution(message)
}

struct PlainTcpReaderLease {
    running: Arc<AtomicBool>,
    stopped: Arc<tokio::sync::Notify>,
}

impl Drop for PlainTcpReaderLease {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        self.stopped.notify_waiters();
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the reader task receives explicit shared lifecycle flags owned by its socket"
)]
fn spawn_tcp_socket_reader(
    runtime: agentos_runtime::RuntimeContext,
    stream: TcpStream,
    sender: AsyncCompletionSender<JavascriptTcpSocketEvent>,
    read_event_notify: Arc<tokio::sync::Notify>,
    event_pusher: Arc<SocketReadinessSubscribers>,
    application_read_interest: Arc<AtomicBool>,
    application_read_notify: Arc<tokio::sync::Notify>,
    tls_mode: Arc<AtomicBool>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
    plain_reader_running: Arc<AtomicBool>,
    plain_reader_stopped: Arc<tokio::sync::Notify>,
    resources: Arc<ResourceLedger>,
    limits: ReactorIoLimits,
    fairness_identity: Arc<OnceLock<(u64, u64)>>,
    fairness_identity_committed: Arc<tokio::sync::Notify>,
) -> Result<(), SidecarError> {
    let (mut buffer, _read_buffer_reservation) =
        reserve_socket_read_buffer(&resources, limits.byte_quantum)?;
    let cancellation = runtime.clone();
    runtime
        .spawn(agentos_runtime::TaskClass::Socket, async move {
            let _lease = PlainTcpReaderLease {
                running: plain_reader_running,
                stopped: plain_reader_stopped,
            };
            let reader_runtime = cancellation.clone();
            let reader = async move {
                if let Err(error) = stream.set_nonblocking(true) {
                    send_async_socket_error_and_close(
                        &sender,
                        &event_pusher,
                        &close_notified,
                        io_error_code(&error),
                        error.to_string(),
                    )
                    .await;
                    read_event_notify.notify_one();
                    return;
                }
                let stream = match tokio::net::TcpStream::from_std(stream) {
                    Ok(stream) => stream,
                    Err(error) => {
                        send_async_socket_error_and_close(
                            &sender,
                            &event_pusher,
                            &close_notified,
                            io_error_code(&error),
                            error.to_string(),
                        )
                        .await;
                        read_event_notify.notify_one();
                        return;
                    }
                };
                loop {
                    if tls_mode.load(Ordering::SeqCst) {
                        break;
                    }
                    while !application_read_interest.load(Ordering::Acquire) {
                        let notified = application_read_notify.notified();
                        if application_read_interest.load(Ordering::Acquire) {
                            break;
                        }
                        notified.await;
                        if tls_mode.load(Ordering::SeqCst) {
                            return;
                        }
                    }
                    let ready = tokio::select! {
                        result = stream.readable() => result,
                        _ = application_read_notify.notified() => {
                            if tls_mode.load(Ordering::SeqCst) {
                                return;
                            }
                            continue;
                        }
                    };
                    if let Err(error) = ready {
                        let code = io_error_code(&error);
                        send_async_socket_error_and_close(
                            &sender,
                            &event_pusher,
                            &close_notified,
                            code,
                            error.to_string(),
                        )
                        .await;
                        read_event_notify.notify_one();
                        break;
                    }
                    let turn = match acquire_plain_socket_fair_turn(
                        &reader_runtime,
                        limits,
                        &fairness_identity,
                        &fairness_identity_committed,
                    )
                    .await
                    {
                        Ok(turn) => turn,
                        Err(error) => {
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                Some(String::from("ERR_AGENTOS_FAIRNESS")),
                                error.to_string(),
                            )
                            .await;
                            read_event_notify.notify_one();
                            break;
                        }
                    };
                    let read_capacity = buffer.len().min(turn.allowance().bytes).max(1);
                    let read_result = stream.try_read(&mut buffer[..read_capacity]);
                    let used_bytes = read_result.as_ref().copied().unwrap_or(0);
                    if let Err(error) = turn.complete(FairBudget::new(1, used_bytes), false) {
                        send_async_socket_error_and_close(
                            &sender,
                            &event_pusher,
                            &close_notified,
                            Some(String::from("ERR_AGENTOS_FAIRNESS")),
                            error.to_string(),
                        )
                        .await;
                        read_event_notify.notify_one();
                        break;
                    }
                    match read_result {
                        Ok(0) => {
                            saw_remote_end.store(true, Ordering::SeqCst);
                            if sender.send(JavascriptTcpSocketEvent::End).await.is_err() {
                                break;
                            }
                            read_event_notify.notify_one();
                            push_socket_event(&event_pusher, "end");
                            if saw_local_shutdown.load(Ordering::SeqCst)
                                && !close_notified.swap(true, Ordering::SeqCst)
                                && sender
                                    .send(JavascriptTcpSocketEvent::Close { had_error: false })
                                    .await
                                    .is_ok()
                            {
                                read_event_notify.notify_one();
                                push_socket_event(&event_pusher, "close");
                            }
                            break;
                        }
                        Ok(bytes_read) => {
                            let Some(reservation) = reserve_socket_event_bytes_or_close(
                                &resources,
                                bytes_read,
                                &sender,
                                &event_pusher,
                                &close_notified,
                            )
                            .await
                            else {
                                read_event_notify.notify_one();
                                break;
                            };
                            if sender
                                .send(JavascriptTcpSocketEvent::Data {
                                    bytes: buffer[..bytes_read].to_vec(),
                                    reservation: SharedReservation::new(reservation),
                                    source_reservations: Vec::new(),
                                })
                                .await
                                .is_err()
                            {
                                break;
                            }
                            read_event_notify.notify_one();
                            push_socket_event(&event_pusher, "data");
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                        Err(error) => {
                            let code = io_error_code(&error);
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                code,
                                error.to_string(),
                            )
                            .await;
                            read_event_notify.notify_one();
                            break;
                        }
                    }
                }
            };
            tokio::select! {
                () = cancellation.admission_closed() => {}
                () = reader => {}
            }
        })
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    Ok(())
}

pub(in crate::execution) async fn reserve_socket_event_bytes_or_close(
    resources: &ResourceLedger,
    bytes: usize,
    sender: &AsyncCompletionSender<JavascriptTcpSocketEvent>,
    event_pusher: &Arc<SocketReadinessSubscribers>,
    close_notified: &Arc<AtomicBool>,
) -> Option<Reservation> {
    match resources
        .reserve_when_available(ResourceClass::BufferedBytes, bytes)
        .await
    {
        Ok(reservation) => Some(reservation),
        Err(error) => {
            send_async_socket_error_and_close(
                sender,
                event_pusher,
                close_notified,
                Some("ERR_AGENTOS_RESOURCE_LIMIT".to_string()),
                error.to_string(),
            )
            .await;
            None
        }
    }
}

pub(in crate::execution) async fn reserve_tls_event_bytes_or_close(
    resources: &ResourceLedger,
    bytes: usize,
    sender: &AsyncCompletionSender<JavascriptTcpSocketEvent>,
    event_pusher: &Arc<SocketReadinessSubscribers>,
    close_notified: &Arc<AtomicBool>,
) -> Option<(Reservation, Reservation)> {
    let buffered = match resources
        .reserve_when_available(ResourceClass::BufferedBytes, bytes)
        .await
    {
        Ok(reservation) => reservation,
        Err(error) => {
            send_async_socket_error_and_close(
                sender,
                event_pusher,
                close_notified,
                Some("ERR_AGENTOS_RESOURCE_LIMIT".to_string()),
                error.to_string(),
            )
            .await;
            return None;
        }
    };
    match resources
        .reserve_when_available(ResourceClass::TlsBytes, bytes)
        .await
    {
        Ok(tls) => Some((buffered, tls)),
        Err(error) => {
            drop(buffered);
            send_async_socket_error_and_close(
                sender,
                event_pusher,
                close_notified,
                Some("ERR_AGENTOS_RESOURCE_LIMIT".to_string()),
                error.to_string(),
            )
            .await;
            None
        }
    }
}

pub(in crate::execution) async fn send_async_socket_error_and_close(
    sender: &AsyncCompletionSender<JavascriptTcpSocketEvent>,
    event_pusher: &Arc<SocketReadinessSubscribers>,
    close_notified: &Arc<AtomicBool>,
    code: Option<String>,
    message: String,
) {
    if sender
        .send(JavascriptTcpSocketEvent::Error { code, message })
        .await
        .is_ok()
    {
        push_socket_event(event_pusher, "error");
    }
    if !close_notified.swap(true, Ordering::SeqCst)
        && sender
            .send(JavascriptTcpSocketEvent::Close { had_error: true })
            .await
            .is_ok()
    {
        push_socket_event(event_pusher, "close");
    }
}

pub(in crate::execution) fn net_tcp_trace_enabled(env: &BTreeMap<String, String>) -> bool {
    env.get("AGENTOS_NET_BRIDGE_TRACE").map(String::as_str) == Some("1")
}

pub(in crate::execution) fn duration_micros_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

pub(in crate::execution) fn net_tcp_trace_reset() {
    reset_socket_read_trace();
    set_socket_read_trace_enabled(true);
    for counter in [
        &NET_TCP_TRACE_COUNTERS.socket_read_calls,
        &NET_TCP_TRACE_COUNTERS.socket_read_zero_wait_calls,
        &NET_TCP_TRACE_COUNTERS.socket_read_data_events,
        &NET_TCP_TRACE_COUNTERS.socket_read_bytes,
        &NET_TCP_TRACE_COUNTERS.socket_read_kernel_us,
        &NET_TCP_TRACE_COUNTERS.socket_read_end_events,
        &NET_TCP_TRACE_COUNTERS.socket_read_eagain,
        &NET_TCP_TRACE_COUNTERS.socket_read_errors,
        &NET_TCP_TRACE_COUNTERS.socket_read_push_attempts,
        &NET_TCP_TRACE_COUNTERS.socket_read_push_sent,
        &NET_TCP_TRACE_COUNTERS.socket_read_push_missing,
        &NET_TCP_TRACE_COUNTERS.socket_read_push_errors,
        &NET_TCP_TRACE_COUNTERS.socket_write_calls,
        &NET_TCP_TRACE_COUNTERS.socket_write_bytes,
        &NET_TCP_TRACE_COUNTERS.socket_write_kernel_us,
        &NET_TCP_TRACE_COUNTERS.socket_write_errors,
        &NET_TCP_TRACE_COUNTERS.server_accept_calls,
        &NET_TCP_TRACE_COUNTERS.server_accept_zero_wait_calls,
        &NET_TCP_TRACE_COUNTERS.server_accept_connections,
        &NET_TCP_TRACE_COUNTERS.server_accept_eagain,
        &NET_TCP_TRACE_COUNTERS.server_accept_errors,
        &NET_TCP_TRACE_COUNTERS.kernel_poll_targets,
        &NET_TCP_TRACE_COUNTERS.kernel_poll_zero_wait_calls,
        &NET_TCP_TRACE_COUNTERS.kernel_poll_wait_us,
        &NET_TCP_TRACE_COUNTERS.kernel_poll_elapsed_us,
        &NET_TCP_TRACE_COUNTERS.kernel_poll_empty,
        &NET_TCP_TRACE_COUNTERS.kernel_poll_ready,
        &NET_TCP_TRACE_COUNTERS.kernel_poll_revents_read,
        &NET_TCP_TRACE_COUNTERS.kernel_poll_revents_hup,
        &NET_TCP_TRACE_COUNTERS.kernel_poll_revents_err,
        &NET_TCP_TRACE_COUNTERS.kernel_poll_revents_bits_or,
    ] {
        counter.store(0, Ordering::Relaxed);
    }
}

pub(in crate::execution) fn net_tcp_trace_snapshot() -> Value {
    let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
    let socket_read = socket_read_trace_snapshot();
    json!({
        "socketReadCalls": load(&NET_TCP_TRACE_COUNTERS.socket_read_calls),
        "socketReadZeroWaitCalls": load(&NET_TCP_TRACE_COUNTERS.socket_read_zero_wait_calls),
        "socketReadDataEvents": load(&NET_TCP_TRACE_COUNTERS.socket_read_data_events),
        "socketReadBytes": load(&NET_TCP_TRACE_COUNTERS.socket_read_bytes),
        "socketReadKernelUs": load(&NET_TCP_TRACE_COUNTERS.socket_read_kernel_us),
        "socketReadRecordCloneCalls": socket_read.socket_record_clone_calls,
        "socketReadRecordCloneUs": socket_read.socket_record_clone_us,
        "socketReadRecvCalls": socket_read.read_recv_calls,
        "socketReadRecvBytes": socket_read.read_recv_bytes,
        "socketReadRecvChunks": socket_read.read_recv_chunks,
        "socketReadRecvCopyUs": socket_read.read_recv_copy_us,
        "socketReadEndEvents": load(&NET_TCP_TRACE_COUNTERS.socket_read_end_events),
        "socketReadEagain": load(&NET_TCP_TRACE_COUNTERS.socket_read_eagain),
        "socketReadErrors": load(&NET_TCP_TRACE_COUNTERS.socket_read_errors),
        "socketReadPushAttempts": load(&NET_TCP_TRACE_COUNTERS.socket_read_push_attempts),
        "socketReadPushSent": load(&NET_TCP_TRACE_COUNTERS.socket_read_push_sent),
        "socketReadPushMissing": load(&NET_TCP_TRACE_COUNTERS.socket_read_push_missing),
        "socketReadPushErrors": load(&NET_TCP_TRACE_COUNTERS.socket_read_push_errors),
        "socketWriteCalls": load(&NET_TCP_TRACE_COUNTERS.socket_write_calls),
        "socketWriteBytes": load(&NET_TCP_TRACE_COUNTERS.socket_write_bytes),
        "socketWriteKernelUs": load(&NET_TCP_TRACE_COUNTERS.socket_write_kernel_us),
        "socketWriteErrors": load(&NET_TCP_TRACE_COUNTERS.socket_write_errors),
        "serverAcceptCalls": load(&NET_TCP_TRACE_COUNTERS.server_accept_calls),
        "serverAcceptZeroWaitCalls": load(&NET_TCP_TRACE_COUNTERS.server_accept_zero_wait_calls),
        "serverAcceptConnections": load(&NET_TCP_TRACE_COUNTERS.server_accept_connections),
        "serverAcceptEagain": load(&NET_TCP_TRACE_COUNTERS.server_accept_eagain),
        "serverAcceptErrors": load(&NET_TCP_TRACE_COUNTERS.server_accept_errors),
        "kernelPollTargets": load(&NET_TCP_TRACE_COUNTERS.kernel_poll_targets),
        "kernelPollZeroWaitCalls": load(&NET_TCP_TRACE_COUNTERS.kernel_poll_zero_wait_calls),
        "kernelPollWaitUs": load(&NET_TCP_TRACE_COUNTERS.kernel_poll_wait_us),
        "kernelPollElapsedUs": load(&NET_TCP_TRACE_COUNTERS.kernel_poll_elapsed_us),
        "kernelPollEmpty": load(&NET_TCP_TRACE_COUNTERS.kernel_poll_empty),
        "kernelPollReady": load(&NET_TCP_TRACE_COUNTERS.kernel_poll_ready),
        "kernelPollReventsRead": load(&NET_TCP_TRACE_COUNTERS.kernel_poll_revents_read),
        "kernelPollReventsHup": load(&NET_TCP_TRACE_COUNTERS.kernel_poll_revents_hup),
        "kernelPollReventsErr": load(&NET_TCP_TRACE_COUNTERS.kernel_poll_revents_err),
        "kernelPollReventsBitsOr": load(&NET_TCP_TRACE_COUNTERS.kernel_poll_revents_bits_or),
    })
}

fn record_net_tcp_kernel_poll(
    enabled: bool,
    wait: Duration,
    elapsed: Duration,
    revents: PollEvents,
) {
    if !enabled {
        return;
    }
    NET_TCP_TRACE_COUNTERS
        .kernel_poll_targets
        .fetch_add(1, Ordering::Relaxed);
    if wait.is_zero() {
        NET_TCP_TRACE_COUNTERS
            .kernel_poll_zero_wait_calls
            .fetch_add(1, Ordering::Relaxed);
    }
    NET_TCP_TRACE_COUNTERS
        .kernel_poll_wait_us
        .fetch_add(duration_micros_u64(wait), Ordering::Relaxed);
    NET_TCP_TRACE_COUNTERS
        .kernel_poll_elapsed_us
        .fetch_add(duration_micros_u64(elapsed), Ordering::Relaxed);
    if revents.is_empty() {
        NET_TCP_TRACE_COUNTERS
            .kernel_poll_empty
            .fetch_add(1, Ordering::Relaxed);
    } else {
        NET_TCP_TRACE_COUNTERS
            .kernel_poll_ready
            .fetch_add(1, Ordering::Relaxed);
        NET_TCP_TRACE_COUNTERS
            .kernel_poll_revents_bits_or
            .fetch_or(u64::from(revents.bits()), Ordering::Relaxed);
    }
    if revents.intersects(POLLIN) {
        NET_TCP_TRACE_COUNTERS
            .kernel_poll_revents_read
            .fetch_add(1, Ordering::Relaxed);
    }
    if revents.intersects(POLLHUP) {
        NET_TCP_TRACE_COUNTERS
            .kernel_poll_revents_hup
            .fetch_add(1, Ordering::Relaxed);
    }
    if revents.intersects(POLLERR) {
        NET_TCP_TRACE_COUNTERS
            .kernel_poll_revents_err
            .fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod socket_read_limit_tests {
    use super::*;

    fn data_event(bytes: &[u8]) -> JavascriptTcpSocketEvent {
        let resources = ResourceLedger::root(
            "tcp-partial-read-test",
            [(
                ResourceClass::BufferedBytes,
                ResourceLimit::new(1024, "test.maxBufferedBytes"),
            )],
        );
        let reservation = resources
            .reserve(ResourceClass::BufferedBytes, bytes.len())
            .expect("reserve test socket bytes");
        JavascriptTcpSocketEvent::Data {
            bytes: bytes.to_vec(),
            reservation: SharedReservation::new(reservation),
            source_reservations: Vec::new(),
        }
    }

    fn event_bytes(event: Option<JavascriptTcpSocketEvent>) -> Vec<u8> {
        match event.expect("expected socket data event") {
            JavascriptTcpSocketEvent::Data { bytes, .. } => bytes,
            other => panic!("expected socket data, got {other:?}"),
        }
    }

    #[test]
    fn partial_socket_reads_preserve_the_unread_suffix() {
        let (first, remainder) = limit_tcp_socket_event(Some(data_event(b"abcdefgh")), 3);
        assert_eq!(event_bytes(first), b"abc");

        let (second, remainder) = limit_tcp_socket_event(remainder, 3);
        assert_eq!(event_bytes(second), b"def");

        let (third, remainder) = limit_tcp_socket_event(remainder, 3);
        assert_eq!(event_bytes(third), b"gh");
        assert!(remainder.is_none());
    }
}

#[cfg(test)]
mod plain_socket_fairness_tests {
    use super::*;

    #[test]
    fn transferred_description_retires_transport_identity_after_last_alias() {
        let process_runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create transferred socket fairness test runtime");
        let runtime = process_runtime.context();
        let vm_generation = runtime
            .allocate_vm_generation()
            .expect("allocate transferred socket fairness generation");
        let identity = Arc::new(OnceLock::new());
        identity
            .set((92_010, vm_generation))
            .expect("commit transferred socket fairness identity");
        let original = SocketFairnessRetirement::new(Arc::clone(&identity), runtime.clone());
        let transferred_alias = Arc::clone(&original);

        runtime.handle().block_on(async {
            let initial = runtime
                .fairness()
                .acquire(vm_generation, 92_010, FairBudget::new(1, 1))
                .await
                .expect("transport identity starts live");
            initial
                .complete(FairBudget::new(1, 1), false)
                .expect("complete initial transport turn");

            drop(original);
            let after_sender_close = runtime
                .fairness()
                .acquire(vm_generation, 92_010, FairBudget::new(1, 1))
                .await
                .expect("sender close must not retire a transferred description");
            after_sender_close
                .complete(FairBudget::new(1, 1), false)
                .expect("complete transferred transport turn");

            drop(transferred_alias);
            let error = runtime
                .fairness()
                .acquire(vm_generation, 92_010, FairBudget::new(1, 1))
                .await
                .expect_err("last alias drop must retire the transport identity");
            assert!(matches!(
                error,
                agentos_runtime::fairness::FairnessError::CapabilityRetired {
                    vm_generation: retired_generation,
                    capability_id: 92_010,
                } if retired_generation == vm_generation
            ));
        });
    }

    #[test]
    fn shutdown_step_releases_the_process_fairness_turn_before_follow_up_waits() {
        let process_runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create plain socket fairness test runtime");
        let runtime = process_runtime.context();
        let first_generation = runtime
            .allocate_vm_generation()
            .expect("allocate first shutdown fairness generation");
        let second_generation = runtime
            .allocate_vm_generation()
            .expect("allocate second shutdown fairness generation");
        let identity = Arc::new(OnceLock::new());
        identity
            .set((92_001, first_generation))
            .expect("commit shutdown fairness identity");
        let committed = Arc::new(tokio::sync::Notify::new());
        let limits = reactor_io_limits(&crate::limits::VmLimits::default());

        runtime.handle().block_on(async {
            run_plain_socket_fair_step(&runtime, limits, &identity, &committed, || Ok(()))
                .await
                .expect("run synchronous shutdown fairness step")
                .expect("synthetic shutdown syscall succeeds");

            // A transport may perform later async cleanup, but the shutdown
            // syscall's process-global grant must already be settled.
            let (_cleanup_tx, cleanup_rx) = tokio::sync::oneshot::channel::<()>();
            let pending_cleanup = tokio::spawn(async move {
                let _ = cleanup_rx.await;
            });
            tokio::task::yield_now().await;
            assert!(!pending_cleanup.is_finished());

            let next = tokio::time::timeout(
                Duration::from_secs(1),
                runtime
                    .fairness()
                    .acquire(second_generation, 92_002, FairBudget::new(1, 1)),
            )
            .await
            .expect("post-shutdown cleanup must not hold the global fairness turn")
            .expect("acquire second VM fairness turn");
            next.complete(FairBudget::new(1, 1), false)
                .expect("complete second VM fairness turn");

            pending_cleanup.abort();
            runtime
                .fairness()
                .retire_vm(first_generation)
                .expect("retire first shutdown fairness generation");
            runtime
                .fairness()
                .retire_vm(second_generation)
                .expect("retire second shutdown fairness generation");
        });
    }
}

#[cfg(test)]
mod transferred_alias_transport_tests {
    use super::*;

    fn exercise_surviving_tcp_alias(close_sender: bool) {
        let process_runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create transferred TCP test runtime");
        let process_context = process_runtime.context();
        let generation = process_context
            .allocate_vm_generation()
            .expect("allocate transferred TCP test generation");
        let resources = Arc::clone(process_context.resources());
        let runtime = process_context.scoped_for_vm(Arc::clone(&resources), generation);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind TCP alias test listener");
        let mut peer = TcpStream::connect(listener.local_addr().expect("listener address"))
            .expect("connect TCP alias test peer");
        peer.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set peer read timeout");
        let (accepted, remote_addr) = listener.accept().expect("accept TCP alias test peer");
        let local_addr = accepted.local_addr().expect("accepted local address");
        let original = ActiveTcpSocket::from_stream(
            accepted,
            None,
            local_addr,
            remote_addr,
            resources,
            runtime.clone(),
            reactor_io_limits(&crate::limits::VmLimits::default()),
        )
        .expect("create original TCP description");
        original
            .set_fairness_identity(Some((81_001, generation)))
            .expect("commit TCP fairness identity");
        let transferred = original.clone_for_fd_transfer();
        assert!(!original.is_final_description_handle());
        assert!(!transferred.is_final_description_handle());

        let survivor = if close_sender {
            drop(original);
            transferred
        } else {
            drop(transferred);
            original
        };
        assert!(survivor.is_final_description_handle());

        survivor
            .application_read_interest
            .store(true, Ordering::Release);
        survivor.application_read_notify.notify_waiters();
        survivor
            .ensure_tcp_reader()
            .expect("start TCP alias reader");
        peer.write_all(b"host-to-survivor")
            .expect("write to surviving alias");
        runtime.handle().block_on(async {
            tokio::time::timeout(
                Duration::from_secs(2),
                survivor.read_event_notify.notified(),
            )
            .await
            .expect("surviving alias receives a transport wake");
        });
        let event = survivor
            .events
            .as_ref()
            .expect("TCP event queue")
            .lock()
            .expect("TCP event queue lock")
            .try_recv()
            .expect("surviving alias reads queued data");
        match event {
            JavascriptTcpSocketEvent::Data { bytes, .. } => {
                assert_eq!(bytes, b"host-to-survivor")
            }
            other => panic!("expected TCP data after alias close, got {other:?}"),
        }

        let completion = survivor
            .begin_plain_write(b"survivor-to-host")
            .expect("write through surviving alias");
        runtime.handle().block_on(async {
            tokio::time::timeout(Duration::from_secs(2), completion)
                .await
                .expect("surviving alias write completes")
                .expect("surviving alias write completion sender")
                .expect("surviving alias write succeeds");
        });
        let mut bytes = [0_u8; 16];
        peer.read_exact(&mut bytes)
            .expect("peer reads surviving alias write");
        assert_eq!(&bytes, b"survivor-to-host");

        drop(peer);
        drop(survivor);
        runtime.close_admission();
    }

    #[test]
    fn transferred_tcp_child_close_leaves_sender_wake_read_and_write_live() {
        exercise_surviving_tcp_alias(false);
    }

    #[test]
    fn transferred_tcp_sender_close_leaves_child_wake_read_and_write_live() {
        exercise_surviving_tcp_alias(true);
    }
}

#[cfg(test)]
mod ssrf_egress_classifier_tests {
    // F-005/006/007 (sec-sidecar T1/T7/T11): the egress classifier must treat the
    // unspecified address (0.0.0.0 / ::), CGNAT (100.64.0.0/10), IPv6 spellings of
    // restricted IPv4 targets (::a.b.c.d), and reserved/multicast (240/4, 224/4) as
    // restricted. 0.0.0.0 routes to 127.0.0.1 on connect(), so leaving it
    // unclassified let a guest bypass the loopback port-ownership gate.
    //
    // These are bounded SAFEGUARD tests: they exercise the classifier and the DNS
    // egress filter directly (no network I/O, no Node), so they run fast and
    // deterministically. See FAILURES.md#F-005, #F-006, #F-007.
    use super::{
        filter_dns_ip_addrs, filter_dns_safe_ip_addrs, filter_tcp_connect_ip_addrs, is_loopback_ip,
        restricted_non_loopback_ip_range, JavascriptSocketFamily, JavascriptSocketPathContext,
        SidecarError, VmListenPolicy,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn socket_policy_context() -> JavascriptSocketPathContext {
        JavascriptSocketPathContext {
            sandbox_root: PathBuf::from("/tmp/agentos-egress-policy-test"),
            unix_abstract_namespace: [0; 32],
            unix_socket_host_dir: PathBuf::from("/tmp/agentos-egress-policy-test/unix"),
            unix_bound_addresses: Arc::new(Mutex::new(BTreeMap::new())),
            host_net_transfer_descriptions: Arc::new(Mutex::new(BTreeMap::new())),
            mounts: Vec::new(),
            listen_policy: VmListenPolicy::default(),
            loopback_exempt_ports: BTreeSet::new(),
            tcp_loopback_guest_to_host_ports: BTreeMap::new(),
            http_loopback_targets: BTreeMap::new(),
            udp_loopback_guest_to_host_ports: BTreeMap::new(),
            udp_loopback_host_to_guest_ports: BTreeMap::new(),
            used_tcp_guest_ports: BTreeMap::new(),
            used_udp_guest_ports: BTreeMap::new(),
        }
    }

    fn assert_restricted(ip: IpAddr, expected_label: &str) {
        let classification = restricted_non_loopback_ip_range(ip);
        assert!(
            classification.is_some(),
            "{ip} must be classified as a restricted egress target"
        );
        let (_cidr, label) = classification.unwrap();
        assert_eq!(
            label, expected_label,
            "{ip} should be labelled {expected_label}, got {label}"
        );
    }

    fn assert_dns_denied(ip: IpAddr, label: &str) {
        match filter_dns_safe_ip_addrs(vec![ip], "attacker.example") {
            Err(SidecarError::Execution(message)) => assert!(
                message.starts_with("EACCES:"),
                "{label}: egress filter must deny with EACCES, got: {message}"
            ),
            other => panic!("{label}: expected EACCES denial, got {other:?}"),
        }
    }

    #[test]
    fn post_dns_socket_policy_filters_every_udp_candidate() {
        let context = socket_policy_context();
        let public = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34));
        let private = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7));
        let mixed_error =
            filter_tcp_connect_ip_addrs(vec![private, public], "mixed.example", 53, &context)
                .expect_err("a mixed safe/blocked answer must fail closed as a unit");
        assert!(mixed_error.to_string().starts_with("EACCES:"));

        let error = filter_tcp_connect_ip_addrs(vec![private], "rebound.example", 53, &context)
            .expect_err("a DNS answer that rebinds entirely into private space is denied");
        assert!(error.to_string().starts_with("EACCES:"));
    }

    #[test]
    fn tcp_socket_family_filters_mixed_dns_answers_before_connect() {
        let ipv4 = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34));
        let ipv6 = IpAddr::V6("2606:2800:220:1:248:1893:25c8:1946".parse().unwrap());

        assert_eq!(
            filter_dns_ip_addrs(vec![ipv6, ipv4], Some(4)).unwrap(),
            vec![ipv4]
        );
        assert_eq!(
            filter_dns_ip_addrs(vec![ipv4, ipv6], Some(6)).unwrap(),
            vec![ipv6]
        );
    }

    #[test]
    fn same_vm_udp_guest_port_passes_loopback_ownership_gate() {
        let mut context = socket_policy_context();
        let guest_port = 4242;
        context
            .udp_loopback_guest_to_host_ports
            .insert((JavascriptSocketFamily::Ipv4, guest_port), guest_port);

        assert_eq!(
            filter_tcp_connect_ip_addrs(
                vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                "127.0.0.1",
                guest_port,
                &context,
            )
            .expect("same-VM UDP guest port must pass the loopback ownership gate"),
            vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]
        );
        assert!(
            filter_tcp_connect_ip_addrs(
                vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                "127.0.0.1",
                guest_port + 1,
                &context,
            )
            .is_err(),
            "an unrelated loopback port must remain denied"
        );
    }

    // F-005 (sec-sidecar T1).
    #[test]
    fn classifier_denies_unspecified_and_cgnat_targets() {
        // 0.0.0.0 (IPv4 unspecified) -> would route to host loopback.
        assert_restricted(IpAddr::V4(Ipv4Addr::UNSPECIFIED), "unspecified");
        // :: (IPv6 unspecified).
        assert_restricted(IpAddr::V6(Ipv6Addr::UNSPECIFIED), "unspecified");

        // CGNAT 100.64.0.0/10 spans 100.64.x.x .. 100.127.x.x.
        assert_restricted(
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            "carrier-grade-nat",
        );
        assert_restricted(
            IpAddr::V4(Ipv4Addr::new(100, 127, 255, 254)),
            "carrier-grade-nat",
        );

        // Guard against over-blocking: addresses just outside 100.64/10 stay allowed.
        assert!(
            restricted_non_loopback_ip_range(IpAddr::V4(Ipv4Addr::new(100, 63, 255, 255)))
                .is_none(),
            "100.63.255.255 is outside CGNAT and must remain allowed"
        );
        assert!(
            restricted_non_loopback_ip_range(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 0))).is_none(),
            "100.128.0.0 is outside CGNAT and must remain allowed"
        );

        // The DNS egress filter must also deny these via EACCES.
        assert_dns_denied(IpAddr::V4(Ipv4Addr::UNSPECIFIED), "0.0.0.0 (unspecified)");
        assert_dns_denied(IpAddr::V6(Ipv6Addr::UNSPECIFIED), ":: (unspecified)");
        assert_dns_denied(
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            "100.64.0.1 (CGNAT)",
        );
    }

    // F-006 (sec-sidecar T7).
    #[test]
    fn classifier_denies_ipv6_spelled_metadata_addresses() {
        // The IPv4-mapped form (::ffff:169.254.169.254) was already handled; the
        // IPv4-compatible form (::169.254.169.254) is the gap this fixes.
        let mapped = "::ffff:169.254.169.254".parse::<Ipv6Addr>().unwrap();
        assert_restricted(IpAddr::V6(mapped), "link-local");

        let compat = "::169.254.169.254".parse::<Ipv6Addr>().unwrap();
        assert_restricted(IpAddr::V6(compat), "link-local");

        // Other IPv4-compatible private/CGNAT spellings must also be canonicalized.
        assert_restricted(
            IpAddr::V6("::10.0.0.1".parse::<Ipv6Addr>().unwrap()),
            "private",
        );
        assert_restricted(
            IpAddr::V6("::100.64.0.1".parse::<Ipv6Addr>().unwrap()),
            "carrier-grade-nat",
        );

        // Guard against over-blocking: the IPv6 unspecified/loopback addresses
        // are not IPv4-compatible host targets, and a public IPv4-compatible
        // address must remain allowed.
        assert_eq!(
            restricted_non_loopback_ip_range(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
            Some(("::/128", "unspecified")),
            ":: must classify as unspecified, not via the IPv4-compat path"
        );
        assert!(
            restricted_non_loopback_ip_range(IpAddr::V6(Ipv6Addr::LOCALHOST)).is_none()
                || is_loopback_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            "::1 must not be classified as a restricted IPv4-compatible target"
        );
        assert!(
            restricted_non_loopback_ip_range(IpAddr::V6("::8.8.8.8".parse::<Ipv6Addr>().unwrap()))
                .is_none(),
            "::8.8.8.8 (public IPv4-compatible) must remain allowed"
        );

        // The DNS egress filter must deny the IPv4-compat metadata spelling.
        assert_dns_denied(
            IpAddr::V6("::169.254.169.254".parse::<Ipv6Addr>().unwrap()),
            "::169.254.169.254 (IPv4-compat metadata)",
        );
    }

    // F-007 (sec-sidecar T11).
    #[test]
    fn classifier_denies_reserved_and_multicast_targets() {
        // 224.0.0.0/4 (multicast) and 240.0.0.0/4 (reserved / future use) are not
        // legitimate unicast egress targets; a guest connect to them must be
        // classified as restricted and denied.
        assert_restricted(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), "multicast");
        assert_restricted(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 255)), "multicast");
        assert_restricted(IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1)), "reserved");
        // 255.255.255.255 (limited broadcast) falls in 240.0.0.0/4.
        assert_restricted(IpAddr::V4(Ipv4Addr::BROADCAST), "reserved");

        // IPv4-compatible IPv6 spellings must canonicalize and be denied too.
        assert_restricted(
            IpAddr::V6("::224.0.0.1".parse::<Ipv6Addr>().unwrap()),
            "multicast",
        );
        assert_restricted(
            IpAddr::V6("::240.0.0.1".parse::<Ipv6Addr>().unwrap()),
            "reserved",
        );

        // Guard against over-blocking: addresses just outside 224/4 stay allowed.
        assert!(
            restricted_non_loopback_ip_range(IpAddr::V4(Ipv4Addr::new(223, 255, 255, 255)))
                .is_none(),
            "223.255.255.255 is outside 224/4 and must remain allowed"
        );

        // The DNS egress filter must also deny these via EACCES.
        assert_dns_denied(
            IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1)),
            "240.0.0.1 (reserved)",
        );
        assert_dns_denied(
            IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)),
            "224.0.0.1 (multicast)",
        );
    }
}
