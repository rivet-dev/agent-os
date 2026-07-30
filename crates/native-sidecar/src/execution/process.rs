use super::*;

static NEXT_SQLITE_HOST_NAMESPACE: AtomicU64 = AtomicU64::new(1);

pub(super) fn admit_one_slot_rpc(
    pending_call_id: Option<u64>,
    incoming_call_id: u64,
    slot_name: &'static str,
) -> Result<(), HostServiceError> {
    let Some(pending_call_id) = pending_call_id else {
        return Ok(());
    };
    if pending_call_id == incoming_call_id {
        return Ok(());
    }
    Err(HostServiceError::new(
        "EBUSY",
        format!(
            "{slot_name} already retains call {pending_call_id}; call {incoming_call_id} was not admitted"
        ),
    )
    .with_details(json!({
        "slotName": slot_name,
        "pendingCallId": pending_call_id,
        "incomingCallId": incoming_call_id,
    })))
}

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
    /// Attach the generation-bound kernel control receiver before any guest
    /// engine is started. Controls requested while the adapter is being
    /// constructed remain durable in this receiver and are applied before the
    /// process is published in the sidecar's active-process map.
    pub(crate) fn attach_runtime_control_before_start(
        kernel_handle: &KernelProcessHandle,
        event_notify: Arc<tokio::sync::Notify>,
    ) -> Result<agentos_kernel::process_runtime::RuntimeControlReceiver, SidecarError> {
        kernel_handle
            .attach_runtime_control(Arc::new(move || event_notify.notify_one()))
            .map_err(|error| SidecarError::host(error.code(), error.message()))
    }

    pub(crate) fn install_executable_image(
        &mut self,
        bytes: Vec<u8>,
        retained_bytes: Reservation,
    ) -> Result<u64, HostServiceError> {
        if self.executable_image.is_some() {
            return Err(HostServiceError::new(
                "EBUSY",
                "this process already owns an executable-image snapshot",
            ));
        }
        let handle = self.next_executable_image_handle;
        self.next_executable_image_handle = handle.checked_add(1).ok_or_else(|| {
            HostServiceError::new("EOVERFLOW", "executable-image handle space exhausted")
        })?;
        self.executable_image = Some(ActiveExecutableImage {
            handle,
            bytes,
            _retained_bytes: retained_bytes,
        });
        Ok(handle)
    }

    pub(crate) fn read_executable_image(
        &self,
        handle: u64,
        offset: u64,
        maximum: usize,
    ) -> Result<&[u8], HostServiceError> {
        let image = self.executable_image.as_ref().ok_or_else(|| {
            HostServiceError::new("EBADF", "no executable-image snapshot is open")
        })?;
        if image.handle != handle {
            return Err(HostServiceError::new(
                "ESTALE",
                "executable-image handle does not name the active snapshot",
            ));
        }
        let start = usize::try_from(offset)
            .map_err(|_| HostServiceError::new("EOVERFLOW", "exec image offset exceeds usize"))?;
        if start >= image.bytes.len() {
            return Ok(&[]);
        }
        let end = start.saturating_add(maximum).min(image.bytes.len());
        Ok(&image.bytes[start..end])
    }

    pub(crate) fn close_executable_image(&mut self, handle: u64) -> Result<(), HostServiceError> {
        let image = self.executable_image.as_ref().ok_or_else(|| {
            HostServiceError::new("EBADF", "no executable-image snapshot is open")
        })?;
        if image.handle != handle {
            return Err(HostServiceError::new(
                "ESTALE",
                "executable-image handle does not name the active snapshot",
            ));
        }
        self.executable_image = None;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn new(
        kernel_pid: u32,
        kernel_handle: KernelProcessHandle,
        runtime_context: agentos_runtime_tokio::RuntimeContext,
        limits: crate::limits::VmLimits,
        process_event_capacity: usize,
        runtime: GuestRuntimeKind,
        execution: ActiveExecution,
    ) -> Self {
        let process_event_notify = Arc::new(tokio::sync::Notify::new());
        let runtime_control = Self::attach_runtime_control_before_start(
            &kernel_handle,
            Arc::clone(&process_event_notify),
        )
        .expect("a kernel process must attach exactly one runtime control receiver");
        Self::new_with_attached_runtime_control(
            kernel_pid,
            kernel_handle,
            runtime_context,
            limits,
            process_event_capacity,
            runtime,
            execution,
            runtime_control,
            process_event_notify,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_attached_runtime_control(
        kernel_pid: u32,
        kernel_handle: KernelProcessHandle,
        runtime_context: agentos_runtime_tokio::RuntimeContext,
        limits: crate::limits::VmLimits,
        process_event_capacity: usize,
        runtime: GuestRuntimeKind,
        mut execution: ActiveExecution,
        runtime_control: agentos_kernel::process_runtime::RuntimeControlReceiver,
        process_event_notify: Arc<tokio::sync::Notify>,
    ) -> Self {
        assert_eq!(
            runtime_control.identity(),
            kernel_handle.runtime_identity(),
            "the attached runtime control receiver must match its kernel process"
        );
        let pending_event_count_limit =
            process_event_capacity.min(limits.process.pending_event_count);
        let pending_stdin_bytes_limit = limits.process.pending_stdin_bytes;
        let pending_event_bytes_limit = limits.process.pending_event_bytes;
        execution
            .configure_adapter_event_limits(pending_event_count_limit, pending_event_bytes_limit);
        // Binding producers lease retained-byte reservations from their own
        // queue before an event can be moved into the ActiveProcess queue.
        // Both queues must therefore start with the same budget identity; a
        // signal-state drain may temporarily lease stdout/exit and requeue it.
        let vm_pending_event_bytes_budget =
            execution.adapter_event_bytes_budget().unwrap_or_else(|| {
                VmPendingByteBudget::new(
                    pending_event_bytes_limit,
                    queue_tracker::TrackedLimit::PendingExecutionEventBytes,
                )
            });
        let common_event_notify = Arc::new(Mutex::new(Arc::clone(&process_event_notify)));
        let common_event_wake = Arc::clone(&common_event_notify);
        let identity = kernel_handle.runtime_identity();
        let (event_submission, common_execution_events) = bounded_execution_event_channel(
            HostProcessContext {
                generation: identity.generation,
                pid: identity.pid,
            },
            pending_event_count_limit,
            PayloadLimit::new(
                "limits.process.pendingEventBytes",
                pending_event_bytes_limit,
            )
            .expect("an admitted process must have a nonzero common event byte limit"),
            Arc::new(move || {
                let notify = match common_event_wake.lock() {
                    Ok(notify) => Arc::clone(&notify),
                    Err(poisoned) => {
                        eprintln!(
                            "ERR_AGENTOS_EXECUTION_WAKE_LOCK_POISONED: recovering the execution wake target after a prior panic"
                        );
                        Arc::clone(&poisoned.into_inner())
                    }
                };
                notify.notify_one();
            }),
        )
        .expect("an admitted process must have a nonzero common event capacity");
        let host_capabilities = ProcessHostCapabilitySet::from_event_submission(event_submission);
        ExecutionBackend::configure_host_services(&mut execution, host_capabilities.clone());
        let control_notify = Arc::clone(&process_event_notify);
        runtime_control.set_wake(Arc::new(move || control_notify.notify_one()));
        let standalone_wasm_backend = execution.standalone_wasm_backend();
        Self {
            kernel_pid,
            kernel_handle,
            runtime_control,
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
            standalone_wasm_backend,
            adapter_policy: ExecutionAdapterPolicy::BINDING,
            detached: false,
            execution,
            guest_cwd: String::from("/"),
            env: BTreeMap::new(),
            host_cwd: PathBuf::from("/"),
            executable_image: None,
            next_executable_image_handle: 1,
            process_event_notify,
            common_event_notify,
            host_capabilities,
            common_execution_events,
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
            pending_net_connects: BTreeMap::new(),
            pending_managed_host_net_connects: BTreeMap::new(),
            pending_runtime_exit: None,
            exit_signal: None,
            exit_core_dumped: false,
            real_interval_timer: ActiveRealIntervalTimer::new(),
            child_processes: BTreeMap::new(),
            next_child_process_id: 0,
            pending_child_process_sync: BTreeMap::new(),
            child_process_bridge_owns_output: false,
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
            deferred_kernel_wait_task: None,
            deferred_kernel_wait_deadline_warned: false,
            deferred_child_write_timer: None,
            deferred_guest_wait: None,
            deferred_guest_wait_interrupted: false,
            guest_signal_checkpoint_pending: false,
            deferred_kernel_poll: None,
            deferred_kernel_read: None,
            module_resolution_cache: agentos_execution::LocalModuleResolutionCache::default(),
            module_resolution_cache_generation: None,
        }
    }

    pub(crate) fn clear_deferred_kernel_wait_rpc(&mut self) {
        self.deferred_kernel_wait_rpc = None;
        if let Some(task) = self.deferred_kernel_wait_task.take() {
            task.abort();
        }
        self.deferred_kernel_wait_deadline_warned = false;
        if let Some(timer) = self.deferred_child_write_timer.take() {
            timer.abort();
        }
    }

    pub(crate) fn clear_deferred_guest_wait(&mut self) -> Option<DeferredGuestWait> {
        self.deferred_guest_wait_interrupted = false;
        let mut wait = self.deferred_guest_wait.take()?;
        if let Some(task) = wait.wake_task.take() {
            task.abort();
        }
        Some(wait)
    }

    pub(crate) fn clear_deferred_kernel_poll(&mut self) -> Option<DeferredKernelPoll> {
        let mut poll = self.deferred_kernel_poll.take()?;
        if let Some(task) = poll.wake_task.take() {
            task.abort();
        }
        Some(poll)
    }

    pub(crate) fn clear_deferred_kernel_read(&mut self) -> Option<DeferredKernelRead> {
        let mut read = self.deferred_kernel_read.take()?;
        if let Some(task) = read.wake_task.take() {
            task.abort();
        }
        Some(read)
    }

    fn apply_backend_shutdown_outcome(
        &mut self,
        outcome: ShutdownOutcome,
    ) -> Result<(), SidecarError> {
        use agentos_kernel::process_runtime::ProcessExit;

        match outcome {
            ShutdownOutcome::AwaitExit => Ok(()),
            ShutdownOutcome::Exited(exit) => {
                self.pending_runtime_exit.get_or_insert(match exit {
                    ExecutionExit::Exited(code) => ProcessExit::Exited(code),
                    ExecutionExit::Signaled {
                        signal,
                        core_dumped,
                    } => ProcessExit::Signaled {
                        signal,
                        core_dumped,
                    },
                });
                Ok(())
            }
            ShutdownOutcome::ForwardSignal { process_id, signal } => {
                signal_runtime_process(process_id, signal)
            }
        }
    }

    pub(crate) fn apply_runtime_controls(&mut self) -> Result<(), SidecarError> {
        use agentos_kernel::process_runtime::{ProcessCancellationReason, ProcessTermination};

        let controls = self.runtime_control.pending();
        if controls.is_empty() {
            return Ok(());
        }

        let result = (|| {
            if let Some(stopped) = controls.stopped {
                if stopped {
                    self.execution.pause()?;
                } else {
                    self.execution.resume()?;
                }
            }

            let cancellation_shutdown_reason = controls.cancellation.map(|reason| match reason {
                ProcessCancellationReason::VmTeardown => ShutdownReason::VmTeardown,
                ProcessCancellationReason::Deadline => ShutdownReason::Deadline,
                ProcessCancellationReason::HostRequest => ShutdownReason::HostRequest,
                ProcessCancellationReason::RuntimeFault => ShutdownReason::RuntimeFault,
            });
            if let Some(termination) = controls.termination {
                let outcome = match termination {
                    ProcessTermination::Signal { signal, .. } => self
                        .execution
                        .begin_shutdown(ShutdownReason::Signal(signal))?,
                    ProcessTermination::RuntimeFault => self
                        .execution
                        .begin_shutdown(ShutdownReason::RuntimeFault)?,
                };
                self.apply_backend_shutdown_outcome(outcome)?;
            } else if let Some(reason) = cancellation_shutdown_reason {
                let outcome = self.execution.begin_shutdown(reason)?;
                self.apply_backend_shutdown_outcome(outcome)?;
            }

            if controls.checkpoint {
                // A combined ppoll owns its temporary mask. Select a signal
                // while that mask is still active, but atomically restore the
                // caller's mask before constructing the handler frame. This is
                // what lets a signal unblocked only by ppoll interrupt the
                // wait while nested handlers still observe the real mask.
                let temporary_mask = self.deferred_kernel_poll.as_mut().and_then(|poll| {
                    poll.temporary_signal_mask_token
                        .take()
                        .map(|token| (token, poll.temporary_signal_thread_id.take()))
                });
                let first_delivery = if let Some((token, thread_id)) = temporary_mask {
                    let result = if let Some(thread_id) = thread_id {
                        self.kernel_handle
                            .end_temporary_signal_mask_and_begin_signal_delivery_for_thread(
                                thread_id, token,
                            )
                    } else {
                        self.kernel_handle
                            .end_temporary_signal_mask_and_begin_signal_delivery(token)
                    };
                    match result {
                        Ok(delivery) => delivery,
                        Err(error) => {
                            let failure = HostServiceError::new(error.code(), error.to_string());
                            if let Some(poll) = self.clear_deferred_kernel_poll() {
                                poll.reply.fail(failure).map_err(SidecarError::from)?;
                            }
                            return Err(kernel_error(error));
                        }
                    }
                } else {
                    None
                };
                for index in 0..64 {
                    let delivery = if index == 0 {
                        match first_delivery {
                            Some(delivery) => Some(delivery),
                            None => self
                                .kernel_handle
                                .begin_signal_delivery()
                                .map_err(kernel_error)?,
                        }
                    } else {
                        self.kernel_handle
                            .begin_signal_delivery()
                            .map_err(kernel_error)?
                    };
                    let Some(delivery) = delivery else { break };
                    let identity = self.kernel_handle.runtime_identity();
                    let delivery_result = match self.execution.deliver_signal_checkpoint(
                        ExecutionWakeIdentity {
                            generation: identity.generation,
                            pid: identity.pid,
                        },
                        delivery.signal,
                        delivery.token,
                        delivery.action.flags,
                        delivery.thread_id,
                    ) {
                        Ok(SignalCheckpointOutcome::Published) => {
                            // Every caught signal must release a parked guest syscall so
                            // its handler can run promptly. The guest adapter applies
                            // SA_RESTART after dispatching the handler and transparently
                            // reissues only the documented restartable operations.
                            if self.deferred_guest_wait.is_some() {
                                self.deferred_guest_wait_interrupted = true;
                            }
                            self.guest_signal_checkpoint_pending = true;
                            let interrupted = HostServiceError::new(
                                "EINTR",
                                "caught signal interrupted the pending guest host call",
                            );
                            let mut interrupted_replies = Vec::new();
                            if let Some(wait) = self.clear_deferred_guest_wait() {
                                interrupted_replies.push(wait.reply);
                            }
                            if let Some(poll) = self.clear_deferred_kernel_poll() {
                                interrupted_replies.push(poll.reply);
                            }
                            if let Some(read) = self.clear_deferred_kernel_read() {
                                interrupted_replies.push(read.reply);
                            }
                            if let Some((request, _)) = self.deferred_kernel_wait_rpc.clone() {
                                self.clear_deferred_kernel_wait_rpc();
                                interrupted_replies.push(request.reply);
                            }
                            let mut settlement_error = None;
                            for reply in interrupted_replies {
                                if let Err(error) = reply.fail(interrupted.clone()) {
                                    if settlement_error.is_none() {
                                        settlement_error = Some(SidecarError::from(error));
                                    }
                                }
                            }
                            if let Some(error) = settlement_error {
                                return Err(error);
                            }
                            // The guest bridge owns exactly this one delivery
                            // until `signal_end`. Kernel delivery scopes are
                            // strict LIFO, so never preclaim a second token.
                            break;
                        }
                        Ok(SignalCheckpointOutcome::ForwardToProcess { process_id }) => {
                            signal_runtime_process(process_id, delivery.signal)
                        }
                        Ok(SignalCheckpointOutcome::Unsupported) => {
                            Err(SidecarError::InvalidState(format!(
                                "unsupported guest signal handler delivery for kernel pid {}",
                                self.kernel_pid
                            )))
                        }
                        Err(error) => Err(error),
                    };
                    let end_result = self
                        .kernel_handle
                        .end_signal_delivery(delivery.token)
                        .map_err(kernel_error);
                    delivery_result?;
                    end_result?;
                }
            }
            Ok(())
        })();

        match result {
            Ok(()) => match self.runtime_control.acknowledge(controls) {
                Ok(()) => Ok(()),
                Err(error) => {
                    self.runtime_control.retry_pending();
                    Err(SidecarError::host(error.code(), error.message()))
                }
            },
            Err(error) => {
                self.runtime_control.retry_pending();
                Err(error)
            }
        }
    }

    fn take_pending_runtime_exit_event(&mut self) -> Option<ActiveExecutionEvent> {
        use agentos_kernel::process_runtime::ProcessExit;

        self.pending_runtime_exit
            .take()
            .map(|termination| match termination {
                ProcessExit::Exited(code) => ActiveExecutionEvent::Exited(code),
                ProcessExit::Signaled {
                    signal,
                    core_dumped,
                } => {
                    self.exit_signal = Some(signal);
                    self.exit_core_dumped = core_dumped;
                    ActiveExecutionEvent::Exited(termination.shell_status())
                }
            })
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
            let limit = self.pending_execution_event_count_limit;
            return Err((
                SidecarError::host_resource_limit(
                    "limits.process.pendingEventCount/runtime.protocol.maxProcessEvents",
                    limit,
                    self.pending_execution_events.len().saturating_add(1),
                    format!(
                        "process execution event queue exceeded {limit} events (limits.process.pendingEventCount/runtime.protocol.maxProcessEvents); raise the limiting setting"
                    ),
                ),
                event,
            ));
        }
        let observed_process_bytes = self
            .pending_execution_event_bytes
            .saturating_add(event_bytes);
        if observed_process_bytes > self.pending_execution_event_bytes_limit {
            let limit = self.pending_execution_event_bytes_limit;
            return Err((
                SidecarError::host_resource_limit(
                    "limits.process.pendingEventBytes",
                    limit,
                    observed_process_bytes,
                    format!(
                        "process execution event queue exceeded {limit} retained bytes (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes"
                    ),
                ),
                event,
            ));
        }
        if !self.vm_pending_event_bytes_budget.try_reserve(event_bytes) {
            let limit = self.vm_pending_event_bytes_budget.limit();
            let observed = self
                .vm_pending_event_bytes_budget
                .used()
                .saturating_add(event_bytes);
            return Err((
                SidecarError::host_resource_limit(
                    "limits.process.pendingEventBytes",
                    limit,
                    observed,
                    format!(
                        "VM process execution event queues exceeded {limit} retained bytes (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes"
                    ),
                ),
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
        // A parent queue may have backpressured an already-accounted child
        // output event. Consuming one parent event creates capacity, so rearm
        // the coalesced process pump when descendants exist.
        if !self.child_processes.is_empty() {
            self.process_event_notify.notify_one();
        }
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
        self.queue_polled_execution_event(polled, true, true)
    }

    pub(super) fn queue_pending_polled_execution_event(
        &mut self,
        polled: PolledExecutionEvent,
    ) -> Result<(), SidecarError> {
        self.queue_polled_execution_event(polled, false, true)
    }

    #[allow(clippy::result_large_err)]
    pub(super) fn try_queue_pending_polled_execution_event(
        &mut self,
        polled: PolledExecutionEvent,
    ) -> Result<(), (SidecarError, PolledExecutionEvent)> {
        self.try_queue_polled_execution_event(polled, false, true)
    }

    /// Return a public child event to the pull owner's durable queue without
    /// waking the global pump that deliberately declined to consume it. The
    /// parent's next `child_process.poll` HostCall supplies the next broker
    /// edge; self-notifying here would spin on the same event indefinitely.
    pub(super) fn queue_pull_owned_polled_execution_event(
        &mut self,
        polled: PolledExecutionEvent,
    ) -> Result<(), SidecarError> {
        self.queue_polled_execution_event(polled, false, false)
    }

    pub(super) fn check_pending_polled_execution_event_admission(
        &self,
        polled: &PolledExecutionEvent,
    ) -> Result<(), SidecarError> {
        let event_bytes = polled.event.retained_bytes();
        if self.pending_execution_events.len() >= self.pending_execution_event_count_limit {
            let limit = self.pending_execution_event_count_limit;
            return Err(SidecarError::host_resource_limit(
                "limits.process.pendingEventCount/runtime.protocol.maxProcessEvents",
                limit,
                self.pending_execution_events.len().saturating_add(1),
                format!(
                    "process execution event queue exceeded {limit} events (limits.process.pendingEventCount/runtime.protocol.maxProcessEvents); raise the limiting setting"
                ),
            ));
        }
        let observed_process_bytes = self
            .pending_execution_event_bytes
            .saturating_add(event_bytes);
        if observed_process_bytes > self.pending_execution_event_bytes_limit {
            let limit = self.pending_execution_event_bytes_limit;
            return Err(SidecarError::host_resource_limit(
                "limits.process.pendingEventBytes",
                limit,
                observed_process_bytes,
                format!(
                    "process execution event queue exceeded {limit} retained bytes (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes"
                ),
            ));
        }
        if let Some(reservation) = polled.reservation.as_ref() {
            if reservation.bytes != event_bytes
                || !Arc::ptr_eq(&reservation.budget, &self.vm_pending_event_bytes_budget)
            {
                return Err(SidecarError::InvalidState(String::from(
                    "process execution event reservation no longer matches its VM queue; event requeue aborted",
                )));
            }
        } else if self
            .vm_pending_event_bytes_budget
            .used()
            .saturating_add(event_bytes)
            > self.vm_pending_event_bytes_budget.limit()
        {
            let limit = self.vm_pending_event_bytes_budget.limit();
            let observed = self
                .vm_pending_event_bytes_budget
                .used()
                .saturating_add(event_bytes);
            return Err(SidecarError::host_resource_limit(
                "limits.process.pendingEventBytes",
                limit,
                observed,
                format!(
                    "VM process execution event queues exceeded {limit} retained bytes (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes"
                ),
            ));
        }
        Ok(())
    }

    /// Collapse duplicate terminal events before ordering one authoritative
    /// exit behind already queued output/internal events. Some runtimes expose
    /// both their adapter exit and the kernel runtime-control exit; retaining
    /// both makes two exits endlessly rotate ahead of one another.
    pub(super) fn discard_pending_exit_events(&mut self) -> usize {
        let previous_len = self.pending_execution_events.len();
        let previous_bytes = self.pending_execution_event_bytes;
        self.pending_execution_events.retain(|event| {
            !matches!(
                event,
                ActiveExecutionEvent::Exited(_)
                    | ActiveExecutionEvent::Common(ExecutionEvent::Exited(_))
            )
        });
        self.pending_execution_event_bytes = self
            .pending_execution_events
            .iter()
            .map(ActiveExecutionEvent::retained_bytes)
            .fold(0usize, usize::saturating_add);
        self.vm_pending_event_bytes_budget
            .release(previous_bytes.saturating_sub(self.pending_execution_event_bytes));
        self.pending_execution_event_count_gauge
            .observe_depth(self.pending_execution_events.len());
        self.pending_execution_event_bytes_gauge
            .observe_depth(self.pending_execution_event_bytes);
        previous_len.saturating_sub(self.pending_execution_events.len())
    }

    fn queue_polled_execution_event(
        &mut self,
        polled: PolledExecutionEvent,
        front: bool,
        notify: bool,
    ) -> Result<(), SidecarError> {
        self.try_queue_polled_execution_event(polled, front, notify)
            .map_err(|(error, _polled)| error)
    }

    #[allow(clippy::result_large_err)]
    fn try_queue_polled_execution_event(
        &mut self,
        polled: PolledExecutionEvent,
        front: bool,
        notify: bool,
    ) -> Result<(), (SidecarError, PolledExecutionEvent)> {
        if let Err(error) = self.check_pending_polled_execution_event_admission(&polled) {
            return Err((error, polled));
        }
        let event_bytes = polled.event.retained_bytes();
        if polled.reservation.is_none()
            && !self.vm_pending_event_bytes_budget.try_reserve(event_bytes)
        {
            let limit = self.vm_pending_event_bytes_budget.limit();
            let observed = self
                .vm_pending_event_bytes_budget
                .used()
                .saturating_add(event_bytes);
            return Err((
                SidecarError::host_resource_limit(
                    "limits.process.pendingEventBytes",
                    limit,
                    observed,
                    format!(
                        "VM process execution event queues exceeded {limit} retained bytes (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes"
                    ),
                ),
                polled,
            ));
        }
        let PolledExecutionEvent { event, reservation } = polled;

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
        if notify {
            self.process_event_notify.notify_one();
        }
        Ok(())
    }

    pub(super) async fn poll_execution_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<PolledExecutionEvent>, SidecarError> {
        self.apply_runtime_controls()?;
        if let Some(event) = self.take_pending_runtime_exit_event() {
            return Ok(Some(PolledExecutionEvent::unreserved(event)));
        }
        if let Some(event) = self.execution.poll_adapter_event_leased() {
            return event;
        }
        if let Some(event) = self
            .common_execution_events
            .try_recv()
            .map_err(SidecarError::from)?
        {
            return Ok(Some(PolledExecutionEvent::unreserved(
                ActiveExecutionEvent::Common(event),
            )));
        }
        let host_capabilities = self.host_capabilities.clone();
        let event = self
            .execution
            .poll_event_with_host(
                self.kernel_handle.runtime_identity(),
                self.limits.reactor.max_bridge_response_bytes,
                timeout,
                &host_capabilities,
            )
            .await?;
        if let Some(event) = event {
            return Ok(Some(PolledExecutionEvent::unreserved(event)));
        }
        Ok(self
            .common_execution_events
            .try_recv()
            .map_err(SidecarError::from)?
            .map(ActiveExecutionEvent::Common)
            .map(PolledExecutionEvent::unreserved))
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn poll_execution_event_for_test(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        self.poll_execution_event(timeout)
            .await
            .map(|event| event.map(PolledExecutionEvent::into_event))
    }

    pub(super) fn try_poll_execution_event(
        &mut self,
    ) -> Result<Option<PolledExecutionEvent>, SidecarError> {
        self.apply_runtime_controls()?;
        if let Some(event) = self.take_pending_runtime_exit_event() {
            return Ok(Some(PolledExecutionEvent::unreserved(event)));
        }
        if let Some(event) = self.execution.poll_adapter_event_leased() {
            return event;
        }
        if let Some(event) = self
            .common_execution_events
            .try_recv()
            .map_err(SidecarError::from)?
        {
            return Ok(Some(PolledExecutionEvent::unreserved(
                ActiveExecutionEvent::Common(event),
            )));
        }
        let host_capabilities = self.host_capabilities.clone();
        let event = self.execution.try_poll_event_with_host(
            self.kernel_handle.runtime_identity(),
            self.limits.reactor.max_bridge_response_bytes,
            &host_capabilities,
        )?;
        if let Some(event) = event {
            return Ok(Some(PolledExecutionEvent::unreserved(event)));
        }
        Ok(self
            .common_execution_events
            .try_recv()
            .map_err(SidecarError::from)?
            .map(ActiveExecutionEvent::Common)
            .map(PolledExecutionEvent::unreserved))
    }

    pub(crate) fn with_process_event_limits(
        mut self,
        limits: &agentos_native_sidecar_core::limits::ProcessLimits,
    ) -> Self {
        self.pending_execution_event_count_limit =
            self.process_event_capacity.min(limits.pending_event_count);
        self.pending_execution_event_bytes_limit = limits.pending_event_bytes;
        self.execution.configure_adapter_event_limits(
            self.pending_execution_event_count_limit,
            limits.pending_event_bytes,
        );
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

    pub(crate) fn configure_current_execution_event_limits(&mut self) {
        self.execution.configure_adapter_event_limits(
            self.pending_execution_event_count_limit,
            self.pending_execution_event_bytes_limit,
        );
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
        self.execution.bind_adapter_event_bytes_budget(events);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_event_notify(mut self, event_notify: Arc<tokio::sync::Notify>) -> Self {
        let control_notify = Arc::clone(&event_notify);
        self.runtime_control
            .set_wake(Arc::new(move || control_notify.notify_one()));
        match self.common_event_notify.lock() {
            Ok(mut notify) => *notify = Arc::clone(&event_notify),
            Err(poisoned) => {
                eprintln!(
                    "ERR_AGENTOS_EXECUTION_WAKE_LOCK_POISONED: recovering the execution wake target after a prior panic"
                );
                *poisoned.into_inner() = Arc::clone(&event_notify);
            }
        }
        self.process_event_notify = event_notify;
        self
    }

    pub(crate) fn with_host_cwd(mut self, host_cwd: PathBuf) -> Self {
        self.host_cwd = host_cwd;
        self
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

    pub(crate) fn with_adapter_policy(mut self, adapter_policy: ExecutionAdapterPolicy) -> Self {
        self.adapter_policy = adapter_policy;
        self
    }

    pub(crate) fn with_standalone_wasm_backend(
        mut self,
        backend: ExecutionStandaloneWasmBackend,
    ) -> Self {
        self.standalone_wasm_backend = backend;
        self
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
        counts.sockets += self.http_servers.len();
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
        lease: agentos_runtime_tokio::capability::CapabilityLease,
    ) -> Result<(), SidecarError> {
        match self.capability_leases.entry(key.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Arc::new(lease));
                Ok(())
            }
            std::collections::btree_map::Entry::Occupied(_) => Err(SidecarError::host(
                "ERR_AGENTOS_CAPABILITY_DUPLICATE",
                format!("process already owns {key:?}"),
            )),
        }
    }

    pub(super) fn shared_capability_lease(
        &self,
        key: &NativeCapabilityKey,
    ) -> Option<Arc<agentos_runtime_tokio::capability::CapabilityLease>> {
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
            SidecarError::host(
                "ERR_AGENTOS_CAPABILITY_MISSING",
                format!("process does not own {key:?}"),
            )
        })?;
        if let Some(session) = self
            .execution
            .execution_wake_handle(self.kernel_handle.runtime_identity())
        {
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
        Err(SidecarError::host(
            "ERR_AGENTOS_CAPABILITY_MISSING",
            format!("process does not own {key:?} and the open description has no retained lease"),
        ))
    }

    pub(super) fn release_capability_if_present(&mut self, key: &NativeCapabilityKey) {
        if let Some(lease) = self.capability_leases.remove(key) {
            if let Some(session) = self
                .execution
                .execution_wake_handle(self.kernel_handle.runtime_identity())
            {
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
        agentos_runtime_tokio::capability::CapabilityId,
        agentos_runtime_tokio::capability::CapabilityGeneration,
    )> {
        self.capability_leases
            .get(key)
            .map(|lease| (lease.id(), lease.generation()))
    }

    pub(super) fn capability_fairness_identity(
        &self,
        key: &NativeCapabilityKey,
    ) -> Option<(
        agentos_runtime_tokio::capability::CapabilityId,
        agentos_runtime_tokio::capability::SessionGeneration,
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
            SidecarError::host(
                "ERR_AGENTOS_CAPABILITY_SESSION",
                String::from("process runtime is not VM-generation scoped"),
            )
        })?;
        let lease = self.capability_leases.get(key).ok_or_else(|| {
            SidecarError::host(
                "ERR_AGENTOS_CAPABILITY_MISSING",
                format!("process does not own {key:?}"),
            )
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
    use std::future::Future as _;
    use std::task::{Context, Poll, Waker};

    fn test_runtime_context() -> agentos_runtime_tokio::RuntimeContext {
        agentos_runtime_tokio::SidecarRuntime::process(
            &agentos_runtime_tokio::RuntimeConfig::default(),
        )
        .expect("create test runtime")
        .context()
    }

    fn take_notify_permit(notify: &tokio::sync::Notify) -> bool {
        let mut notified = Box::pin(notify.notified());
        let mut context = Context::from_waker(Waker::noop());
        matches!(notified.as_mut().poll(&mut context), Poll::Ready(()))
    }

    #[test]
    fn startup_signal_is_durable_across_backend_construction() {
        use agentos_kernel::process_runtime::ProcessExit;

        let mut config = KernelVmConfig::new("vm-startup-signal-endpoint");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register execution driver");
        let kernel_handle = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn kernel process");
        let pid = kernel_handle.pid();
        let notify = Arc::new(tokio::sync::Notify::new());
        let runtime_control =
            ActiveProcess::attach_runtime_control_before_start(&kernel_handle, Arc::clone(&notify))
                .expect("attach runtime endpoint before backend construction");

        kernel
            .kill_process(EXECUTION_DRIVER_NAME, pid, SIGTERM)
            .expect("signal process during backend construction");
        assert!(take_notify_permit(&notify));
        assert!(runtime_control.pending().termination.is_some());

        let mut process = ActiveProcess::new_with_attached_runtime_control(
            pid,
            kernel_handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            super::GuestRuntimeKind::WebAssembly,
            ActiveExecution::Binding(BindingExecution::default()),
            runtime_control,
            Arc::clone(&notify),
        );
        process
            .apply_runtime_controls()
            .expect("apply durable startup signal to constructed backend");
        assert_eq!(
            process.pending_runtime_exit,
            Some(ProcessExit::Signaled {
                signal: SIGTERM,
                core_dumped: false,
            })
        );

        process.kernel_handle.finish_signaled(SIGTERM, false);
        drop(process);
        kernel.waitpid(pid).expect("reap signaled startup process");
        assert!(kernel.list_processes().is_empty());
    }

    #[test]
    fn one_slot_rpc_admission_is_typed_and_never_overwrites() {
        admit_one_slot_rpc(None, 8, "deferredKernelWaitRpc").expect("empty slot");
        admit_one_slot_rpc(Some(8), 8, "deferredKernelWaitRpc").expect("same call recheck");
        let error = admit_one_slot_rpc(Some(7), 8, "deferredKernelWaitRpc")
            .expect_err("different call must not replace the retained waiter");
        assert_eq!(error.code, "EBUSY");
        assert_eq!(
            error.details.as_ref().expect("slot details")["pendingCallId"],
            7
        );
        assert_eq!(
            error.details.as_ref().expect("slot details")["incomingCallId"],
            8
        );
    }

    #[test]
    fn executable_image_snapshot_is_single_bounded_and_releases_accounting() {
        let mut config = KernelVmConfig::new("vm-executable-image-snapshot");
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
        let pid = handle.pid();
        let runtime = test_runtime_context();
        let resources = Arc::clone(runtime.resources());
        let baseline = resources.usage(ResourceClass::ExecutorBytes).used;
        let mut process = ActiveProcess::new(
            pid,
            handle,
            runtime,
            crate::limits::VmLimits::default(),
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::WebAssembly,
            ActiveExecution::Binding(BindingExecution::default()),
        );
        let retained = resources
            .reserve(ResourceClass::ExecutorBytes, 4)
            .expect("reserve first image");
        let image_handle = process
            .install_executable_image(vec![1, 2, 3, 4], retained)
            .expect("install first image");
        assert_eq!(
            resources.usage(ResourceClass::ExecutorBytes).used,
            baseline + 4
        );
        assert_eq!(
            process
                .read_executable_image(image_handle, 1, 2)
                .expect("bounded image read"),
            &[2, 3]
        );
        assert_eq!(
            process
                .read_executable_image(image_handle, 99, usize::MAX)
                .expect("read beyond EOF"),
            &[] as &[u8]
        );

        let duplicate = resources
            .reserve(ResourceClass::ExecutorBytes, 1)
            .expect("reserve duplicate attempt");
        let error = process
            .install_executable_image(vec![9], duplicate)
            .expect_err("a second image must not replace the live snapshot");
        assert_eq!(error.code, "EBUSY");
        assert_eq!(
            resources.usage(ResourceClass::ExecutorBytes).used,
            baseline + 4,
            "rejected image reservation must be released"
        );
        let error = process
            .close_executable_image(image_handle + 1)
            .expect_err("a stale handle must not close the live snapshot");
        assert_eq!(error.code, "ESTALE");
        assert_eq!(
            resources.usage(ResourceClass::ExecutorBytes).used,
            baseline + 4
        );

        process
            .close_executable_image(image_handle)
            .expect("close active image");
        assert_eq!(resources.usage(ResourceClass::ExecutorBytes).used, baseline);

        let retained = resources
            .reserve(ResourceClass::ExecutorBytes, 3)
            .expect("reserve teardown image");
        process
            .install_executable_image(vec![5, 6, 7], retained)
            .expect("install teardown image");
        process.kernel_handle.finish(0);
        drop(process);
        assert_eq!(
            resources.usage(ResourceClass::ExecutorBytes).used,
            baseline,
            "process teardown must release the retained image"
        );
        kernel.waitpid(pid).expect("reap process");
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
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
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
    fn leased_event_transfers_at_exact_vm_cap_without_loss_or_double_charge() {
        let event = ActiveExecutionEvent::Stdout(vec![0x51; 32]);
        let event_bytes = event.retained_bytes();
        let budget = VmPendingByteBudget::new(
            event_bytes,
            queue_tracker::TrackedLimit::PendingExecutionEventBytes,
        );
        let mut config = KernelVmConfig::new("vm-exact-cap-event-transfer");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register execution driver");
        let source_handle = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn source process");
        let target_handle = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn target process");
        let source_pid = source_handle.pid();
        let target_pid = target_handle.pid();
        let mut source = ActiveProcess::new(
            source_pid,
            source_handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
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
        let mut target = ActiveProcess::new(
            target_pid,
            target_handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
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

        source
            .queue_pending_execution_event(event)
            .expect("source event exactly fills the VM cap");
        let leased = source
            .lease_pending_execution_event()
            .expect("lease source event");
        target
            .try_queue_pending_polled_execution_event(leased)
            .expect("the existing reservation transfers at the exact cap");
        assert_eq!(
            budget.used(),
            event_bytes,
            "transfer must not double charge"
        );
        assert!(source.pending_execution_events.is_empty());

        let overflow = PolledExecutionEvent::unreserved(ActiveExecutionEvent::Exited(0));
        let (error, overflow) = target
            .try_queue_pending_polled_execution_event(overflow)
            .expect_err("one additional retained event must exceed the exact cap");
        assert_eq!(error.code(), Some("ERR_AGENTOS_RESOURCE_LIMIT"));
        assert!(matches!(overflow.event(), ActiveExecutionEvent::Exited(0)));
        assert_eq!(budget.used(), event_bytes, "rejection must not leak bytes");

        assert!(matches!(
            target.pop_pending_execution_event(),
            Some(ActiveExecutionEvent::Stdout(bytes)) if bytes == vec![0x51; 32]
        ));
        assert_eq!(
            budget.used(),
            0,
            "consumption must release the one reservation"
        );

        let backpressure_budget = VmPendingByteBudget::new(
            event_bytes.saturating_mul(2),
            queue_tracker::TrackedLimit::PendingExecutionEventBytes,
        );
        source = source.with_vm_pending_byte_budgets(
            VmPendingByteBudget::new(
                event_bytes.saturating_mul(2),
                queue_tracker::TrackedLimit::PendingKernelStdinBytes,
            ),
            Arc::clone(&backpressure_budget),
        );
        target = target.with_vm_pending_byte_budgets(
            VmPendingByteBudget::new(
                event_bytes.saturating_mul(2),
                queue_tracker::TrackedLimit::PendingKernelStdinBytes,
            ),
            Arc::clone(&backpressure_budget),
        );
        source.pending_execution_event_count_limit = 1;
        target.pending_execution_event_count_limit = 1;
        target
            .queue_pending_execution_event(ActiveExecutionEvent::Stdout(vec![0x41; 32]))
            .expect("fill the parent process queue");
        source
            .queue_pending_execution_event(ActiveExecutionEvent::Stdout(vec![0x42; 32]))
            .expect("fill the remaining VM aggregate bytes in the child");
        assert_eq!(backpressure_budget.used(), event_bytes * 2);
        target.child_processes.insert(String::from("child"), source);
        while take_notify_permit(&target.process_event_notify) {}

        let child_event = target
            .child_processes
            .get_mut("child")
            .unwrap()
            .lease_pending_execution_event()
            .expect("lease child output for parent transfer");
        let (error, child_event) = target
            .try_queue_pending_polled_execution_event(child_event)
            .expect_err("a full parent process queue must backpressure the transfer");
        assert_eq!(error.code(), Some("ERR_AGENTOS_RESOURCE_LIMIT"));
        target
            .child_processes
            .get_mut("child")
            .unwrap()
            .queue_pull_owned_polled_execution_event(child_event)
            .expect("return the same reservation to the child queue");
        assert_eq!(
            backpressure_budget.used(),
            event_bytes * 2,
            "backpressure and requeue must neither lose nor double-charge bytes"
        );

        assert!(matches!(
            target.pop_pending_execution_event(),
            Some(ActiveExecutionEvent::Stdout(bytes)) if bytes == vec![0x41; 32]
        ));
        assert!(
            take_notify_permit(&target.process_event_notify),
            "draining a parent with descendants must rearm the global pump"
        );
        let child_event = target
            .child_processes
            .get_mut("child")
            .unwrap()
            .lease_pending_execution_event()
            .expect("the pull-owned child event remains durable");
        target
            .try_queue_pending_polled_execution_event(child_event)
            .expect("the rearmed transfer succeeds after parent capacity is freed");
        assert_eq!(backpressure_budget.used(), event_bytes);
        assert!(matches!(
            target.pop_pending_execution_event(),
            Some(ActiveExecutionEvent::Stdout(bytes)) if bytes == vec![0x42; 32]
        ));
        assert_eq!(backpressure_budget.used(), 0);
        source = target
            .child_processes
            .remove("child")
            .expect("recover child for orderly teardown");

        source.kernel_handle.finish(0);
        target.kernel_handle.finish(0);
        kernel.waitpid(source_pid).expect("reap source process");
        kernel.waitpid(target_pid).expect("reap target process");
    }

    #[test]
    fn duplicate_exit_events_cannot_spin_ahead_of_trailing_output() {
        let mut config = KernelVmConfig::new("duplicate-child-exit-ordering");
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
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::WebAssembly,
            ActiveExecution::Binding(BindingExecution::default()),
        );

        process
            .queue_pending_execution_event(ActiveExecutionEvent::Stdout(b"late\n".to_vec()))
            .expect("queue trailing output");
        process
            .queue_pending_execution_event(ActiveExecutionEvent::Exited(0))
            .expect("queue adapter exit");
        process
            .queue_pending_execution_event(ActiveExecutionEvent::Common(ExecutionEvent::Exited(
                agentos_execution::backend::ExecutionExit::Exited(0),
            )))
            .expect("queue runtime-control exit");
        assert!(take_notify_permit(&process.process_event_notify));

        assert_eq!(process.discard_pending_exit_events(), 2);
        let trailing_output = process
            .lease_pending_execution_event()
            .expect("lease trailing output for its pull owner");
        process
            .queue_pull_owned_polled_execution_event(trailing_output)
            .expect("return pull-owned output without a global rearm");
        assert!(
            !take_notify_permit(&process.process_event_notify),
            "returning a pull-owned event must not self-rearm the global pump"
        );
        process
            .queue_pending_polled_execution_event(PolledExecutionEvent::unreserved(
                ActiveExecutionEvent::Exited(0),
            ))
            .expect("queue one authoritative exit behind output");
        assert!(take_notify_permit(&process.process_event_notify));

        assert!(matches!(
            process.pop_pending_execution_event(),
            Some(ActiveExecutionEvent::Stdout(bytes)) if bytes == b"late\n"
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
    fn synthetic_runtime_termination_preserves_exact_signal_until_event_emission() {
        let mut config = KernelVmConfig::new("exact-synthetic-runtime-exit");
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
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::JavaScript,
            ActiveExecution::Binding(BindingExecution::default()),
        );

        kernel
            .kill_process(EXECUTION_DRIVER_NAME, process.kernel_pid, SIGTERM)
            .expect("request SIGTERM");
        assert_eq!(process.exit_signal, None, "a request is not an exit report");

        let event = process
            .try_poll_execution_event()
            .expect("poll runtime control")
            .expect("synthetic terminal event")
            .into_event();
        assert!(matches!(event, ActiveExecutionEvent::Exited(143)));
        assert_eq!(process.exit_signal, Some(SIGTERM));
        assert!(!process.exit_core_dumped);

        process.kernel_handle.finish_signaled(SIGTERM, false);
        kernel.waitpid(process.kernel_pid).expect("reap process");
    }

    #[test]
    fn stop_and_continue_wait_state_follow_runtime_control_acknowledgement() {
        let mut config = KernelVmConfig::new("runtime-control-stop-ack");
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
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::JavaScript,
            ActiveExecution::Binding(BindingExecution::default()),
        );
        let ActiveExecution::Binding(binding) = &process.execution else {
            unreachable!("test process must retain binding execution");
        };
        let paused = Arc::clone(&binding.paused);
        let cancelled = Arc::clone(&binding.cancelled);
        let pending_events = Arc::clone(&binding.pending_events);
        let overflow_reason = Arc::clone(&binding.event_overflow_reason);
        let pending_bytes = Arc::clone(&binding.pending_event_bytes);
        let count_limit = Arc::clone(&binding.pending_event_count_limit);
        let bytes_limit = Arc::clone(&binding.pending_event_bytes_limit);
        let event_budget = Arc::clone(&binding.vm_pending_event_bytes_budget);

        kernel
            .kill_process(EXECUTION_DRIVER_NAME, process.kernel_pid, libc::SIGTSTP)
            .expect("request stop");
        assert_eq!(
            kernel
                .list_processes()
                .get(&process.kernel_pid)
                .expect("kernel process")
                .status,
            agentos_kernel::process_table::ProcessStatus::Running
        );
        process
            .apply_runtime_controls()
            .expect("apply and acknowledge stop");
        assert_eq!(
            kernel
                .list_processes()
                .get(&process.kernel_pid)
                .expect("kernel process")
                .status,
            agentos_kernel::process_table::ProcessStatus::Stopped
        );
        assert!(process.runtime_control.pending().is_empty());
        assert!(paused.load(Ordering::Acquire));
        assert!(send_binding_process_event(
            &cancelled,
            &pending_events,
            &overflow_reason,
            &pending_bytes,
            &count_limit,
            &bytes_limit,
            &event_budget,
            ActiveExecutionEvent::Stdout(b"paused-output".to_vec()),
        ));
        assert!(process
            .try_poll_execution_event()
            .expect("poll stopped binding")
            .is_none());

        kernel
            .kill_process(EXECUTION_DRIVER_NAME, process.kernel_pid, libc::SIGCONT)
            .expect("request continue");
        assert_eq!(
            kernel
                .list_processes()
                .get(&process.kernel_pid)
                .expect("kernel process")
                .status,
            agentos_kernel::process_table::ProcessStatus::Stopped
        );
        process
            .apply_runtime_controls()
            .expect("apply and acknowledge continue");
        assert_eq!(
            kernel
                .list_processes()
                .get(&process.kernel_pid)
                .expect("kernel process")
                .status,
            agentos_kernel::process_table::ProcessStatus::Running
        );
        assert!(process.runtime_control.pending().is_empty());
        assert!(!paused.load(Ordering::Acquire));
        let event = process
            .try_poll_execution_event()
            .expect("poll resumed binding")
            .expect("queued binding event after resume")
            .into_event();
        assert!(matches!(
            event,
            ActiveExecutionEvent::Stdout(bytes) if bytes == b"paused-output"
        ));

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
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
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
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
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
            .expect("binding pending-event queue")
            .is_empty());
        self.vm_pending_event_bytes_budget = budget;
        self
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_descendant_wait_ownership(
        mut self,
        ownership: DescendantWaitOwnership,
    ) -> Self {
        self.descendant_wait_ownership = ownership;
        self
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_descendant_output_ownership(
        mut self,
        ownership: DescendantOutputOwnership,
    ) -> Self {
        self.descendant_output_ownership = ownership;
        self
    }
}

impl Drop for BindingExecution {
    fn drop(&mut self) {
        // Stop a background callback producer before reclaiming the queue. The
        // producer checks this flag while holding the same queue lock, so it
        // cannot enqueue after the retained-byte total is released here.
        self.cancelled.store(true, Ordering::Release);
        self.pause_notify.notify_waiters();
        let mut pending_events = match self.pending_events.lock() {
            Ok(pending_events) => pending_events,
            Err(poisoned) => {
                eprintln!(
                    "ERR_AGENTOS_BINDING_EVENT_QUEUE_POISONED: recovering the binding event queue while releasing reservations"
                );
                poisoned.into_inner()
            }
        };
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
    let mut transfers = match registry.lock() {
        Ok(transfers) => transfers,
        Err(poisoned) => {
            eprintln!(
                "ERR_AGENTOS_HOST_NET_TRANSFER_REGISTRY_POISONED: recovering the transfer registry during resource accounting"
            );
            poisoned.into_inner()
        }
    };
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
    let session = process
        .execution
        .execution_wake_handle(process.kernel_handle.runtime_identity());

    for (socket_id, socket) in &process.tcp_sockets {
        let key = NativeCapabilityKey::TcpSocket(socket_id.clone());
        let identity = process.capability_readiness_identity(&key);
        socket.set_event_pusher(
            session.clone(),
            identity,
            Arc::clone(&process.process_event_notify),
        );
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
        socket.set_event_pusher(
            session.clone(),
            process.capability_readiness_identity(&key),
            Arc::clone(&process.process_event_notify),
        );
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
        listener.set_event_pusher(
            session.clone(),
            process.capability_readiness_identity(&key),
            Arc::clone(&process.process_event_notify),
        );
    }
    for (socket_id, socket) in &process.udp_sockets {
        let key = NativeCapabilityKey::UdpSocket(socket_id.clone());
        let identity = process.capability_readiness_identity(&key);
        socket.set_event_pusher(
            session.clone(),
            identity,
            Arc::clone(&process.process_event_notify),
        );
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
            Self::Common(ExecutionEvent::Output { bytes, .. }) => {
                std::mem::size_of::<Self>().saturating_add(bytes.len())
            }
            Self::Common(ExecutionEvent::HostCall { .. })
            | Self::Common(ExecutionEvent::Exited(_)) => 4 * 1024,
            Self::Common(ExecutionEvent::Warning(error))
            | Self::Common(ExecutionEvent::RuntimeFault(error)) => {
                std::mem::size_of::<Self>().saturating_add(error.encoded_bytes())
            }
            Self::Common(_) => 4 * 1024,
            Self::Stdout(bytes) | Self::Stderr(bytes) => {
                std::mem::size_of::<Self>().saturating_add(bytes.len())
            }
            // Internal RPC events are serviced eagerly rather than retained;
            // account a conservative fixed envelope if briefly deferred. The
            // wire payload is independently frame-bounded.
            Self::HostRpcRequest(_)
            | Self::HostCallCompletion(_)
            | Self::DeferredPosixPollWake
            | Self::ManagedStreamReadRecheck(_)
            | Self::ManagedUdpPollRecheck(_) => 4 * 1024,
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
    if execution.paused.load(Ordering::Acquire) && !execution.cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let event = execution
        .pending_events
        .lock()
        .map_err(|_| {
            SidecarError::host(
                "EIO",
                "ERR_AGENTOS_BINDING_EVENT_QUEUE_POISONED: binding event queue was poisoned by a prior panic",
            )
        })?
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
        .map_err(|_| {
            SidecarError::host(
                "EIO",
                "ERR_AGENTOS_BINDING_OVERFLOW_STATE_POISONED: binding overflow state was poisoned by a prior panic",
            )
        })?
        .clone()
    {
        return Err(SidecarError::Host(reason));
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
        Err(SidecarError::ExecutionEventChannelClosed { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

impl ExecutionBackend for BindingExecution {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Binding
    }

    fn descendant_wait_ownership(&self) -> DescendantWaitOwnership {
        self.descendant_wait_ownership
    }

    fn descendant_output_ownership(&self) -> DescendantOutputOwnership {
        self.descendant_output_ownership
    }

    fn configure_host_services(&mut self, host: ProcessHostCapabilitySet) {
        self.host_capabilities = Some(host);
    }

    fn is_prepared_for_start(&self) -> bool {
        false
    }

    fn start_prepared(&mut self) -> Result<(), HostServiceError> {
        Err(HostServiceError::new(
            "ERR_AGENTOS_EXECUTION_NOT_PREPARED",
            "binding execution cannot be a prepared execve image",
        ))
    }

    fn begin_shutdown(
        &mut self,
        reason: ShutdownReason,
    ) -> Result<ShutdownOutcome, HostServiceError> {
        self.cancelled.store(true, Ordering::Release);
        self.pause_notify.notify_waiters();
        self.event_notify.notify_one();
        Ok(match reason {
            ShutdownReason::Signal(signal) => ShutdownOutcome::Exited(ExecutionExit::Signaled {
                signal,
                core_dumped: false,
            }),
            ShutdownReason::RuntimeFault => ShutdownOutcome::Exited(ExecutionExit::Exited(1)),
            ShutdownReason::Deadline | ShutdownReason::VmTeardown | ShutdownReason::HostRequest => {
                ShutdownOutcome::Exited(ExecutionExit::Exited(137))
            }
            ShutdownReason::Completed => ShutdownOutcome::Exited(ExecutionExit::Exited(0)),
        })
    }

    fn set_paused(&self, paused: bool) -> Result<(), HostServiceError> {
        self.paused.store(paused, Ordering::Release);
        if !paused {
            self.pause_notify.notify_waiters();
            self.event_notify.notify_one();
        }
        Ok(())
    }

    fn write_stdin(&mut self, _bytes: &[u8]) -> Result<(), HostServiceError> {
        Ok(())
    }

    fn close_stdin(&mut self) -> Result<(), HostServiceError> {
        Ok(())
    }

    fn deliver_signal_checkpoint(
        &self,
        _identity: ExecutionWakeIdentity,
        _signal: i32,
        _delivery_token: u64,
        _flags: u32,
        _thread_id: u32,
    ) -> Result<SignalCheckpointOutcome, HostServiceError> {
        Ok(SignalCheckpointOutcome::Unsupported)
    }
}

impl ActiveExecution {
    /// Engine affinity is adapter construction metadata used only when this
    /// process later replaces or spawns its standalone-WASM image. Keep the
    /// storage-enum match behind the adapter boundary so common process
    /// lifecycle code never switches on an executor variant.
    fn standalone_wasm_backend(&self) -> ExecutionStandaloneWasmBackend {
        match self {
            Self::Wasm(execution) => execution.standalone_backend(),
            Self::Javascript(_) | Self::Python(_) | Self::Binding(_) => {
                ExecutionStandaloneWasmBackend::V8
            }
        }
    }

    fn backend(&self) -> &dyn ExecutionBackend {
        match self {
            Self::Javascript(execution) => execution,
            Self::Python(execution) => execution,
            Self::Wasm(execution) => execution.as_ref(),
            Self::Binding(execution) => execution,
        }
    }

    fn backend_mut(&mut self) -> &mut dyn ExecutionBackend {
        match self {
            Self::Javascript(execution) => execution,
            Self::Python(execution) => execution,
            Self::Wasm(execution) => execution.as_mut(),
            Self::Binding(execution) => execution,
        }
    }

    /// Poll an adapter-owned queue whose retained-byte reservation cannot be
    /// represented by the generic backend event alone. `None` means the
    /// adapter uses the common backend/event channel; `Some` owns the complete
    /// poll result, including an empty queue.
    fn poll_adapter_event_leased(
        &mut self,
    ) -> Option<Result<Option<PolledExecutionEvent>, SidecarError>> {
        match self {
            Self::Binding(execution) => Some(poll_binding_process_event_leased(execution)),
            Self::Javascript(_) | Self::Python(_) | Self::Wasm(_) => None,
        }
    }

    fn configure_adapter_event_limits(&self, count: usize, bytes: usize) {
        if let Self::Binding(execution) = self {
            execution
                .pending_event_count_limit
                .store(count, Ordering::Release);
            execution
                .pending_event_bytes_limit
                .store(bytes, Ordering::Release);
        }
    }

    fn adapter_event_bytes_budget(&self) -> Option<Arc<VmPendingByteBudget>> {
        match self {
            Self::Binding(execution) => Some(Arc::clone(&execution.vm_pending_event_bytes_budget)),
            Self::Javascript(_) | Self::Python(_) | Self::Wasm(_) => None,
        }
    }

    fn bind_adapter_event_bytes_budget(&mut self, budget: Arc<VmPendingByteBudget>) {
        if let Self::Binding(execution) = self {
            if !Arc::ptr_eq(&execution.vm_pending_event_bytes_budget, &budget) {
                debug_assert_eq!(execution.pending_event_bytes.load(Ordering::Acquire), 0);
                execution.vm_pending_event_bytes_budget = budget;
            }
        }
    }

    pub(crate) fn is_prepared_for_start(&self) -> bool {
        ExecutionBackend::is_prepared_for_start(self)
    }

    pub(crate) fn start_prepared(&mut self) -> Result<(), SidecarError> {
        ExecutionBackend::start_prepared(self).map_err(SidecarError::from)
    }

    pub(crate) fn native_process_id(&self) -> Option<u32> {
        ExecutionBackend::native_process_id(self)
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

    pub(crate) fn execution_wake_handle(
        &self,
        identity: ProcessRuntimeIdentity,
    ) -> Option<ExecutionWakeHandle> {
        ExecutionBackend::wake_handle(
            self,
            ExecutionWakeIdentity {
                generation: identity.generation,
                pid: identity.pid,
            },
        )
    }

    pub(crate) fn terminate(&mut self) -> Result<(), SidecarError> {
        self.begin_shutdown(ShutdownReason::HostRequest).map(|_| ())
    }

    pub(crate) fn begin_shutdown(
        &mut self,
        reason: ShutdownReason,
    ) -> Result<ShutdownOutcome, SidecarError> {
        ExecutionBackend::begin_shutdown(self, reason).map_err(SidecarError::from)
    }

    pub(crate) fn pause(&self) -> Result<(), SidecarError> {
        ExecutionBackend::set_paused(self, true).map_err(SidecarError::from)
    }

    pub(crate) fn resume(&self) -> Result<(), SidecarError> {
        ExecutionBackend::set_paused(self, false).map_err(SidecarError::from)
    }

    pub(crate) fn deliver_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
        signal: i32,
        delivery_token: u64,
        flags: u32,
        thread_id: u32,
    ) -> Result<SignalCheckpointOutcome, SidecarError> {
        ExecutionBackend::deliver_signal_checkpoint(
            self,
            identity,
            signal,
            delivery_token,
            flags,
            thread_id,
        )
        .map_err(SidecarError::from)
    }

    // Source-included integration tests exercise the adapter without a host
    // capability set. Production event pumps always use `poll_event_with_host`.
    #[allow(dead_code)]
    pub(crate) async fn poll_event(
        &mut self,
        identity: ProcessRuntimeIdentity,
        max_reply_bytes: usize,
        timeout: Duration,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        self.poll_event_inner(identity, max_reply_bytes, timeout, None)
            .await
    }

    pub(crate) async fn poll_event_with_host(
        &mut self,
        identity: ProcessRuntimeIdentity,
        max_reply_bytes: usize,
        timeout: Duration,
        host: &ProcessHostCapabilitySet,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        self.poll_event_inner(identity, max_reply_bytes, timeout, Some(host))
            .await
    }

    async fn poll_event_inner(
        &mut self,
        identity: ProcessRuntimeIdentity,
        max_reply_bytes: usize,
        timeout: Duration,
        host: Option<&ProcessHostCapabilitySet>,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        match self {
            Self::Javascript(execution) => {
                let responder = execution.sync_rpc_responder();
                let event = execution
                    .poll_event(timeout)
                    .await
                    .map_err(javascript_error)?;
                match event {
                    Some(event) => map_javascript_execution_event_with_host(
                        event,
                        responder,
                        identity,
                        max_reply_bytes,
                        host,
                    ),
                    None => Ok(None),
                }
            }
            Self::Python(execution) => {
                let responder = execution.javascript_sync_rpc_responder();
                let python_responder = execution.vfs_rpc_responder();
                let event = execution.poll_event(timeout).await.map_err(python_error)?;
                match event {
                    Some(event) => map_python_execution_event_with_host(
                        event,
                        responder,
                        python_responder,
                        identity,
                        max_reply_bytes,
                        host,
                    ),
                    None => Ok(None),
                }
            }
            Self::Wasm(execution) => {
                let responder = execution.sync_rpc_responder();
                let event = execution.poll_event(timeout).await.map_err(wasm_error)?;
                match event {
                    Some(event) => map_wasm_execution_event_with_host(
                        event,
                        responder,
                        identity,
                        max_reply_bytes,
                        host,
                    ),
                    None => Ok(None),
                }
            }
            Self::Binding(execution) => {
                let _ = timeout;
                poll_binding_process_event(execution)
            }
        }
    }

    /// Probe the runtime event queue once without parking the sidecar thread or
    /// registering a waker outside the coalesced process-event broker.
    pub(crate) fn try_poll_event_with_host(
        &mut self,
        identity: ProcessRuntimeIdentity,
        max_reply_bytes: usize,
        host: &ProcessHostCapabilitySet,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        self.try_poll_event_inner(identity, max_reply_bytes, Some(host))
    }

    fn try_poll_event_inner(
        &mut self,
        identity: ProcessRuntimeIdentity,
        max_reply_bytes: usize,
        host: Option<&ProcessHostCapabilitySet>,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        match self {
            Self::Javascript(execution) => {
                let responder = execution.sync_rpc_responder();
                let event = execution.try_poll_event().map_err(javascript_error)?;
                match event {
                    Some(event) => map_javascript_execution_event_with_host(
                        event,
                        responder,
                        identity,
                        max_reply_bytes,
                        host,
                    ),
                    None => Ok(None),
                }
            }
            Self::Python(execution) => {
                let responder = execution.javascript_sync_rpc_responder();
                let python_responder = execution.vfs_rpc_responder();
                let event = execution.try_poll_event().map_err(python_error)?;
                match event {
                    Some(event) => map_python_execution_event_with_host(
                        event,
                        responder,
                        python_responder,
                        identity,
                        max_reply_bytes,
                        host,
                    ),
                    None => Ok(None),
                }
            }
            Self::Wasm(execution) => {
                let responder = execution.sync_rpc_responder();
                let event = execution.try_poll_event().map_err(wasm_error)?;
                match event {
                    Some(event) => map_wasm_execution_event_with_host(
                        event,
                        responder,
                        identity,
                        max_reply_bytes,
                        host,
                    ),
                    None => Ok(None),
                }
            }
            Self::Binding(execution) => poll_binding_process_event(execution),
        }
    }
}

impl ExecutionBackend for ActiveExecution {
    fn kind(&self) -> ExecutionBackendKind {
        self.backend().kind()
    }

    fn synchronous_fd_write_policy(&self) -> agentos_execution::backend::SynchronousFdWritePolicy {
        self.backend().synchronous_fd_write_policy()
    }

    fn descendant_wait_ownership(&self) -> DescendantWaitOwnership {
        self.backend().descendant_wait_ownership()
    }

    fn descendant_output_ownership(&self) -> DescendantOutputOwnership {
        self.backend().descendant_output_ownership()
    }

    fn native_process_id(&self) -> Option<u32> {
        self.backend().native_process_id()
    }

    fn wake_handle(&self, identity: ExecutionWakeIdentity) -> Option<ExecutionWakeHandle> {
        self.backend().wake_handle(identity)
    }

    fn configure_host_services(&mut self, host: ProcessHostCapabilitySet) {
        self.backend_mut().configure_host_services(host)
    }

    fn is_prepared_for_start(&self) -> bool {
        self.backend().is_prepared_for_start()
    }

    fn start_prepared(&mut self) -> Result<(), HostServiceError> {
        self.backend_mut().start_prepared()
    }

    fn begin_shutdown(
        &mut self,
        reason: ShutdownReason,
    ) -> Result<ShutdownOutcome, HostServiceError> {
        self.backend_mut().begin_shutdown(reason)
    }

    fn set_paused(&self, paused: bool) -> Result<(), HostServiceError> {
        self.backend().set_paused(paused)
    }

    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), HostServiceError> {
        self.backend_mut().write_stdin(bytes)
    }

    fn close_stdin(&mut self) -> Result<(), HostServiceError> {
        self.backend_mut().close_stdin()
    }

    fn deliver_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
        signal: i32,
        delivery_token: u64,
        flags: u32,
        thread_id: u32,
    ) -> Result<SignalCheckpointOutcome, HostServiceError> {
        self.backend()
            .deliver_signal_checkpoint(identity, signal, delivery_token, flags, thread_id)
    }

    fn take_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        self.backend().take_signal_checkpoint(identity)
    }

    fn take_signal_checkpoint_for_thread(
        &self,
        identity: ExecutionWakeIdentity,
        thread_id: u32,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        self.backend()
            .take_signal_checkpoint_for_thread(identity, thread_id)
    }

    fn discard_signal_checkpoints(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<(), HostServiceError> {
        self.backend().discard_signal_checkpoints(identity)
    }
}

pub(super) fn discard_exec_signal_state(process: &mut ActiveProcess) {
    let identity = process.kernel_handle.runtime_identity();
    if let Err(error) = process
        .execution
        .discard_signal_checkpoints(ExecutionWakeIdentity {
            generation: identity.generation,
            pid: identity.pid,
        })
    {
        eprintln!("ERR_AGENTOS_EXEC_SIGNAL_CHECKPOINT_DISCARD: {error}");
    }
    process.guest_signal_checkpoint_pending = false;
}

#[cfg(test)]
mod execution_backend_lifecycle_tests {
    use super::*;

    fn assert_execution_backend<T: ExecutionBackend>() {}

    #[test]
    fn binding_and_active_execution_use_the_common_lifecycle_contract() {
        assert_execution_backend::<BindingExecution>();
        assert_execution_backend::<ActiveExecution>();

        let binding = BindingExecution::default();
        let cancelled = Arc::clone(&binding.cancelled);
        let mut execution = ActiveExecution::Binding(binding);

        let process = HostProcessContext {
            generation: 11,
            pid: 29,
        };
        let (events, _receiver) = bounded_execution_event_channel(
            process,
            1,
            PayloadLimit::new("limits.process.pendingEventBytes", 1024).expect("byte limit"),
            Arc::new(|| {}),
        )
        .expect("host event lane");
        ExecutionBackend::configure_host_services(
            &mut execution,
            ProcessHostCapabilitySet::from_event_submission(events),
        );
        let ActiveExecution::Binding(binding) = &execution else {
            unreachable!("binding execution")
        };
        assert_eq!(
            binding
                .host_capabilities
                .as_ref()
                .expect("backend received host services")
                .process(),
            process
        );

        assert_eq!(
            ExecutionBackend::kind(&execution),
            ExecutionBackendKind::Binding
        );
        assert!(!execution.is_prepared_for_start());

        let error = ExecutionBackend::start_prepared(&mut execution)
            .expect_err("binding adapters are never prepared exec images");
        assert_eq!(error.code, "ERR_AGENTOS_EXECUTION_NOT_PREPARED");

        let outcome = ExecutionBackend::begin_shutdown(&mut execution, ShutdownReason::VmTeardown)
            .expect("binding shutdown uses the shared lifecycle");
        assert_eq!(outcome, ShutdownOutcome::Exited(ExecutionExit::Exited(137)));
        assert!(cancelled.load(Ordering::Acquire));

        let outcome = ExecutionBackend::begin_shutdown(&mut execution, ShutdownReason::Signal(15))
            .expect("binding signal shutdown uses the shared lifecycle");
        assert_eq!(
            outcome,
            ShutdownOutcome::Exited(ExecutionExit::Signaled {
                signal: 15,
                core_dumped: false,
            })
        );
    }
}

fn execution_host_call(
    request: HostRpcRequest,
    responder: JavascriptSyncRpcResponder,
    identity: ProcessRuntimeIdentity,
    max_reply_bytes: usize,
) -> Result<ExecutionHostCall, SidecarError> {
    let reply = DirectHostReplyHandle::new(
        HostCallIdentity {
            generation: identity.generation,
            pid: identity.pid,
            call_id: request.id,
        },
        Arc::new(responder),
        max_reply_bytes,
    )
    .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
    Ok(ExecutionHostCall { request, reply })
}

pub(crate) fn settle_execution_host_call(
    reply: &DirectHostReplyHandle,
    response: Result<HostServiceResponse, SidecarError>,
) -> Result<(), SidecarError> {
    let result = match response {
        Ok(HostServiceResponse::Json(value)) => reply.succeed(HostCallReply::Json(value)),
        Ok(HostServiceResponse::Raw(payload)) => reply.succeed(HostCallReply::Raw(payload)),
        Ok(HostServiceResponse::SourceBackedJson {
            value,
            source_reservations,
        }) => reply.succeed_retained(HostCallReply::Json(value), source_reservations),
        Ok(HostServiceResponse::SourceBackedRaw {
            payload,
            source_reservations,
        }) => reply.succeed_retained(HostCallReply::Raw(payload), source_reservations),
        Ok(HostServiceResponse::Deferred { .. }) => Err(HostServiceError::new(
            "EINVAL",
            "deferred response must be awaited before direct settlement",
        )),
        Err(error) => reply.fail(host_service_error(&error)),
    };
    result.map_err(SidecarError::from)
}

#[cfg(test)]
mod typed_direct_error_tests {
    use super::*;
    use agentos_execution::backend::{
        DirectHostReplyTarget, HostCallIdentity, HostCallReply, HostServiceError,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingTarget {
        replies: Mutex<Vec<Result<HostCallReply, HostServiceError>>>,
    }

    impl DirectHostReplyTarget for RecordingTarget {
        fn claim(&self, _call_id: u64) -> Result<bool, HostServiceError> {
            Ok(true)
        }

        fn respond(
            &self,
            _call_id: u64,
            _claimed: bool,
            result: Result<HostCallReply, HostServiceError>,
        ) -> Result<(), HostServiceError> {
            self.replies.lock().expect("reply lock").push(result);
            Ok(())
        }
    }

    fn reply(target: Arc<RecordingTarget>, call_id: u64) -> DirectHostReplyHandle {
        DirectHostReplyHandle::new(
            HostCallIdentity {
                generation: 3,
                pid: 41,
                call_id,
            },
            target,
            64 * 1024,
        )
        .expect("direct reply")
    }

    #[test]
    fn direct_settlement_preserves_limit_details_and_typed_errno() {
        let target = Arc::new(RecordingTarget::default());
        let limit = SidecarError::ResourceLimit(agentos_runtime_tokio::accounting::LimitError {
            scope: String::from("vm=vm-typed-errors"),
            resource: agentos_runtime_tokio::accounting::ResourceClass::BridgeResponseBytes,
            used: 60,
            requested: 8,
            limit: 64,
            config_path: String::from("runtime.resources.maxBridgeResponseBytes"),
        });
        let deferred = crate::state::DeferredRpcError::from(host_service_error(&limit));
        settle_execution_host_call(
            &reply(Arc::clone(&target), 1),
            Err(SidecarError::from(deferred)),
        )
        .expect("settle deferred limit error");

        let denied = HostServiceError::new(
            "EACCES",
            "permission denied; diagnostic mentions ENOENT but must not change the code",
        )
        .with_details(serde_json::json!({ "path": "/private/config" }));
        settle_execution_host_call(
            &reply(Arc::clone(&target), 2),
            Err(SidecarError::Host(denied)),
        )
        .expect("settle permission error");

        let replies = target.replies.lock().expect("reply lock");
        let limit = replies[0].as_ref().expect_err("limit error response");
        assert_eq!(limit.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        let details = limit.details.as_ref().expect("limit details");
        assert_eq!(details["limitName"], "bridgeResponseBytes");
        assert_eq!(details["limit"], 64);
        assert_eq!(details["observed"], 68);
        assert_eq!(
            details["configPath"],
            "runtime.resources.maxBridgeResponseBytes"
        );

        let denied = replies[1].as_ref().expect_err("permission error response");
        assert_eq!(denied.code, "EACCES");
        assert_eq!(
            denied.details.as_ref().expect("errno details")["path"],
            "/private/config"
        );
    }
}

fn map_javascript_execution_event_with_host(
    event: JavascriptExecutionEvent,
    responder: JavascriptSyncRpcResponder,
    identity: ProcessRuntimeIdentity,
    max_reply_bytes: usize,
    host: Option<&ProcessHostCapabilitySet>,
) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
    let event = match event {
        JavascriptExecutionEvent::Stdout(chunk) => ActiveExecutionEvent::Stdout(chunk),
        JavascriptExecutionEvent::Stderr(chunk) => ActiveExecutionEvent::Stderr(chunk),
        JavascriptExecutionEvent::SyncRpcRequest(request) => {
            return route_compatibility_host_call(
                execution_host_call(request, responder, identity, max_reply_bytes)?,
                false,
                max_reply_bytes,
                host,
            );
        }
        JavascriptExecutionEvent::SignalState {
            signal,
            registration,
        } => ActiveExecutionEvent::SignalState {
            signal,
            registration: map_execution_signal_registration(registration),
        },
        JavascriptExecutionEvent::Exited(code) => ActiveExecutionEvent::Exited(code),
    };
    Ok(Some(event))
}

fn map_python_execution_event_with_host(
    event: PythonExecutionEvent,
    responder: JavascriptSyncRpcResponder,
    python_responder: PythonVfsRpcResponder,
    identity: ProcessRuntimeIdentity,
    max_reply_bytes: usize,
    host: Option<&ProcessHostCapabilitySet>,
) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
    let event = match event {
        PythonExecutionEvent::Stdout(chunk) => ActiveExecutionEvent::Stdout(chunk),
        PythonExecutionEvent::Stderr(chunk) => ActiveExecutionEvent::Stderr(chunk),
        PythonExecutionEvent::HostRpcRequest(request) => {
            return route_compatibility_host_call(
                execution_host_call(request, responder, identity, max_reply_bytes)?,
                false,
                max_reply_bytes,
                host,
            );
        }
        PythonExecutionEvent::VfsRpcRequest(request) => {
            let Some(host) = host else {
                python_responder
                    .respond_host_error(
                        request.id,
                        HostServiceError::new(
                            "ENOTSUP",
                            "Python host capabilities are unavailable",
                        )
                        .with_details(json!({
                            "generation": identity.generation,
                            "pid": identity.pid,
                        })),
                    )
                    .map_err(python_error)?;
                return Ok(None);
            };
            let admission = match host.admit_json_request(&*request, 0) {
                Ok(admission) => admission,
                Err(error) => {
                    python_responder
                        .respond_host_error(request.id, error)
                        .map_err(python_error)?;
                    return Ok(None);
                }
            };
            let call_id = request.id;
            let Some(call) = (match python_responder.try_host_call(
                *request,
                HostCallIdentity {
                    generation: identity.generation,
                    pid: identity.pid,
                    call_id,
                },
                max_reply_bytes,
                max_reply_bytes,
            ) {
                Ok(call) => call,
                Err(error) => {
                    python_responder
                        .respond_host_error(call_id, error)
                        .map_err(python_error)?;
                    return Ok(None);
                }
            }) else {
                return Err(SidecarError::host(
                    "ENOSYS",
                    "Python request was not converted to a common host operation",
                ));
            };
            if let Err(error) = host.submit(call.operation, call.reply.clone(), admission) {
                call.reply.fail(error).map_err(SidecarError::from)?;
            }
            return Ok(None);
        }
        PythonExecutionEvent::Exited(code) => ActiveExecutionEvent::Exited(code),
    };
    Ok(Some(event))
}

fn map_wasm_execution_event_with_host(
    event: WasmExecutionEvent,
    responder: Option<JavascriptSyncRpcResponder>,
    identity: ProcessRuntimeIdentity,
    max_reply_bytes: usize,
    host: Option<&ProcessHostCapabilitySet>,
) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
    let event = match event {
        WasmExecutionEvent::Stdout(chunk) => ActiveExecutionEvent::Stdout(chunk),
        WasmExecutionEvent::Stderr(chunk) => ActiveExecutionEvent::Stderr(chunk),
        WasmExecutionEvent::SyncRpcRequest(request) => {
            let responder = responder.ok_or_else(|| {
                SidecarError::host(
                    "ERR_AGENTOS_WASMTIME_SYNC_RPC",
                    "native Wasmtime emitted an impossible V8 sync RPC event",
                )
            })?;
            return route_compatibility_host_call(
                execution_host_call(request, responder, identity, max_reply_bytes)?,
                true,
                max_reply_bytes,
                host,
            );
        }
        WasmExecutionEvent::HostCall { request, reply } => {
            return route_compatibility_host_call(
                ExecutionHostCall { request, reply },
                true,
                max_reply_bytes,
                host,
            );
        }
        WasmExecutionEvent::SignalState {
            signal,
            registration,
        } => ActiveExecutionEvent::SignalState {
            signal,
            registration: map_execution_signal_registration(registration),
        },
        WasmExecutionEvent::Exited(code) => ActiveExecutionEvent::Exited(code),
    };
    Ok(Some(event))
}

fn route_compatibility_host_call(
    call: ExecutionHostCall,
    full_filesystem: bool,
    max_reply_bytes: usize,
    host: Option<&ProcessHostCapabilitySet>,
) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
    #[derive(Serialize)]
    struct CompatibilityRequestCharge<'a> {
        method: &'a str,
        args: &'a [Value],
    }

    let reply = call.reply.clone();
    let admission = match host
        .map(|host| {
            let raw_bytes = call
                .request
                .raw_bytes_args
                .values()
                .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
                .ok_or_else(|| {
                    HostServiceError::new(
                        "EOVERFLOW",
                        "compatibility host-call raw payload size overflowed",
                    )
                })?;
            host.admit_json_request(
                &CompatibilityRequestCharge {
                    method: &call.request.method,
                    args: &call.request.args,
                },
                raw_bytes,
            )
        })
        .transpose()
    {
        Ok(admission) => admission,
        Err(error) => {
            reply.fail(error).map_err(SidecarError::from)?;
            return Ok(None);
        }
    };
    let event = match decode_compatibility_host_call(call, full_filesystem, max_reply_bytes) {
        Ok(event) => event,
        Err(error) => {
            reply
                .fail(host_service_error(&error))
                .map_err(SidecarError::from)?;
            return Ok(None);
        }
    };
    let Some(host) = host else {
        return Ok(Some(event));
    };
    match event {
        ActiveExecutionEvent::Common(ExecutionEvent::HostCall { operation, reply }) => {
            host.submit(
                operation,
                reply,
                admission.expect("host-backed routing always creates request admission"),
            )
            .map_err(SidecarError::from)?;
            Ok(None)
        }
        other => Ok(Some(other)),
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

        let Some(child_pid) = process.execution.native_process_id() else {
            continue;
        };
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
    Err(SidecarError::host(
        "EACCES",
        format!("permission denied, {resource}: {reason}"),
    ))
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SidecarError::Io(format!(
                    "failed to inspect socket descriptor target for process {pid}: {error}"
                )));
            }
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
pub(super) fn collect_socket_port_state(
    kernel: &SidecarKernel,
    process_id: &str,
    process: &ActiveProcess,
    tcp_guest_to_host: &mut BTreeMap<(SocketFamily, u16), u16>,
    http_loopback_targets: &mut BTreeMap<(SocketFamily, u16), HttpLoopbackTarget>,
    udp_guest_to_host: &mut BTreeMap<(SocketFamily, u16), u16>,
    udp_host_to_guest: &mut BTreeMap<(SocketFamily, u16), u16>,
    used_tcp_ports: &mut BTreeMap<SocketFamily, BTreeSet<u16>>,
    used_udp_ports: &mut BTreeMap<SocketFamily, BTreeSet<u16>>,
) {
    for (family, port) in process.tcp_port_reservations.values() {
        used_tcp_ports.entry(*family).or_default().insert(*port);
    }

    let mut record_tcp_listener = |guest_addr: SocketAddr, host_port: u16| {
        let family = SocketFamily::from_ip(guest_addr.ip());
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
            Err(error) => {
                eprintln!(
                    "ERR_AGENTOS_SOCKET_INVENTORY: failed to inspect HTTP listener {server_id} for process {process_id}: {error}"
                );
                continue;
            }
        };
        record_tcp_listener(server.guest_local_addr, host_port);
        let family = SocketFamily::from_ip(server.guest_local_addr.ip());
        http_loopback_targets.insert(
            (family, server.guest_local_addr.port()),
            HttpLoopbackTarget {
                process_id: process_id.to_owned(),
                server_id: *server_id,
            },
        );
    }

    match process.http2.shared.lock() {
        Ok(http2) => {
            for server in http2.servers.values() {
                record_tcp_listener(server.guest_local_addr, server.actual_local_addr.port());
            }
        }
        Err(error) => {
            eprintln!(
                "ERR_AGENTOS_SOCKET_INVENTORY: failed to inspect HTTP/2 listeners for process {process_id}: {error}"
            );
        }
    }

    for socket in process.tcp_sockets.values() {
        let guest_addr = socket
            .kernel_socket_id
            .and_then(|socket_id| kernel.socket_get(socket_id))
            .and_then(|record| record.local_address().cloned())
            .and_then(|address| resolve_tcp_bind_addr(address.host(), address.port()).ok())
            .unwrap_or(socket.guest_local_addr);
        let family = SocketFamily::from_ip(guest_addr.ip());
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
        let family = SocketFamily::from_ip(guest_addr.ip());
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
        collect_socket_port_state(
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
        agentos_runtime_tokio::capability::CapabilityId,
        agentos_runtime_tokio::capability::CapabilityGeneration,
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
        if let Err(error) = request.reply.fail(HostServiceError::new(
            "EINTR",
            "process teardown interrupted the pending host call",
        )) {
            eprintln!("ERR_AGENTOS_PARKED_HOST_REPLY_TEARDOWN: {error}");
        }
    }
    if let Some(wait) = process.clear_deferred_guest_wait() {
        if let Err(error) = wait.reply.fail(HostServiceError::new(
            "EINTR",
            "process teardown interrupted the pending wait",
        )) {
            eprintln!("ERR_AGENTOS_PARKED_GUEST_WAIT_TEARDOWN: {error}");
        }
    }
    if let Some(poll) = process.clear_deferred_kernel_poll() {
        if let Some(token) = poll.temporary_signal_mask_token {
            let result = if let Some(thread_id) = poll.temporary_signal_thread_id {
                process
                    .kernel_handle
                    .end_temporary_signal_mask_for_thread(thread_id, token)
            } else {
                process.kernel_handle.end_temporary_signal_mask(token)
            };
            if let Err(error) = result {
                eprintln!("ERR_AGENTOS_PPOLL_MASK_RESTORE_TEARDOWN: {error}");
            }
        }
        if let Err(error) = poll.reply.fail(HostServiceError::new(
            "EINTR",
            "process teardown interrupted the pending kernel poll",
        )) {
            eprintln!("ERR_AGENTOS_PARKED_KERNEL_POLL_TEARDOWN: {error}");
        }
    }
    if let Some(read) = process.clear_deferred_kernel_read() {
        if let Err(error) = read.reply.fail(HostServiceError::new(
            "EINTR",
            "process teardown interrupted the pending descriptor read",
        )) {
            eprintln!("ERR_AGENTOS_PARKED_KERNEL_READ_TEARDOWN: {error}");
        }
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
                "ERR_AGENTOS_SQLITE_TEARDOWN: failed to close database {database_id} while terminating process {}: {error}",
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

    let child_ids = process.child_processes.keys().cloned().collect::<Vec<_>>();
    for child_id in child_ids {
        let Some(mut child) = process.child_processes.remove(&child_id) else {
            continue;
        };
        terminate_child_process_tree(kernel, &mut child, kernel_readiness, unix_address_registry);
        if let Err(error) = kernel.kill_process(EXECUTION_DRIVER_NAME, child.kernel_pid, SIGTERM) {
            eprintln!(
                "ERR_AGENTOS_CHILD_TEARDOWN_SIGNAL: failed to signal child kernel pid {}: {error}",
                child.kernel_pid
            );
        }
        if let Some(native_process_id) = child.execution.native_process_id() {
            if let Err(error) = signal_runtime_process(native_process_id, SIGTERM) {
                eprintln!(
                    "ERR_AGENTOS_CHILD_TEARDOWN_SIGNAL: failed to signal native child pid {native_process_id}: {error}"
                );
            }
        }
        child.kernel_handle.finish(0);
        if let Err(error) = kernel.wait_and_reap(child.kernel_pid) {
            eprintln!(
                "ERR_AGENTOS_CHILD_TEARDOWN_REAP: failed to reap child kernel pid {}: {error}",
                child.kernel_pid
            );
        }
    }
}
