use super::*;

static NEXT_SQLITE_HOST_NAMESPACE: AtomicU64 = AtomicU64::new(1);

/// Ownership of VM-wide retained-byte accounting for an event temporarily
/// removed from a process queue. Keeping this reservation alive across a
/// capacity check prevents a concurrent producer from consuming the bytes an
/// already-accepted event needs if that event must be put back.
#[derive(Debug)]
pub(super) struct PendingExecutionEventReservation {
    budget: Arc<VmPendingByteBudget>,
    bytes: usize,
}

impl PendingExecutionEventReservation {
    fn transfer_to_queue(mut self) {
        self.bytes = 0;
    }
}

impl Drop for PendingExecutionEventReservation {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}

#[derive(Debug)]
pub(super) struct PolledExecutionEvent {
    pub(super) event: ActiveExecutionEvent,
    pub(super) reservation: Option<PendingExecutionEventReservation>,
}

impl PolledExecutionEvent {
    pub(super) fn unreserved(event: ActiveExecutionEvent) -> Self {
        Self {
            event,
            reservation: None,
        }
    }

    pub(super) fn event(&self) -> &ActiveExecutionEvent {
        &self.event
    }

    pub(super) fn into_event(self) -> ActiveExecutionEvent {
        self.event
    }
}

impl ActiveProcess {
    pub(crate) fn new(
        kernel_pid: u32,
        kernel_handle: KernelProcessHandle,
        runtime_context: agentos_runtime::RuntimeContext,
        limits: crate::limits::VmLimits,
        process_event_capacity: usize,
        runtime: GuestRuntimeKind,
        execution: ActiveExecution,
    ) -> Self {
        let pending_event_count_limit =
            process_event_capacity.min(limits.process.pending_event_count);
        let pending_stdin_bytes_limit = limits.process.pending_stdin_bytes;
        let pending_event_bytes_limit = limits.process.pending_event_bytes;
        if let ActiveExecution::Binding(binding) = &execution {
            binding
                .pending_event_count_limit
                .store(pending_event_count_limit, Ordering::Release);
            binding
                .pending_event_bytes_limit
                .store(pending_event_bytes_limit, Ordering::Release);
        }
        // Binding producers lease retained-byte reservations from their own
        // queue before an event can be moved into the ActiveProcess queue.
        // Both queues must therefore start with the same budget identity; a
        // signal-state drain may temporarily lease stdout/exit and requeue it.
        let vm_pending_event_bytes_budget = match &execution {
            ActiveExecution::Binding(binding) => Arc::clone(&binding.vm_pending_event_bytes_budget),
            _ => VmPendingByteBudget::new(
                pending_event_bytes_limit,
                queue_tracker::TrackedLimit::PendingExecutionEventBytes,
            ),
        };
        Self {
            kernel_pid,
            kernel_handle,
            runtime_context,
            limits,
            kernel_stdin_writer_fd: None,
            direct_posix_stdin: false,
            kernel_stdin_reader_fd: 0,
            pending_kernel_stdin: PendingKernelStdin::default(),
            pending_kernel_stdin_gauge: queue_tracker::register_queue(
                queue_tracker::TrackedLimit::PendingKernelStdinBytes,
                pending_stdin_bytes_limit,
            ),
            vm_pending_stdin_bytes_budget: VmPendingByteBudget::new(
                pending_stdin_bytes_limit,
                queue_tracker::TrackedLimit::PendingKernelStdinBytes,
            ),
            tty_master_fd: None,
            runtime,
            detached: false,
            execution,
            guest_cwd: String::from("/"),
            env: BTreeMap::new(),
            host_cwd: PathBuf::from("/"),
            shadow_root: None,
            host_write_dirty: false,
            mapped_host_fds: BTreeMap::new(),
            next_mapped_host_fd: MAPPED_HOST_FD_START,
            process_event_notify: Arc::new(tokio::sync::Notify::new()),
            process_event_capacity,
            wasm_flock_fds: BTreeMap::new(),
            pending_execution_events: VecDeque::new(),
            pending_execution_event_bytes: 0,
            pending_execution_event_count_limit: pending_event_count_limit,
            pending_execution_event_bytes_limit: pending_event_bytes_limit,
            pending_execution_event_count_gauge: queue_tracker::register_queue(
                queue_tracker::TrackedLimit::PendingExecutionEvents,
                pending_event_count_limit,
            ),
            pending_execution_event_bytes_gauge: queue_tracker::register_queue(
                queue_tracker::TrackedLimit::PendingExecutionEventBytes,
                pending_event_bytes_limit,
            ),
            vm_pending_event_bytes_budget,
            pending_javascript_net_connects: BTreeMap::new(),
            pending_self_signal_exit: None,
            exit_signal: None,
            exit_core_dumped: false,
            pending_wasm_signals: BTreeSet::new(),
            pending_wasm_signals_gauge: queue_tracker::register_queue(
                queue_tracker::TrackedLimit::PendingWasmSignals,
                64,
            ),
            real_interval_timer: ActiveRealIntervalTimer::new(),
            child_processes: BTreeMap::new(),
            next_child_process_id: 0,
            pending_child_process_sync: BTreeMap::new(),
            http_servers: BTreeMap::new(),
            pending_http_requests: BTreeMap::new(),
            http2: Default::default(),
            capability_leases: BTreeMap::new(),
            tcp_listeners: BTreeMap::new(),
            next_tcp_listener_id: 0,
            tcp_sockets: BTreeMap::new(),
            next_tcp_socket_id: 0,
            tcp_port_reservations: BTreeMap::new(),
            next_tcp_port_reservation_id: 0,
            unix_listeners: BTreeMap::new(),
            next_unix_listener_id: 0,
            unix_sockets: BTreeMap::new(),
            next_unix_socket_id: 0,
            udp_sockets: BTreeMap::new(),
            next_udp_socket_id: 0,
            python_sockets: BTreeMap::new(),
            next_python_socket_id: 0,
            hash_sessions: BTreeMap::new(),
            next_hash_session_id: 0,
            cipher_sessions: BTreeMap::new(),
            next_cipher_session_id: 0,
            diffie_hellman_sessions: BTreeMap::new(),
            next_diffie_hellman_session_id: 0,
            sqlite_databases: BTreeMap::new(),
            sqlite_host_namespace: format!(
                "{}-{}",
                std::process::id(),
                NEXT_SQLITE_HOST_NAMESPACE.fetch_add(1, Ordering::Relaxed)
            ),
            next_sqlite_database_id: 0,
            sqlite_statements: BTreeMap::new(),
            next_sqlite_statement_id: 0,
            tty_master_owner: None,
            tty_raw_mode_generation: None,
            deferred_kernel_wait_rpc: None,
            deferred_child_write_timer: None,
            module_resolution_cache: agentos_execution::LocalModuleResolutionCache::default(),
        }
    }

    pub(crate) fn clear_deferred_kernel_wait_rpc(&mut self) {
        self.deferred_kernel_wait_rpc = None;
        if let Some(timer) = self.deferred_child_write_timer.take() {
            timer.abort();
        }
    }

    pub(crate) fn queue_pending_execution_event(
        &mut self,
        event: ActiveExecutionEvent,
    ) -> Result<(), SidecarError> {
        self.try_queue_pending_execution_event(event)
            .map_err(|(error, _event)| error)
    }

    // On admission failure the event must be returned intact so the caller can
    // requeue it without losing its accounting reservation.
    #[allow(clippy::result_large_err)]
    fn try_queue_pending_execution_event(
        &mut self,
        event: ActiveExecutionEvent,
    ) -> Result<(), (SidecarError, ActiveExecutionEvent)> {
        let event_bytes = event.retained_bytes();
        if self.pending_execution_events.len() >= self.pending_execution_event_count_limit {
            return Err((
                SidecarError::InvalidState(format!(
                    "process execution event queue exceeded {} events (limits.process.pendingEventCount/runtime.protocol.maxProcessEvents); raise the limiting setting",
                    self.pending_execution_event_count_limit
                )),
                event,
            ));
        }
        if self
            .pending_execution_event_bytes
            .saturating_add(event_bytes)
            > self.pending_execution_event_bytes_limit
        {
            return Err((
                SidecarError::InvalidState(format!(
                    "process execution event queue exceeded {} retained bytes (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes",
                    self.pending_execution_event_bytes_limit
                )),
                event,
            ));
        }
        if !self.vm_pending_event_bytes_budget.try_reserve(event_bytes) {
            return Err((
                SidecarError::InvalidState(format!(
                    "VM process execution event queues exceeded {} retained bytes (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes",
                    self.vm_pending_event_bytes_budget.limit()
                )),
                event,
            ));
        }
        self.pending_execution_event_bytes = self
            .pending_execution_event_bytes
            .saturating_add(event_bytes);
        self.pending_execution_events.push_back(event);
        self.pending_execution_event_count_gauge
            .observe_depth(self.pending_execution_events.len());
        self.pending_execution_event_bytes_gauge
            .observe_depth(self.pending_execution_event_bytes);
        self.process_event_notify.notify_one();
        Ok(())
    }

    #[allow(clippy::result_large_err)]
    pub(super) fn try_queue_pending_execution_envelope(
        &mut self,
        envelope: ProcessEventEnvelope,
    ) -> Result<(), (SidecarError, ProcessEventEnvelope)> {
        let ProcessEventEnvelope {
            connection_id,
            session_id,
            vm_id,
            process_id,
            event,
        } = envelope;
        self.try_queue_pending_execution_event(event)
            .map_err(|(error, event)| {
                (
                    error,
                    ProcessEventEnvelope {
                        connection_id,
                        session_id,
                        vm_id,
                        process_id,
                        event,
                    },
                )
            })
    }

    pub(super) fn lease_pending_execution_event(&mut self) -> Option<PolledExecutionEvent> {
        let event = self.pending_execution_events.pop_front()?;
        let event_bytes = event.retained_bytes();
        self.pending_execution_event_bytes = self
            .pending_execution_event_bytes
            .saturating_sub(event_bytes);
        self.pending_execution_event_count_gauge
            .observe_depth(self.pending_execution_events.len());
        self.pending_execution_event_bytes_gauge
            .observe_depth(self.pending_execution_event_bytes);
        Some(PolledExecutionEvent {
            event,
            reservation: Some(PendingExecutionEventReservation {
                budget: Arc::clone(&self.vm_pending_event_bytes_budget),
                bytes: event_bytes,
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn pop_pending_execution_event(&mut self) -> Option<ActiveExecutionEvent> {
        self.lease_pending_execution_event()
            .map(PolledExecutionEvent::into_event)
    }

    pub(super) fn requeue_pending_execution_event(
        &mut self,
        polled: PolledExecutionEvent,
    ) -> Result<(), SidecarError> {
        self.queue_polled_execution_event(polled, true)
    }

    pub(super) fn queue_pending_polled_execution_event(
        &mut self,
        polled: PolledExecutionEvent,
    ) -> Result<(), SidecarError> {
        self.queue_polled_execution_event(polled, false)
    }

    fn queue_polled_execution_event(
        &mut self,
        polled: PolledExecutionEvent,
        front: bool,
    ) -> Result<(), SidecarError> {
        let PolledExecutionEvent { event, reservation } = polled;
        let event_bytes = event.retained_bytes();
        if self.pending_execution_events.len() >= self.pending_execution_event_count_limit {
            return Err(SidecarError::InvalidState(format!(
                "process execution event queue exceeded {} events (limits.process.pendingEventCount/runtime.protocol.maxProcessEvents); raise the limiting setting",
                self.pending_execution_event_count_limit
            )));
        }
        if self
            .pending_execution_event_bytes
            .saturating_add(event_bytes)
            > self.pending_execution_event_bytes_limit
        {
            return Err(SidecarError::InvalidState(format!(
                "process execution event queue exceeded {} retained bytes (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes",
                self.pending_execution_event_bytes_limit
            )));
        }

        let reservation = match reservation {
            Some(reservation) => {
                if reservation.bytes != event_bytes
                    || !Arc::ptr_eq(&reservation.budget, &self.vm_pending_event_bytes_budget)
                {
                    return Err(SidecarError::InvalidState(String::from(
                        "process execution event reservation no longer matches its VM queue; event requeue aborted",
                    )));
                }
                Some(reservation)
            }
            None => {
                if !self.vm_pending_event_bytes_budget.try_reserve(event_bytes) {
                    return Err(SidecarError::InvalidState(format!(
                        "VM process execution event queues exceeded {} retained bytes (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes",
                        self.vm_pending_event_bytes_budget.limit()
                    )));
                }
                None
            }
        };

        self.pending_execution_event_bytes = self
            .pending_execution_event_bytes
            .saturating_add(event_bytes);
        if front {
            self.pending_execution_events.push_front(event);
        } else {
            self.pending_execution_events.push_back(event);
        }
        self.pending_execution_event_count_gauge
            .observe_depth(self.pending_execution_events.len());
        self.pending_execution_event_bytes_gauge
            .observe_depth(self.pending_execution_event_bytes);
        if let Some(reservation) = reservation {
            reservation.transfer_to_queue();
        }
        self.process_event_notify.notify_one();
        Ok(())
    }

    pub(super) async fn poll_execution_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<PolledExecutionEvent>, SidecarError> {
        if let ActiveExecution::Binding(execution) = &mut self.execution {
            return poll_binding_process_event_leased(execution);
        }
        self.execution
            .poll_event(timeout)
            .await
            .map(|event| event.map(PolledExecutionEvent::unreserved))
    }

    pub(super) fn try_poll_execution_event(
        &mut self,
    ) -> Result<Option<PolledExecutionEvent>, SidecarError> {
        if let ActiveExecution::Binding(execution) = &mut self.execution {
            return poll_binding_process_event_leased(execution);
        }
        self.execution
            .try_poll_event()
            .map(|event| event.map(PolledExecutionEvent::unreserved))
    }

    pub(crate) fn with_process_event_limits(
        mut self,
        limits: &agentos_native_sidecar_core::limits::ProcessLimits,
    ) -> Self {
        self.pending_execution_event_count_limit =
            self.process_event_capacity.min(limits.pending_event_count);
        self.pending_execution_event_bytes_limit = limits.pending_event_bytes;
        if let ActiveExecution::Binding(execution) = &self.execution {
            execution
                .pending_event_count_limit
                .store(self.pending_execution_event_count_limit, Ordering::Release);
            execution
                .pending_event_bytes_limit
                .store(limits.pending_event_bytes, Ordering::Release);
        }
        self.pending_kernel_stdin_gauge = queue_tracker::register_queue(
            queue_tracker::TrackedLimit::PendingKernelStdinBytes,
            limits.pending_stdin_bytes,
        );
        self.pending_execution_event_count_gauge = queue_tracker::register_queue(
            queue_tracker::TrackedLimit::PendingExecutionEvents,
            self.pending_execution_event_count_limit,
        );
        self.pending_execution_event_bytes_gauge = queue_tracker::register_queue(
            queue_tracker::TrackedLimit::PendingExecutionEventBytes,
            limits.pending_event_bytes,
        );
        self
    }

    pub(crate) fn with_vm_pending_byte_budgets(
        mut self,
        stdin: Arc<VmPendingByteBudget>,
        events: Arc<VmPendingByteBudget>,
    ) -> Self {
        debug_assert_eq!(self.pending_kernel_stdin.total, 0);
        debug_assert_eq!(self.pending_execution_event_bytes, 0);
        self.vm_pending_stdin_bytes_budget = stdin;
        self.vm_pending_event_bytes_budget = Arc::clone(&events);
        if let ActiveExecution::Binding(execution) = &mut self.execution {
            if !Arc::ptr_eq(&execution.vm_pending_event_bytes_budget, &events) {
                debug_assert_eq!(execution.pending_event_bytes.load(Ordering::Acquire), 0);
                execution.vm_pending_event_bytes_budget = events;
            }
        }
        self
    }

    pub(crate) fn queue_pending_wasm_signal(&mut self, signal: i32) -> Result<(), SidecarError> {
        self.pending_wasm_signals.insert(signal);
        self.pending_wasm_signals_gauge
            .observe_depth(self.pending_wasm_signals.len());
        Ok(())
    }

    pub(crate) fn with_event_notify(mut self, event_notify: Arc<tokio::sync::Notify>) -> Self {
        self.process_event_notify = event_notify;
        self
    }

    pub(crate) fn with_host_cwd(mut self, host_cwd: PathBuf) -> Self {
        self.host_cwd = host_cwd;
        self
    }

    pub(crate) fn with_shadow_root(mut self, shadow_root: PathBuf) -> Self {
        self.shadow_root = Some(shadow_root);
        self
    }

    pub(crate) fn mark_host_write_dirty(&mut self) {
        self.host_write_dirty = true;
    }

    pub(crate) fn host_write_dirty_recursive(&self) -> bool {
        self.host_write_dirty
            || self
                .child_processes
                .values()
                .any(ActiveProcess::host_write_dirty_recursive)
    }

    pub(crate) fn clean_host_writes_are_observable(&self) -> bool {
        matches!(
            self.execution,
            ActiveExecution::Javascript(_) | ActiveExecution::Python(_) | ActiveExecution::Wasm(_)
        )
    }

    pub(crate) fn clean_host_writes_are_observable_recursive(&self) -> bool {
        self.clean_host_writes_are_observable()
            && self
                .child_processes
                .values()
                .all(ActiveProcess::clean_host_writes_are_observable_recursive)
    }

    pub(crate) fn with_guest_cwd(mut self, guest_cwd: String) -> Self {
        self.guest_cwd = guest_cwd;
        self
    }

    pub(crate) fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    pub(crate) fn with_kernel_stdin_writer_fd(mut self, fd: u32) -> Self {
        self.kernel_stdin_writer_fd = Some(fd);
        self
    }

    pub(crate) fn with_tty_master_fd(mut self, fd: Option<u32>) -> Self {
        self.tty_master_fd = fd;
        self
    }

    pub(crate) fn with_detached(mut self, detached: bool) -> Self {
        self.detached = detached;
        self
    }

    pub(crate) fn allocate_mapped_host_fd(&mut self, fd: ActiveMappedHostFd) -> u32 {
        let handle = self.next_mapped_host_fd;
        self.next_mapped_host_fd = self
            .next_mapped_host_fd
            .checked_add(1)
            .unwrap_or(MAPPED_HOST_FD_START);
        self.mapped_host_fds.insert(handle, fd);
        handle
    }

    pub(crate) fn mapped_host_fd(&self, fd: u32) -> Option<&ActiveMappedHostFd> {
        self.mapped_host_fds.get(&fd)
    }

    pub(crate) fn mapped_host_fd_mut(&mut self, fd: u32) -> Option<&mut ActiveMappedHostFd> {
        self.mapped_host_fds.get_mut(&fd)
    }

    pub(crate) fn close_mapped_host_fd(&mut self, fd: u32) -> bool {
        self.mapped_host_fds.remove(&fd).is_some()
    }

    pub(crate) fn allocate_child_process_id(&mut self) -> String {
        self.next_child_process_id += 1;
        format!("child-{}", self.next_child_process_id)
    }

    pub(super) fn allocate_tcp_listener_id(&mut self) -> String {
        self.next_tcp_listener_id += 1;
        format!("listener-{}", self.next_tcp_listener_id)
    }

    pub(super) fn allocate_tcp_socket_id(&mut self) -> String {
        self.next_tcp_socket_id += 1;
        format!("socket-{}", self.next_tcp_socket_id)
    }

    pub(super) fn allocate_tcp_port_reservation_id(&mut self) -> String {
        self.next_tcp_port_reservation_id += 1;
        format!("tcp-port-reservation-{}", self.next_tcp_port_reservation_id)
    }

    pub(super) fn allocate_unix_listener_id(&mut self) -> String {
        self.next_unix_listener_id += 1;
        format!("unix-listener-{}", self.next_unix_listener_id)
    }

    pub(super) fn allocate_unix_socket_id(&mut self) -> String {
        self.next_unix_socket_id += 1;
        format!("unix-socket-{}", self.next_unix_socket_id)
    }

    pub(super) fn allocate_udp_socket_id(&mut self) -> String {
        self.next_udp_socket_id += 1;
        format!("udp-socket-{}", self.next_udp_socket_id)
    }

    #[allow(dead_code)]
    pub(crate) fn network_resource_counts(&self) -> NetworkResourceCounts {
        let mut counts = NetworkResourceCounts::default();
        let mut descriptions = BTreeMap::new();
        self.collect_network_resource_counts(false, &mut descriptions, &mut counts);
        add_host_net_description_counts(&descriptions, &mut counts);
        counts
    }

    fn collect_network_resource_counts(
        &self,
        sidecar_only: bool,
        descriptions: &mut BTreeMap<usize, bool>,
        counts: &mut NetworkResourceCounts,
    ) {
        counts.sockets += self.http_servers.len() + self.python_sockets.len();
        let http2 = self
            .http2
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        counts.sockets += http2.servers.len() + http2.sessions.len();
        counts.connections += http2.sessions.len();
        drop(http2);

        for listener in self.tcp_listeners.values() {
            if !sidecar_only || listener.kernel_socket_id.is_none() {
                descriptions
                    .entry(Arc::as_ptr(&listener.description_handles) as usize)
                    .or_insert(false);
            }
        }
        for socket in self.tcp_sockets.values() {
            if !sidecar_only || socket.kernel_socket_id.is_none() {
                descriptions.insert(Arc::as_ptr(&socket.description_handles) as usize, true);
            }
        }
        for listener in self.unix_listeners.values() {
            descriptions
                .entry(Arc::as_ptr(&listener.description_handles) as usize)
                .or_insert(false);
        }
        for socket in self.unix_sockets.values() {
            descriptions.insert(Arc::as_ptr(&socket.description_handles) as usize, true);
        }
        for socket in self.udp_sockets.values() {
            if !sidecar_only || socket.kernel_socket_id.is_none() {
                descriptions
                    .entry(Arc::as_ptr(&socket.description_handles) as usize)
                    .or_insert(false);
            }
        }
        for child in self.child_processes.values() {
            child.collect_network_resource_counts(sidecar_only, descriptions, counts);
        }
    }

    fn track_capability(
        &mut self,
        key: NativeCapabilityKey,
        lease: agentos_runtime::capability::CapabilityLease,
    ) -> Result<(), SidecarError> {
        match self.capability_leases.entry(key.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Arc::new(lease));
                Ok(())
            }
            std::collections::btree_map::Entry::Occupied(_) => Err(SidecarError::InvalidState(
                format!("ERR_AGENTOS_CAPABILITY_DUPLICATE: process already owns {key:?}"),
            )),
        }
    }

    pub(super) fn shared_capability_lease(
        &self,
        key: &NativeCapabilityKey,
    ) -> Option<Arc<agentos_runtime::capability::CapabilityLease>> {
        self.capability_leases.get(key).map(Arc::clone)
    }

    pub(super) fn release_capability(
        &mut self,
        key: &NativeCapabilityKey,
    ) -> Result<(), SidecarError> {
        self.release_capability_preserving_fairness(key, None)
    }

    /// Release a guest alias while allowing an open socket description to
    /// retain its stable transport scheduler identity. The description's RAII
    /// guard retires that identity after the final SCM_RIGHTS alias is gone.
    pub(super) fn release_capability_preserving_fairness(
        &mut self,
        key: &NativeCapabilityKey,
        preserved_identity: Option<(u64, u64)>,
    ) -> Result<(), SidecarError> {
        let lease = self.capability_leases.remove(key).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "ERR_AGENTOS_CAPABILITY_MISSING: process does not own {key:?}"
            ))
        })?;
        if let Some(session) = self.execution.javascript_v8_session_handle() {
            if let Err(error) = session.remove_readiness(lease.id(), lease.generation()) {
                eprintln!(
                    "ERR_AGENTOS_READY_REMOVE: capability={} generation={}: {error}",
                    lease.id(),
                    lease.generation()
                );
            }
        }
        if let Some(vm_generation) = self.runtime_context.vm_generation() {
            if preserved_identity != Some((lease.id(), vm_generation)) {
                self.runtime_context
                    .fairness()
                    .retire_capability(vm_generation, lease.id())
                    .map_err(|error| SidecarError::Execution(error.to_string()))?;
            }
        }
        Ok(())
    }

    pub(super) fn release_description_capability(
        &mut self,
        key: &NativeCapabilityKey,
        preserved_identity: Option<(u64, u64)>,
        description_lease: &SocketDescriptionLease,
    ) -> Result<(), SidecarError> {
        if self.capability_leases.contains_key(key) {
            return self.release_capability_preserving_fairness(key, preserved_identity);
        }
        if description_lease.is_retained() {
            return Ok(());
        }
        Err(SidecarError::InvalidState(format!(
            "ERR_AGENTOS_CAPABILITY_MISSING: process does not own {key:?} and the open description has no retained lease"
        )))
    }

    pub(super) fn release_capability_if_present(&mut self, key: &NativeCapabilityKey) {
        if let Some(lease) = self.capability_leases.remove(key) {
            if let Some(session) = self.execution.javascript_v8_session_handle() {
                if let Err(error) = session.remove_readiness(lease.id(), lease.generation()) {
                    eprintln!(
                        "ERR_AGENTOS_READY_REMOVE: capability={} generation={}: {error}",
                        lease.id(),
                        lease.generation()
                    );
                }
            }
            if let Some(vm_generation) = self.runtime_context.vm_generation() {
                if let Err(error) = self
                    .runtime_context
                    .fairness()
                    .retire_capability(vm_generation, lease.id())
                {
                    eprintln!(
                        "ERR_AGENTOS_FAIRNESS_RETIRE: capability={} vm_generation={vm_generation}: {error}",
                        lease.id()
                    );
                }
            }
        }
    }

    pub(super) fn capability_readiness_identity(
        &self,
        key: &NativeCapabilityKey,
    ) -> Option<(
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::CapabilityGeneration,
    )> {
        self.capability_leases
            .get(key)
            .map(|lease| (lease.id(), lease.generation()))
    }

    pub(super) fn capability_fairness_identity(
        &self,
        key: &NativeCapabilityKey,
    ) -> Option<(
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::SessionGeneration,
    )> {
        self.capability_leases.get(key).and_then(|lease| {
            self.runtime_context
                .vm_generation()
                .map(|generation| (lease.id(), generation))
        })
    }

    pub(super) fn validate_capability_alias(
        &self,
        key: &NativeCapabilityKey,
        kind: CapabilityKind,
    ) -> Result<(), SidecarError> {
        let generation = self.runtime_context.vm_generation().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_CAPABILITY_SESSION: process runtime is not VM-generation scoped",
            ))
        })?;
        let lease = self.capability_leases.get(key).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "ERR_AGENTOS_CAPABILITY_MISSING: process does not own {key:?}"
            ))
        })?;
        lease.validate(generation, kind).map_err(SidecarError::from)
    }
}

impl Drop for ActiveProcess {
    fn drop(&mut self) {
        if let Some(timer) = self.deferred_child_write_timer.take() {
            timer.abort();
        }
        let pending_stdin_bytes = self.pending_kernel_stdin.total;
        self.vm_pending_stdin_bytes_budget
            .release(pending_stdin_bytes);
        self.pending_kernel_stdin.clear();
        self.pending_kernel_stdin_gauge.observe_depth(0);

        self.vm_pending_event_bytes_budget
            .release(self.pending_execution_event_bytes);
        self.pending_execution_events.clear();
        self.pending_execution_event_bytes = 0;
        self.pending_execution_event_count_gauge.observe_depth(0);
        self.pending_execution_event_bytes_gauge.observe_depth(0);
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod pending_event_reservation_tests {
    use super::*;
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::{KernelVmConfig, SpawnOptions};
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::MemoryFileSystem;

    fn test_runtime_context() -> agentos_runtime::RuntimeContext {
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .expect("create test runtime")
            .context()
    }

    #[test]
    fn checked_out_event_keeps_vm_bytes_reserved_until_requeue_or_consumption() {
        let event = ActiveExecutionEvent::Stdout(vec![0x5a; 32]);
        let event_bytes = event.retained_bytes();
        let budget = VmPendingByteBudget::new(
            event_bytes,
            queue_tracker::TrackedLimit::PendingExecutionEventBytes,
        );
        let mut config = KernelVmConfig::new("vm-pending-event-reservation");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register execution driver");
        let handle = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn process");
        let mut process = ActiveProcess::new(
            handle.pid(),
            handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::WebAssembly,
            ActiveExecution::Binding(BindingExecution::default()),
        )
        .with_vm_pending_byte_budgets(
            VmPendingByteBudget::new(
                event_bytes,
                queue_tracker::TrackedLimit::PendingKernelStdinBytes,
            ),
            Arc::clone(&budget),
        );

        process
            .queue_pending_execution_event(event)
            .expect("initial event fits the VM aggregate");
        let checked_out = process
            .lease_pending_execution_event()
            .expect("lease accepted event");
        let sibling_budget = Arc::clone(&budget);
        let sibling = std::thread::spawn(move || sibling_budget.try_reserve(event_bytes));
        assert!(
            !sibling.join().expect("sibling producer thread"),
            "a sibling producer must not steal a checked-out event reservation"
        );

        process
            .requeue_pending_execution_event(checked_out)
            .expect("requeue reuses the reservation");
        assert!(matches!(
            process.pop_pending_execution_event(),
            Some(ActiveExecutionEvent::Stdout(bytes)) if bytes == vec![0x5a; 32]
        ));
        assert!(budget.try_reserve(event_bytes));
        budget.release(event_bytes);

        let internal_event = ActiveExecutionEvent::SignalState {
            signal: 10,
            registration: SignalHandlerRegistration {
                action: SignalDispositionAction::User,
                mask: Vec::new(),
                flags: 0,
            },
        };
        let internal_bytes = internal_event.retained_bytes();
        let internal_budget = VmPendingByteBudget::new(
            internal_bytes,
            queue_tracker::TrackedLimit::PendingExecutionEventBytes,
        );
        process = process.with_vm_pending_byte_budgets(
            VmPendingByteBudget::new(
                internal_bytes,
                queue_tracker::TrackedLimit::PendingKernelStdinBytes,
            ),
            Arc::clone(&internal_budget),
        );
        process
            .queue_pending_execution_event(internal_event)
            .expect("internal event fills aggregate budget");
        let consumed = process
            .lease_pending_execution_event()
            .expect("lease internal event")
            .into_event();
        assert!(matches!(
            consumed,
            ActiveExecutionEvent::SignalState { signal: 10, .. }
        ));
        assert!(internal_budget.try_reserve(internal_bytes));
        internal_budget.release(internal_bytes);

        process.kernel_handle.finish(0);
        kernel.waitpid(process.kernel_pid).expect("reap process");
    }

    #[test]
    fn root_binding_signal_state_drain_preserves_output_and_exit_events() {
        let event_budget = VmPendingByteBudget::new(
            1024,
            queue_tracker::TrackedLimit::PendingExecutionEventBytes,
        );
        let binding = BindingExecution::default()
            .with_vm_pending_event_bytes_budget(Arc::clone(&event_budget));
        let cancelled = Arc::clone(&binding.cancelled);
        let pending_events = Arc::clone(&binding.pending_events);
        let overflow_reason = Arc::clone(&binding.event_overflow_reason);
        let pending_bytes = Arc::clone(&binding.pending_event_bytes);
        let count_limit = Arc::clone(&binding.pending_event_count_limit);
        let bytes_limit = Arc::clone(&binding.pending_event_bytes_limit);

        let mut config = KernelVmConfig::new("root-binding-signal-state-drain");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register execution driver");
        let handle = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn binding process");
        let mut process = ActiveProcess::new(
            handle.pid(),
            handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::JavaScript,
            ActiveExecution::Binding(binding),
        );

        let ActiveExecution::Binding(binding) = &process.execution else {
            unreachable!("test process must retain binding execution");
        };
        assert!(Arc::ptr_eq(
            &binding.vm_pending_event_bytes_budget,
            &process.vm_pending_event_bytes_budget,
        ));

        for event in [
            ActiveExecutionEvent::Stdout(b"binding-output".to_vec()),
            ActiveExecutionEvent::Exited(0),
        ] {
            assert!(send_binding_process_event(
                &cancelled,
                &pending_events,
                &overflow_reason,
                &pending_bytes,
                &count_limit,
                &bytes_limit,
                &event_budget,
                event,
            ));
        }

        // `get_signal_state` leases every execution event while looking for
        // SignalState updates, then requeues unrelated stdout/exit events.
        let mut deferred = VecDeque::new();
        while let Some(event) = process
            .try_poll_execution_event()
            .expect("lease binding event")
        {
            deferred.push_back(event);
        }
        for event in deferred.into_iter().rev() {
            process
                .requeue_pending_execution_event(event)
                .expect("signal-state drain must preserve leased binding event");
        }

        assert!(matches!(
            process.pop_pending_execution_event(),
            Some(ActiveExecutionEvent::Stdout(bytes)) if bytes == b"binding-output"
        ));
        assert!(matches!(
            process.pop_pending_execution_event(),
            Some(ActiveExecutionEvent::Exited(0))
        ));
        assert!(process.pop_pending_execution_event().is_none());

        process.kernel_handle.finish(0);
        kernel.waitpid(process.kernel_pid).expect("reap process");
    }

    #[test]
    fn duplicate_process_capability_preserves_the_live_lease() {
        let resources = Arc::new(ResourceLedger::root(
            "vm=duplicate-process-capability",
            [
                (
                    ResourceClass::Capabilities,
                    ResourceLimit::new(2, "limits.reactor.maxCapabilities"),
                ),
                (
                    ResourceClass::ReadyHandles,
                    ResourceLimit::new(2, "limits.reactor.maxReadyHandles"),
                ),
                (
                    ResourceClass::Sockets,
                    ResourceLimit::new(2, "limits.resources.maxSockets"),
                ),
                (
                    ResourceClass::Connections,
                    ResourceLimit::new(2, "limits.resources.maxConnections"),
                ),
            ],
        ));
        let capabilities = CapabilityRegistry::new(7, Arc::clone(&resources));
        let first = capabilities
            .reserve(CapabilityKind::UdpSocket)
            .expect("reserve first capability")
            .commit(CapabilityBackend::Native {
                local_id: String::from("udp-first"),
            })
            .expect("commit first capability");
        let first_id = first.id();
        let duplicate = capabilities
            .reserve(CapabilityKind::UdpSocket)
            .expect("reserve duplicate capability")
            .commit(CapabilityBackend::Native {
                local_id: String::from("udp-duplicate"),
            })
            .expect("commit duplicate capability");

        let mut config = KernelVmConfig::new("vm-duplicate-process-capability");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register execution driver");
        let handle = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn process");
        let mut process = ActiveProcess::new(
            handle.pid(),
            handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::WebAssembly,
            ActiveExecution::Binding(BindingExecution::default()),
        );
        let key = NativeCapabilityKey::UdpSocket(String::from("same-key"));
        process
            .track_capability(key.clone(), first)
            .expect("track first lease");
        let error = process
            .track_capability(key.clone(), duplicate)
            .expect_err("duplicate key must be rejected");
        assert!(error
            .to_string()
            .contains("ERR_AGENTOS_CAPABILITY_DUPLICATE"));
        assert_eq!(
            process
                .capability_leases
                .get(&key)
                .expect("original lease remains")
                .id(),
            first_id
        );
        assert_eq!(capabilities.outstanding_len(), 1);

        process.capability_leases.clear();
        assert!(resources.is_zero());
        process.kernel_handle.finish(0);
        kernel.waitpid(process.kernel_pid).expect("reap process");
    }

    #[test]
    fn socket_description_retains_original_capability_until_final_alias_drops() {
        let resources = Arc::new(ResourceLedger::root(
            "vm=socket-description-lease",
            [
                (
                    ResourceClass::Capabilities,
                    ResourceLimit::new(1, "limits.reactor.maxCapabilities"),
                ),
                (
                    ResourceClass::ReadyHandles,
                    ResourceLimit::new(1, "limits.reactor.maxReadyHandles"),
                ),
                (
                    ResourceClass::Sockets,
                    ResourceLimit::new(1, "limits.resources.maxSockets"),
                ),
            ],
        ));
        let capabilities = CapabilityRegistry::new(11, Arc::clone(&resources));
        let lease = Arc::new(
            capabilities
                .reserve(CapabilityKind::UdpSocket)
                .expect("reserve capability")
                .commit(CapabilityBackend::Native {
                    local_id: String::from("shared-udp-description"),
                })
                .expect("commit capability"),
        );
        let description = Arc::new(SocketDescriptionLease::default());
        description.retain(Arc::clone(&lease));
        let alias = Arc::clone(&description);

        drop(lease);
        drop(description);
        assert_eq!(capabilities.outstanding_len(), 1);
        assert!(!resources.is_zero());

        drop(alias);
        assert_eq!(capabilities.outstanding_len(), 0);
        assert!(resources.is_zero());
    }

    #[test]
    fn accepted_connection_retires_from_listener_after_final_alias() {
        let connections = Arc::new(Mutex::new(BTreeSet::from([String::from("tcp-accepted-1")])));
        let retirement =
            ListenerConnectionRetirement::new(&connections, String::from("tcp-accepted-1"));
        let alias = Arc::clone(&retirement);

        drop(retirement);
        assert!(connections
            .lock()
            .expect("listener connections")
            .contains("tcp-accepted-1"));

        drop(alias);
        assert!(connections.lock().expect("listener connections").is_empty());
    }
}

impl BindingExecution {
    pub(crate) fn with_vm_pending_event_bytes_budget(
        mut self,
        budget: Arc<VmPendingByteBudget>,
    ) -> Self {
        debug_assert_eq!(self.pending_event_bytes.load(Ordering::Acquire), 0);
        debug_assert!(self
            .pending_events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty());
        self.vm_pending_event_bytes_budget = budget;
        self
    }
}

impl Drop for BindingExecution {
    fn drop(&mut self) {
        // Stop a background callback producer before reclaiming the queue. The
        // producer checks this flag while holding the same queue lock, so it
        // cannot enqueue after the retained-byte total is released here.
        self.cancelled.store(true, Ordering::Release);
        let mut pending_events = self
            .pending_events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending_events.clear();
        let pending_bytes = self.pending_event_bytes.swap(0, Ordering::AcqRel);
        self.vm_pending_event_bytes_budget.release(pending_bytes);
    }
}

pub(super) fn add_host_net_description_counts(
    descriptions: &BTreeMap<usize, bool>,
    counts: &mut NetworkResourceCounts,
) {
    counts.sockets += descriptions.len();
    counts.connections += descriptions
        .values()
        .filter(|connected| **connected)
        .count();
}

pub(super) fn add_live_host_net_transfer_descriptions(
    registry: &HostNetTransferDescriptionRegistry,
    descriptions: &mut BTreeMap<usize, bool>,
) {
    let mut transfers = registry.lock().unwrap_or_else(|error| error.into_inner());
    transfers.retain(|description_id, transfer| {
        let alive = transfer.handles.upgrade().is_some();
        if alive {
            descriptions
                .entry(*description_id)
                .and_modify(|connected| *connected |= transfer.connected)
                .or_insert(transfer.connected);
        }
        alive
    });
}

pub(super) fn process_network_resource_counts_with_transfers(
    kernel: &SidecarKernel,
    process: &ActiveProcess,
    registry: &HostNetTransferDescriptionRegistry,
) -> NetworkResourceCounts {
    let snapshot = kernel.resource_snapshot();
    let mut counts = NetworkResourceCounts {
        sockets: snapshot.sockets,
        connections: snapshot.socket_connections,
    };
    let mut descriptions = BTreeMap::new();
    process.collect_network_resource_counts(true, &mut descriptions, &mut counts);
    add_live_host_net_transfer_descriptions(registry, &mut descriptions);
    add_host_net_description_counts(&descriptions, &mut counts);
    counts
}

pub(super) fn rebind_process_runtime_event_targets(
    process: &mut ActiveProcess,
    kernel_readiness: &KernelSocketReadinessRegistry,
) {
    let session = process.execution.javascript_v8_session_handle();

    for (socket_id, socket) in &process.tcp_sockets {
        let key = NativeCapabilityKey::TcpSocket(socket_id.clone());
        let identity = process.capability_readiness_identity(&key);
        socket.set_event_pusher(session.clone(), identity);
        register_kernel_readiness_target(
            kernel_readiness,
            socket.kernel_socket_id,
            session.clone(),
            Some(Arc::clone(&socket.read_event_notify)),
            identity,
            socket_id.clone(),
            KernelSocketReadinessEvent::Data,
        );
    }
    for (socket_id, socket) in &process.unix_sockets {
        let key = NativeCapabilityKey::UnixSocket(socket_id.clone());
        socket.set_event_pusher(session.clone(), process.capability_readiness_identity(&key));
    }
    for (listener_id, listener) in &process.tcp_listeners {
        let key = NativeCapabilityKey::TcpListener(listener_id.clone());
        register_kernel_readiness_target(
            kernel_readiness,
            listener.kernel_socket_id,
            session.clone(),
            None,
            process.capability_readiness_identity(&key),
            listener_id.clone(),
            KernelSocketReadinessEvent::Accept,
        );
    }
    for (listener_id, listener) in &process.unix_listeners {
        let key = NativeCapabilityKey::UnixListener(listener_id.clone());
        listener.set_event_pusher(session.clone(), process.capability_readiness_identity(&key));
    }
    for (socket_id, socket) in &process.udp_sockets {
        let key = NativeCapabilityKey::UdpSocket(socket_id.clone());
        let identity = process.capability_readiness_identity(&key);
        socket.set_event_pusher(session.clone(), identity);
        register_kernel_readiness_target(
            kernel_readiness,
            socket.kernel_socket_id,
            session.clone(),
            Some(Arc::clone(&socket.read_event_notify)),
            identity,
            socket_id.clone(),
            KernelSocketReadinessEvent::Datagram,
        );
    }
    if let Ok(mut http2) = process.http2.shared.lock() {
        http2.event_session = session;
    }
}

pub(super) fn discard_replaced_image_pending_events(process: &mut ActiveProcess) {
    // Bytes written before exec remain observable through the same pipe on
    // Linux. Retain output, but discard old-image RPCs, signal registrations,
    // and exit notifications that cannot apply to the replacement image.
    let previous_pending_bytes = process.pending_execution_event_bytes;
    process.pending_execution_events.retain(|event| {
        matches!(
            event,
            ActiveExecutionEvent::Stdout(_) | ActiveExecutionEvent::Stderr(_)
        )
    });
    process.pending_execution_event_bytes = process
        .pending_execution_events
        .iter()
        .map(ActiveExecutionEvent::retained_bytes)
        .fold(0usize, usize::saturating_add);
    process
        .vm_pending_event_bytes_budget
        .release(previous_pending_bytes.saturating_sub(process.pending_execution_event_bytes));
    process
        .pending_execution_event_count_gauge
        .observe_depth(process.pending_execution_events.len());
    process
        .pending_execution_event_bytes_gauge
        .observe_depth(process.pending_execution_event_bytes);
}

impl ActiveExecutionEvent {
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::Stdout(bytes) | Self::Stderr(bytes) => {
                std::mem::size_of::<Self>().saturating_add(bytes.len())
            }
            // Internal RPC events are serviced eagerly rather than retained;
            // account a conservative fixed envelope if briefly deferred. The
            // wire payload is independently frame-bounded.
            Self::JavascriptSyncRpcRequest(_)
            | Self::JavascriptSyncRpcCompletion(_)
            | Self::PythonVfsRpcRequest(_)
            | Self::PythonSocketConnectCompletion(_) => 4 * 1024,
            Self::SignalState { .. } | Self::Exited(_) => std::mem::size_of::<Self>(),
        }
    }
}

impl ProcessEventEnvelope {
    pub(crate) fn retained_bytes(&self) -> usize {
        self.connection_id
            .len()
            .saturating_add(self.session_id.len())
            .saturating_add(self.vm_id.len())
            .saturating_add(self.process_id.len())
            .saturating_add(self.event.retained_bytes())
    }
}

fn poll_binding_process_event(
    execution: &BindingExecution,
) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
    poll_binding_process_event_leased(execution)
        .map(|event| event.map(PolledExecutionEvent::into_event))
}

fn poll_binding_process_event_leased(
    execution: &BindingExecution,
) -> Result<Option<PolledExecutionEvent>, SidecarError> {
    let event = execution
        .pending_events
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .pop_front();
    if let Some(event) = event {
        let event_bytes = event.retained_bytes();
        execution
            .pending_event_bytes
            .fetch_sub(event_bytes, Ordering::AcqRel);
        return Ok(Some(PolledExecutionEvent {
            event,
            reservation: Some(PendingExecutionEventReservation {
                budget: Arc::clone(&execution.vm_pending_event_bytes_budget),
                bytes: event_bytes,
            }),
        }));
    }
    if let Some(reason) = execution
        .event_overflow_reason
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return Err(SidecarError::InvalidState(reason));
    }
    Ok(None)
}

pub(super) fn descendant_pending_execution_event_capacity(
    root: &ActiveProcess,
    child_path: &[&str],
) -> Option<usize> {
    let mut child = root;
    for child_process_id in child_path {
        child = child.child_processes.get(*child_process_id)?;
    }
    Some(
        child
            .pending_execution_event_count_limit
            .saturating_sub(child.pending_execution_events.len()),
    )
}

pub(super) fn poll_child_execution_after_exit(
    child: &mut ActiveProcess,
) -> Result<Option<PolledExecutionEvent>, SidecarError> {
    match child.try_poll_execution_event() {
        Ok(event) => Ok(event),
        Err(SidecarError::Execution(message))
            if child.runtime == GuestRuntimeKind::WebAssembly
                && message == WasmExecutionError::EventChannelClosed.to_string() =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

impl ActiveExecution {
    pub(crate) fn is_prepared_for_start(&self) -> bool {
        match self {
            Self::Javascript(execution) => execution.is_prepared_for_start(),
            Self::Python(execution) => execution.is_prepared_for_start(),
            Self::Wasm(execution) => execution.is_prepared_for_start(),
            Self::Binding(_) => false,
        }
    }

    pub(crate) fn start_prepared(&mut self) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .start_prepared()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Python(execution) => execution
                .start_prepared()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .start_prepared()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Binding(_) => Err(SidecarError::InvalidState(String::from(
                "binding execution cannot be a prepared execve image",
            ))),
        }
    }

    pub(crate) fn python_vfs_rpc_responder(&self) -> Result<PythonVfsRpcResponder, SidecarError> {
        match self {
            Self::Python(execution) => Ok(execution.vfs_rpc_responder()),
            _ => Err(SidecarError::InvalidState(String::from(
                "only Python executions expose a Python VFS RPC responder",
            ))),
        }
    }

    pub(crate) fn claim_javascript_sync_rpc_response(
        &mut self,
        id: u64,
    ) -> Result<bool, SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .claim_sync_rpc_response(id)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Python(execution) => execution
                .claim_javascript_sync_rpc_response(id)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .claim_sync_rpc_response(id)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Binding(_) => Err(SidecarError::InvalidState(String::from(
                "binding executions cannot claim JavaScript sync RPC responses",
            ))),
        }
    }

    pub(crate) fn respond_claimed_javascript_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .respond_claimed_sync_rpc_success(id, result)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Python(execution) => execution
                .respond_claimed_javascript_sync_rpc_success(id, result)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .respond_claimed_sync_rpc_success(id, result)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Binding(_) => Err(SidecarError::InvalidState(String::from(
                "binding executions cannot service claimed JavaScript sync RPC responses",
            ))),
        }
    }

    pub(crate) fn respond_claimed_javascript_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), SidecarError> {
        let code = code.into();
        let message = message.into();
        match self {
            Self::Javascript(execution) => execution
                .respond_claimed_sync_rpc_error(id, code, message)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Python(execution) => execution
                .respond_claimed_javascript_sync_rpc_error(id, code, message)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .respond_claimed_sync_rpc_error(id, code, message)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Binding(_) => Err(SidecarError::InvalidState(String::from(
                "binding executions cannot service claimed JavaScript sync RPC errors",
            ))),
        }
    }

    pub(crate) fn uses_shared_v8_runtime(&self) -> bool {
        match self {
            Self::Javascript(execution) => execution.uses_shared_v8_runtime(),
            Self::Python(execution) => execution.uses_shared_v8_runtime(),
            Self::Wasm(execution) => execution.uses_shared_v8_runtime(),
            Self::Binding(_) => false,
        }
    }

    pub(crate) fn has_exited(&self) -> bool {
        matches!(self, Self::Javascript(execution) if execution.has_exited())
    }

    pub(crate) fn execute_retained_language(
        &mut self,
        language: RetainedExecutionLanguage,
        source: String,
        file_path: String,
        module: bool,
    ) -> Result<(), SidecarError> {
        match (self, language) {
            (Self::Javascript(execution), RetainedExecutionLanguage::JavaScript) => execution
                .execute_retained(source, file_path, module)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            (Self::Python(execution), RetainedExecutionLanguage::Python) => execution
                .execute_retained(source)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            _ => Err(SidecarError::InvalidState(String::from(
                "retained language does not match the resident executor",
            ))),
        }
    }

    pub(crate) fn child_pid(&self) -> u32 {
        match self {
            Self::Javascript(execution) => execution.child_pid(),
            Self::Python(execution) => execution.child_pid(),
            Self::Wasm(execution) => execution.child_pid(),
            Self::Binding(_) => 0,
        }
    }

    pub(crate) fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .write_stdin(chunk)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            // Sidecar Python and WASM read fd 0 from the sidecar kernel pipe.
            // Their in-process stdin bridges are bypassed in this mode, so
            // duplicating input into those bridges only fills an unread buffer.
            Self::Python(_) | Self::Wasm(_) => Ok(()),
            Self::Binding(_) => Ok(()),
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
            Self::Binding(_) => Ok(()),
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
            Self::Wasm(execution) => execution
                .send_stream_event(event_type, payload)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            _ => Err(SidecarError::InvalidState(String::from(
                "only embedded V8 executions can receive JavaScript stream events",
            ))),
        }
    }

    pub(crate) fn javascript_v8_session_handle(&self) -> Option<V8SessionHandle> {
        match self {
            Self::Javascript(execution) => Some(execution.v8_session_handle()),
            Self::Wasm(execution) => Some(execution.v8_session_handle()),
            _ => None,
        }
    }

    pub(crate) fn terminate(&mut self) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .terminate()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Python(execution) => execution
                .kill()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .terminate()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Binding(_) => Ok(()),
        }
    }

    pub(crate) fn pause(&self) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .pause()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Python(execution) => execution
                .pause()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .pause()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Binding(_) => Ok(()),
        }
    }

    pub(crate) fn resume(&self) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .resume()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Python(execution) => execution
                .resume()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .resume()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Binding(_) => Ok(()),
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
            Self::Python(execution) => execution
                .respond_javascript_sync_rpc_success(id, result)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .respond_sync_rpc_success(id, result)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            _ => Err(SidecarError::InvalidState(String::from(
                "only JavaScript, Python, and WebAssembly executions can service JavaScript sync RPC responses",
            ))),
        }
    }

    pub(crate) fn respond_javascript_sync_rpc_raw_success(
        &mut self,
        id: u64,
        payload: Vec<u8>,
    ) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .respond_sync_rpc_raw_success(id, payload)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .respond_sync_rpc_raw_success(id, payload)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            _ => Err(SidecarError::InvalidState(String::from(
                "only embedded V8 executions can service raw JavaScript sync RPC responses",
            ))),
        }
    }

    pub(crate) fn respond_javascript_sync_rpc_response(
        &mut self,
        id: u64,
        response: JavascriptSyncRpcServiceResponse,
    ) -> Result<(), SidecarError> {
        match response {
            JavascriptSyncRpcServiceResponse::Json(result) => {
                self.respond_javascript_sync_rpc_success(id, result)
            }
            JavascriptSyncRpcServiceResponse::Raw(payload) => {
                self.respond_javascript_sync_rpc_raw_success(id, payload)
            }
            JavascriptSyncRpcServiceResponse::Deferred { .. } => Err(SidecarError::InvalidState(
                String::from("deferred response must be awaited by the sidecar dispatcher"),
            )),
            JavascriptSyncRpcServiceResponse::SourceBackedJson {
                value,
                source_reservations,
            } => {
                let result = self.respond_javascript_sync_rpc_success(id, value);
                drop(source_reservations);
                result
            }
            JavascriptSyncRpcServiceResponse::SourceBackedRaw {
                payload,
                source_reservations,
            } => {
                let result = self.respond_javascript_sync_rpc_raw_success(id, payload);
                drop(source_reservations);
                result
            }
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
            Self::Python(execution) => execution
                .respond_javascript_sync_rpc_error(id, code, message)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .respond_sync_rpc_error(id, code, message)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            _ => Err(SidecarError::InvalidState(String::from(
                "only JavaScript, Python, and WebAssembly executions can service JavaScript sync RPC responses",
            ))),
        }
    }

    pub(crate) async fn poll_event(
        &mut self,
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
                        PythonExecutionEvent::JavascriptSyncRpcRequest(request) => {
                            ActiveExecutionEvent::JavascriptSyncRpcRequest(request)
                        }
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
                        WasmExecutionEvent::SyncRpcRequest(request) => {
                            ActiveExecutionEvent::JavascriptSyncRpcRequest(request)
                        }
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
            Self::Binding(execution) => {
                let _ = timeout;
                poll_binding_process_event(execution)
            }
        }
    }

    /// Probe the runtime event queue once without parking the sidecar thread or
    /// registering a waker outside the coalesced process-event broker.
    pub(crate) fn try_poll_event(&mut self) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .try_poll_event()
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
                .try_poll_event()
                .map(|event| {
                    event.map(|event| match event {
                        PythonExecutionEvent::Stdout(chunk) => ActiveExecutionEvent::Stdout(chunk),
                        PythonExecutionEvent::Stderr(chunk) => ActiveExecutionEvent::Stderr(chunk),
                        PythonExecutionEvent::JavascriptSyncRpcRequest(request) => {
                            ActiveExecutionEvent::JavascriptSyncRpcRequest(request)
                        }
                        PythonExecutionEvent::VfsRpcRequest(request) => {
                            ActiveExecutionEvent::PythonVfsRpcRequest(request)
                        }
                        PythonExecutionEvent::Exited(code) => ActiveExecutionEvent::Exited(code),
                    })
                })
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .try_poll_event()
                .map(|event| {
                    event.map(|event| match event {
                        WasmExecutionEvent::Stdout(chunk) => ActiveExecutionEvent::Stdout(chunk),
                        WasmExecutionEvent::Stderr(chunk) => ActiveExecutionEvent::Stderr(chunk),
                        WasmExecutionEvent::SyncRpcRequest(request) => {
                            ActiveExecutionEvent::JavascriptSyncRpcRequest(request)
                        }
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
            Self::Binding(execution) => poll_binding_process_event(execution),
        }
    }
}

pub(super) fn find_socket_state_entry(
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
            if let Some(entry) =
                find_kernel_socket_state_entry(&vm.kernel, process_id, process, kind, request)?
            {
                return Ok(Some(entry));
            }

            match kind {
                SocketQueryKind::TcpListener => {
                    for server in process.http_servers.values() {
                        let local_addr = server.guest_local_addr;
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

                    for listener in process.tcp_listeners.values() {
                        if listener.kernel_socket_id.is_some() {
                            continue;
                        }
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
                        if socket.kernel_socket_id.is_some() {
                            continue;
                        }
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

pub(super) fn require_vm_inspection_permission<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    capability: &str,
    domain: &str,
    resource: &str,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let decision = bridge.static_permission_decision(vm_id, capability, domain, Some(resource));
    if decision.as_ref().is_some_and(|decision| decision.allow) {
        return Ok(());
    }

    let reason = decision
        .and_then(|decision| decision.reason)
        .unwrap_or_else(|| format!("{capability} permission required"));
    Err(SidecarError::Execution(format!(
        "EACCES: permission denied, {resource}: {reason}"
    )))
}

pub(super) fn socket_query_resource(
    kind: SocketQueryKind,
    request: &FindListenerRequest,
) -> String {
    if let Some(path) = request.path.as_deref() {
        return format!("unix://{path}");
    }

    let host = request.host.as_deref().unwrap_or("*");
    let port = request
        .port
        .map_or_else(|| String::from("*"), |port| port.to_string());
    match kind {
        SocketQueryKind::TcpListener => format!("tcp://{host}:{port}"),
        SocketQueryKind::UdpBound => format!("udp://{host}:{port}"),
    }
}

pub(super) fn snapshot_vm_processes(vm: &VmState) -> Vec<ProcessSnapshotEntry> {
    let process_table = vm.kernel.list_processes();
    snapshot_vm_processes_inner(vm, &process_table)
}

fn snapshot_vm_processes_inner(
    vm: &VmState,
    process_table: &BTreeMap<u32, agentos_kernel::process_table::ProcessInfo>,
) -> Vec<ProcessSnapshotEntry> {
    let mut entries = Vec::new();

    for (process_id, process) in &vm.active_processes {
        collect_process_snapshot_entries(process_id, process, process_table, &mut entries);
    }

    for exited in &vm.exited_process_snapshots {
        entries.push(exited.process.clone());
    }

    entries
}

pub(super) fn prune_exited_process_snapshots(vm: &mut VmState) {
    let cutoff = Instant::now() - EXITED_PROCESS_SNAPSHOT_RETENTION;
    while vm
        .exited_process_snapshots
        .front()
        .is_some_and(|snapshot| snapshot.captured_at < cutoff)
    {
        vm.exited_process_snapshots.pop_front();
    }
}

pub(super) fn build_process_snapshot_entry(
    process_id: &str,
    process: &ActiveProcess,
    info: &agentos_kernel::process_table::ProcessInfo,
    exit_code: Option<i32>,
) -> ProcessSnapshotEntry {
    wire_process_snapshot_entry_from_shared(process_snapshot_entry_from_kernel(
        process_id,
        info,
        process.guest_cwd.clone(),
        exit_code,
    ))
}

fn wire_process_snapshot_entry_from_shared(
    entry: SharedProcessSnapshotEntry,
) -> ProcessSnapshotEntry {
    ProcessSnapshotEntry {
        process_id: entry.process_id,
        pid: entry.pid,
        ppid: entry.ppid,
        pgid: entry.pgid,
        sid: entry.sid,
        driver: entry.driver,
        command: entry.command,
        args: entry.args,
        cwd: entry.cwd,
        status: match entry.status {
            SharedProcessSnapshotStatus::Running => ProcessSnapshotStatus::Running,
            SharedProcessSnapshotStatus::Stopped => ProcessSnapshotStatus::Stopped,
            SharedProcessSnapshotStatus::Exited => ProcessSnapshotStatus::Exited,
        },
        exit_code: entry.exit_code,
    }
}

fn collect_process_snapshot_entries(
    process_id: &str,
    process: &ActiveProcess,
    process_table: &BTreeMap<u32, agentos_kernel::process_table::ProcessInfo>,
    entries: &mut Vec<ProcessSnapshotEntry>,
) {
    if let Some(info) = process_table.get(&process.kernel_pid) {
        entries.push(build_process_snapshot_entry(
            process_id, process, info, None,
        ));
    }

    for (child_id, child) in &process.child_processes {
        let child_process_id = format!("{process_id}/{child_id}");
        collect_process_snapshot_entries(&child_process_id, child, process_table, entries);
    }
}

fn find_kernel_socket_state_entry(
    kernel: &SidecarKernel,
    process_id: &str,
    process: &ActiveProcess,
    kind: SocketQueryKind,
    request: &FindListenerRequest,
) -> Result<Option<SocketStateEntry>, SidecarError> {
    let entry = match kind {
        SocketQueryKind::TcpListener => process
            .tcp_listeners
            .values()
            .filter_map(|listener| listener.kernel_socket_id)
            .find_map(|socket_id| {
                kernel_socket_state_entry(kernel, process_id, socket_id, kind, request)
            }),
        SocketQueryKind::UdpBound => process
            .udp_sockets
            .values()
            .filter_map(|socket| socket.kernel_socket_id)
            .find_map(|socket_id| {
                kernel_socket_state_entry(kernel, process_id, socket_id, kind, request)
            }),
    };

    if entry.is_some() {
        return Ok(entry);
    }

    for child in process.child_processes.values() {
        if let Some(entry) =
            find_kernel_socket_state_entry(kernel, process_id, child, kind, request)?
        {
            return Ok(Some(entry));
        }
    }

    Ok(None)
}

fn kernel_socket_state_entry(
    kernel: &SidecarKernel,
    process_id: &str,
    socket_id: SocketId,
    kind: SocketQueryKind,
    request: &FindListenerRequest,
) -> Option<SocketStateEntry> {
    let record = kernel.socket_get(socket_id)?;
    let local_address = record.local_address()?;
    match kind {
        SocketQueryKind::TcpListener if record.state() == SocketState::Listening => {}
        SocketQueryKind::TcpListener => return None,
        SocketQueryKind::UdpBound => {}
    }

    if !socket_host_matches(request.host.as_deref(), local_address.host()) {
        return None;
    }
    if request
        .port
        .is_some_and(|port| local_address.port() != port)
    {
        return None;
    }

    Some(SocketStateEntry {
        process_id: process_id.to_owned(),
        host: Some(local_address.host().to_owned()),
        port: Some(local_address.port()),
        path: None,
    })
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

pub(super) fn is_unspecified_socket_host(host: &str) -> bool {
    host == "0.0.0.0" || host == "::"
}

pub(super) fn is_loopback_socket_host(host: &str) -> bool {
    host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost")
}

pub(crate) fn vm_network_resource_counts(vm: &VmState) -> NetworkResourceCounts {
    let snapshot = vm.kernel.resource_snapshot();
    let mut counts = NetworkResourceCounts {
        sockets: snapshot.sockets,
        connections: snapshot.socket_connections,
    };
    let mut descriptions = BTreeMap::new();
    for process in vm.active_processes.values() {
        process.collect_network_resource_counts(true, &mut descriptions, &mut counts);
    }
    add_live_host_net_transfer_descriptions(&vm.host_net_transfer_descriptions, &mut descriptions);
    add_host_net_description_counts(&descriptions, &mut counts);
    counts
}

pub(super) fn vm_spawn_host_net_resource_counts(vm: &VmState) -> NetworkResourceCounts {
    vm_network_resource_counts(vm)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_javascript_socket_port_state(
    kernel: &SidecarKernel,
    process_id: &str,
    process: &ActiveProcess,
    tcp_guest_to_host: &mut BTreeMap<(JavascriptSocketFamily, u16), u16>,
    http_loopback_targets: &mut BTreeMap<
        (JavascriptSocketFamily, u16),
        JavascriptHttpLoopbackTarget,
    >,
    udp_guest_to_host: &mut BTreeMap<(JavascriptSocketFamily, u16), u16>,
    udp_host_to_guest: &mut BTreeMap<(JavascriptSocketFamily, u16), u16>,
    used_tcp_ports: &mut BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
    used_udp_ports: &mut BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
) {
    for (family, port) in process.tcp_port_reservations.values() {
        used_tcp_ports.entry(*family).or_default().insert(*port);
    }

    let mut record_tcp_listener = |guest_addr: SocketAddr, host_port: u16| {
        let family = JavascriptSocketFamily::from_ip(guest_addr.ip());
        used_tcp_ports
            .entry(family)
            .or_default()
            .insert(guest_addr.port());
        // VM-local loopback connects should also resolve listeners bound to
        // unspecified guest addresses like 0.0.0.0/::.
        tcp_guest_to_host.insert((family, guest_addr.port()), host_port);
    };

    for listener in process.tcp_listeners.values() {
        let local_addr = listener
            .kernel_socket_id
            .and_then(|socket_id| kernel.socket_get(socket_id))
            .and_then(|record| record.local_address().cloned())
            .and_then(|address| resolve_tcp_bind_addr(address.host(), address.port()).ok())
            .unwrap_or_else(|| listener.guest_local_addr());
        record_tcp_listener(local_addr, local_addr.port());
    }

    for (server_id, server) in &process.http_servers {
        let host_port = match server.listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(_) => continue,
        };
        record_tcp_listener(server.guest_local_addr, host_port);
        let family = JavascriptSocketFamily::from_ip(server.guest_local_addr.ip());
        http_loopback_targets.insert(
            (family, server.guest_local_addr.port()),
            JavascriptHttpLoopbackTarget {
                process_id: process_id.to_owned(),
                server_id: *server_id,
            },
        );
    }

    if let Ok(http2) = process.http2.shared.lock() {
        for server in http2.servers.values() {
            record_tcp_listener(server.guest_local_addr, server.actual_local_addr.port());
        }
    }

    for socket in process.tcp_sockets.values() {
        let guest_addr = socket
            .kernel_socket_id
            .and_then(|socket_id| kernel.socket_get(socket_id))
            .and_then(|record| record.local_address().cloned())
            .and_then(|address| resolve_tcp_bind_addr(address.host(), address.port()).ok())
            .unwrap_or(socket.guest_local_addr);
        let family = JavascriptSocketFamily::from_ip(guest_addr.ip());
        used_tcp_ports
            .entry(family)
            .or_default()
            .insert(guest_addr.port());
    }

    for socket in process.udp_sockets.values() {
        let guest_addr = socket
            .kernel_socket_id
            .and_then(|socket_id| kernel.socket_get(socket_id))
            .and_then(|record| record.local_address().cloned())
            .and_then(|address| {
                resolve_udp_bind_addr(address.host(), address.port(), socket.family).ok()
            })
            .or_else(|| socket.local_addr());
        let Some(guest_addr) = guest_addr else {
            continue;
        };
        let family = JavascriptSocketFamily::from_ip(guest_addr.ip());
        used_udp_ports
            .entry(family)
            .or_default()
            .insert(guest_addr.port());
        if let Some(host_addr) = socket.native_local_addr {
            if is_loopback_ip(guest_addr.ip()) || guest_addr.ip().is_unspecified() {
                udp_guest_to_host.insert((family, guest_addr.port()), host_addr.port());
                udp_host_to_guest.insert((family, host_addr.port()), guest_addr.port());
            }
        } else if socket.kernel_socket_id.is_some()
            && (is_loopback_ip(guest_addr.ip()) || guest_addr.ip().is_unspecified())
        {
            udp_guest_to_host.insert((family, guest_addr.port()), guest_addr.port());
            udp_host_to_guest.insert((family, guest_addr.port()), guest_addr.port());
        }
    }

    for (child_process_id, child) in &process.child_processes {
        let child_id = format!("{process_id}/{child_process_id}");
        collect_javascript_socket_port_state(
            kernel,
            &child_id,
            child,
            tcp_guest_to_host,
            http_loopback_targets,
            udp_guest_to_host,
            udp_host_to_guest,
            used_tcp_ports,
            used_udp_ports,
        );
    }
}

pub(super) fn reserve_capability(
    registry: &CapabilityRegistry,
    kind: CapabilityKind,
) -> Result<PendingCapability, SidecarError> {
    registry.reserve(kind).map_err(SidecarError::from)
}

pub(super) fn commit_process_capability(
    process: &mut ActiveProcess,
    pending: PendingCapability,
    key: NativeCapabilityKey,
    local_id: String,
    kernel_socket_id: Option<SocketId>,
) -> Result<
    (
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::CapabilityGeneration,
    ),
    SidecarError,
> {
    let backend = kernel_socket_id.map_or(CapabilityBackend::Native { local_id }, |socket_id| {
        CapabilityBackend::Kernel { socket_id }
    });
    let lease = pending.commit(backend).map_err(SidecarError::from)?;
    let identity = (lease.id(), lease.generation());
    process.track_capability(key, lease)?;
    Ok(identity)
}

/// Unblock a guest thread parked in a deferred `__kernel_stdin_read` /
/// `__kernel_poll` sync RPC. Isolate termination cannot interrupt the native
/// bridge wait, so teardown must answer the parked RPC BEFORE dropping the
/// execution (drop joins the guest thread) or cleanup deadlocks against it.
pub(super) fn flush_parked_kernel_wait_rpc(process: &mut ActiveProcess) {
    let request = process
        .deferred_kernel_wait_rpc
        .as_ref()
        .map(|(request, _)| request.clone());
    process.clear_deferred_kernel_wait_rpc();
    if let Some(request) = request {
        let _ = process
            .execution
            .respond_javascript_sync_rpc_error(request.id, "EINTR", "process teardown")
            .or_else(ignore_stale_javascript_sync_rpc_response);
    }
}

pub(crate) fn terminate_child_process_tree(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    kernel_readiness: &KernelSocketReadinessRegistry,
    unix_address_registry: &GuestUnixAddressRegistry,
) {
    flush_parked_kernel_wait_rpc(process);
    let sqlite_database_ids = process.sqlite_databases.keys().copied().collect::<Vec<_>>();
    for database_id in sqlite_database_ids {
        if let Err(error) = close_sqlite_database(kernel, process, database_id, true) {
            eprintln!(
                "ERR_AGENTOS_SQLITE_CLOSE: pid={} database_id={database_id} error={error}",
                process.kernel_pid
            );
        }
    }
    process.sqlite_statements.clear();
    let http_servers = std::mem::take(&mut process.http_servers);
    for (server_id, server) in http_servers {
        server.closed.store(true, Ordering::Release);
        server.close_notify.notify_waiters();
        if let Err(error) = process.release_capability(&NativeCapabilityKey::HttpServer(server_id))
        {
            eprintln!("ERR_AGENTOS_CAPABILITY_RELEASE: {error}");
        }
    }
    process.pending_http_requests.clear();
    terminate_http2_process_state(&process.http2.shared);

    let listener_ids = process.tcp_listeners.keys().cloned().collect::<Vec<_>>();
    for listener_id in listener_ids {
        if let Some(listener) = process.tcp_listeners.remove(&listener_id) {
            if let Err(error) = release_tcp_listener_handle(
                process,
                &listener_id,
                listener,
                kernel,
                kernel_readiness,
            ) {
                eprintln!("ERR_AGENTOS_TCP_LISTENER_RELEASE: {error}");
            }
        }
    }

    let sockets = process.tcp_sockets.keys().cloned().collect::<Vec<_>>();
    for socket_id in sockets {
        if let Some(socket) = process.tcp_sockets.remove(&socket_id) {
            release_tcp_socket_handle(process, &socket_id, socket, kernel, kernel_readiness);
        }
    }

    let unix_listener_ids = process.unix_listeners.keys().cloned().collect::<Vec<_>>();
    for listener_id in unix_listener_ids {
        if let Some(listener) = process.unix_listeners.remove(&listener_id) {
            if let Err(error) = release_unix_listener_capability(process, &listener_id, &listener) {
                eprintln!("ERR_AGENTOS_CAPABILITY_RELEASE: {error}");
            }
            if listener.is_final_description_handle() {
                if let Err(error) = close_pending_guest_unix_connections(
                    unix_address_registry,
                    &listener.registry_binding_id,
                ) {
                    eprintln!("ERR_AGENTOS_UNIX_SOCKET_METADATA: {error}");
                }
                if let Err(error) =
                    release_guest_unix_binding(unix_address_registry, &listener.registry_binding_id)
                {
                    eprintln!("ERR_AGENTOS_UNIX_SOCKET_METADATA: {error}");
                }
                if let Err(error) =
                    purge_guest_unix_target(unix_address_registry, &listener.registry_binding_id)
                {
                    eprintln!("ERR_AGENTOS_UNIX_SOCKET_METADATA: {error}");
                }
                drop(listener.close());
            }
        }
    }

    let unix_sockets = process.unix_sockets.keys().cloned().collect::<Vec<_>>();
    for socket_id in unix_sockets {
        if let Some(socket) = process.unix_sockets.remove(&socket_id) {
            release_unix_socket_handle(process, &socket_id, socket, unix_address_registry);
        }
    }

    let udp_socket_ids = process.udp_sockets.keys().cloned().collect::<Vec<_>>();
    for socket_id in udp_socket_ids {
        if let Some(socket) = process.udp_sockets.remove(&socket_id) {
            if let Err(error) =
                release_udp_socket_handle(process, &socket_id, socket, kernel, kernel_readiness)
            {
                eprintln!("ERR_AGENTOS_UDP_SOCKET_RELEASE: {error}");
            }
        }
    }

    // Python handles are adapter references to the TCP/UDP capabilities closed
    // above, not independent descriptors or leases. Dropping them also releases
    // any charged partial-read view still retained by the adapter.
    process.python_sockets.clear();

    let child_ids = process.child_processes.keys().cloned().collect::<Vec<_>>();
    for child_id in child_ids {
        let Some(mut child) = process.child_processes.remove(&child_id) else {
            continue;
        };
        terminate_child_process_tree(kernel, &mut child, kernel_readiness, unix_address_registry);
        let _ = kernel.kill_process(EXECUTION_DRIVER_NAME, child.kernel_pid, SIGTERM);
        let _ = signal_runtime_process(child.execution.child_pid(), SIGTERM);
        child.kernel_handle.finish(0);
        let _ = kernel.wait_and_reap(child.kernel_pid);
    }
}
