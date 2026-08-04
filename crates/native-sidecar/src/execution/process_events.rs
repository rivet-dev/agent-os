use super::*;
use crate::protocol::ExecutionStreamChannel;

pub(crate) struct ProcessEventPumpTurn {
    pub(crate) emitted_any: bool,
    pub(crate) javascript_services: Vec<OwnedJavascriptEventService>,
    pub(crate) python_services: Vec<OwnedPythonEventService>,
    pub(crate) python_socket_completions: Vec<OwnedPythonSocketCompletionService>,
    pub(crate) child_bridge_services: Vec<OwnedChildBridgeEventService>,
}

pub(crate) struct OwnedJavascriptEventService {
    pub(crate) ownership: OwnershipScope,
    pub(crate) vm_id: String,
    pub(crate) process_id: String,
    /// Descendant path below `process_id`; empty for a root process.
    pub(crate) child_path: Vec<String>,
    pub(crate) vm: crate::state::VmHandle,
    pub(crate) request: JavascriptSyncRpcRequest,
    _reservation: Option<PendingExecutionEventReservation>,
}

pub(crate) struct OwnedPythonEventService {
    pub(crate) ownership: OwnershipScope,
    pub(crate) vm_id: String,
    pub(crate) process_id: String,
    /// Descendant path below `process_id`; empty for a root process.
    pub(crate) child_path: Vec<String>,
    pub(crate) vm: crate::state::VmHandle,
    pub(crate) responder: PythonVfsRpcResponder,
    pub(crate) request: PythonVfsRpcRequest,
    _reservation: Option<PendingExecutionEventReservation>,
}

pub(crate) struct OwnedPythonSocketCompletionService {
    pub(crate) ownership: OwnershipScope,
    pub(crate) vm_id: String,
    pub(crate) process_id: String,
    pub(crate) child_path: Vec<String>,
    pub(crate) vm: crate::state::VmHandle,
    pub(crate) responder: PythonVfsRpcResponder,
    pub(crate) completion: PythonSocketConnectCompletion,
    _reservation: Option<PendingExecutionEventReservation>,
}

impl OwnedJavascriptEventService {
    pub(super) fn new(
        ownership: OwnershipScope,
        vm_id: String,
        process_id: String,
        child_path: Vec<String>,
        vm: crate::state::VmHandle,
        request: JavascriptSyncRpcRequest,
        reservation: Option<PendingExecutionEventReservation>,
    ) -> Self {
        Self {
            ownership,
            vm_id,
            process_id,
            child_path,
            vm,
            request,
            _reservation: reservation,
        }
    }
}

impl OwnedPythonEventService {
    pub(super) fn new(
        ownership: OwnershipScope,
        vm_id: String,
        process_id: String,
        child_path: Vec<String>,
        vm: crate::state::VmHandle,
        responder: PythonVfsRpcResponder,
        request: PythonVfsRpcRequest,
        reservation: Option<PendingExecutionEventReservation>,
    ) -> Self {
        Self {
            ownership,
            vm_id,
            process_id,
            child_path,
            vm,
            responder,
            request,
            _reservation: reservation,
        }
    }
}

impl OwnedPythonSocketCompletionService {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        ownership: OwnershipScope,
        vm_id: String,
        process_id: String,
        child_path: Vec<String>,
        vm: crate::state::VmHandle,
        responder: PythonVfsRpcResponder,
        completion: PythonSocketConnectCompletion,
        reservation: Option<PendingExecutionEventReservation>,
    ) -> Self {
        Self {
            ownership,
            vm_id,
            process_id,
            child_path,
            vm,
            responder,
            completion,
            _reservation: reservation,
        }
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn into_parts(
        self,
    ) -> (
        OwnershipScope,
        String,
        String,
        Vec<String>,
        crate::state::VmHandle,
        PythonVfsRpcResponder,
        PythonSocketConnectCompletion,
        Option<PendingExecutionEventReservation>,
    ) {
        (
            self.ownership,
            self.vm_id,
            self.process_id,
            self.child_path,
            self.vm,
            self.responder,
            self.completion,
            self._reservation,
        )
    }
}

pub(super) struct BindingProcessEventRequest {
    pub(super) runtime_context: agentos_runtime::RuntimeContext,
    pub(super) sidecar_requests: SharedSidecarRequestClient,
    pub(super) connection_id: String,
    pub(super) session_id: String,
    pub(super) vm_id: String,
    pub(super) binding_resolution: BindingCommandResolution,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) pending_events: Arc<Mutex<VecDeque<ActiveExecutionEvent>>>,
    pub(super) event_overflow_reason: Arc<Mutex<Option<String>>>,
    pub(super) pending_event_bytes: Arc<AtomicUsize>,
    pub(super) pending_event_count_limit: Arc<AtomicUsize>,
    pub(super) pending_event_bytes_limit: Arc<AtomicUsize>,
    pub(super) vm_pending_event_bytes_budget: Arc<VmPendingByteBudget>,
    pub(super) event_notify: Arc<tokio::sync::Notify>,
}

// The producer owns these independent atomics/queues; keeping them explicit
// avoids introducing another partially initialized shared-state wrapper.
#[allow(clippy::too_many_arguments)]
pub(crate) fn send_binding_process_event(
    cancelled: &AtomicBool,
    pending_events: &Arc<Mutex<VecDeque<ActiveExecutionEvent>>>,
    event_overflow_reason: &Mutex<Option<String>>,
    pending_event_bytes: &AtomicUsize,
    pending_event_count_limit: &AtomicUsize,
    pending_event_bytes_limit: &AtomicUsize,
    vm_pending_event_bytes_budget: &VmPendingByteBudget,
    event: ActiveExecutionEvent,
) -> bool {
    let mut pending_events = pending_events
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cancelled.load(Ordering::Acquire) {
        return false;
    }
    let count_limit = pending_event_count_limit.load(Ordering::Acquire);
    let event_bytes = event.retained_bytes();
    let bytes = pending_event_bytes.load(Ordering::Acquire);
    if pending_events.len() >= count_limit {
        let mut reason = event_overflow_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reason.get_or_insert_with(|| {
            format!(
                "process execution event queue exceeded {count_limit} events \
                 (limits.process.pendingEventCount); raise limits.process.pendingEventCount"
            )
        });
        return false;
    }
    let byte_limit = pending_event_bytes_limit.load(Ordering::Acquire);
    if bytes.saturating_add(event_bytes) > byte_limit {
        let mut reason = event_overflow_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reason.get_or_insert_with(|| {
            format!(
                "process execution event queue exceeded {byte_limit} bytes \
                 (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes"
            )
        });
        return false;
    }
    if !vm_pending_event_bytes_budget.try_reserve(event_bytes) {
        let mut reason = event_overflow_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reason.get_or_insert_with(|| {
            format!(
                "VM process execution event queues exceeded {} bytes \
                 (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes",
                vm_pending_event_bytes_budget.limit()
            )
        });
        return false;
    }
    pending_events.push_back(event);
    pending_event_bytes.fetch_add(event_bytes, Ordering::AcqRel);
    true
}

#[allow(clippy::too_many_arguments)]
fn send_binding_process_event_and_notify(
    cancelled: &AtomicBool,
    pending_events: &Arc<Mutex<VecDeque<ActiveExecutionEvent>>>,
    event_overflow_reason: &Mutex<Option<String>>,
    pending_event_bytes: &AtomicUsize,
    pending_event_count_limit: &AtomicUsize,
    pending_event_bytes_limit: &AtomicUsize,
    vm_pending_event_bytes_budget: &VmPendingByteBudget,
    event_notify: &tokio::sync::Notify,
    event: ActiveExecutionEvent,
) -> bool {
    let sent = send_binding_process_event(
        cancelled,
        pending_events,
        event_overflow_reason,
        pending_event_bytes,
        pending_event_count_limit,
        pending_event_bytes_limit,
        vm_pending_event_bytes_budget,
        event,
    );
    if sent {
        event_notify.notify_one();
    }
    sent
}

pub(super) fn spawn_binding_process_events(request: BindingProcessEventRequest) {
    let BindingProcessEventRequest {
        runtime_context,
        sidecar_requests,
        connection_id,
        session_id,
        vm_id,
        binding_resolution,
        cancelled,
        pending_events,
        event_overflow_reason,
        pending_event_bytes,
        pending_event_count_limit,
        pending_event_bytes_limit,
        vm_pending_event_bytes_budget,
        event_notify,
    } = request;
    let failure_cancelled = Arc::clone(&cancelled);
    let failure_events = Arc::clone(&pending_events);
    let failure_overflow_reason = Arc::clone(&event_overflow_reason);
    let failure_event_bytes = Arc::clone(&pending_event_bytes);
    let failure_event_count_limit = Arc::clone(&pending_event_count_limit);
    let failure_event_bytes_limit = Arc::clone(&pending_event_bytes_limit);
    let failure_vm_event_bytes_budget = Arc::clone(&vm_pending_event_bytes_budget);
    let failure_notify = Arc::clone(&event_notify);
    let submit_result =
        runtime_context
            .blocking()
            .submit(BINDING_HOST_CALL_BLOCKING_JOB_BYTES, move || {
                let enqueue = |event| {
                    send_binding_process_event_and_notify(
                        &cancelled,
                        &pending_events,
                        &event_overflow_reason,
                        &pending_event_bytes,
                        &pending_event_count_limit,
                        &pending_event_bytes_limit,
                        &vm_pending_event_bytes_budget,
                        &event_notify,
                        event,
                    )
                };
                match binding_resolution {
                    BindingCommandResolution::Failure(message) => {
                        if enqueue(ActiveExecutionEvent::Stderr(format_binding_failure_output(
                            &message,
                        ))) {
                            let _ = enqueue(ActiveExecutionEvent::Exited(1));
                        }
                    }
                    BindingCommandResolution::Invoke { request, timeout } => {
                        let response = sidecar_requests.invoke(
                            OwnershipScope::vm(connection_id, session_id, vm_id),
                            SidecarRequestPayload::HostCallback(request),
                            timeout,
                        );
                        if cancelled.load(Ordering::Acquire) {
                            return;
                        }
                        let (output, exit_code, stdout) = match response {
                            Ok(crate::protocol::SidecarResponsePayload::HostCallbackResult(
                                result,
                            )) => {
                                if let Some(value) = result.result {
                                    let value: serde_json::Value = serde_json::from_str(&value)
                                        .unwrap_or(serde_json::Value::String(value));
                                    let output = serde_json::to_vec(&json!({
                                        "ok": true,
                                        "result": value,
                                    }))
                                    .unwrap_or_else(|error| {
                                        format_binding_failure_output(&format!(
                                            "failed to serialize binding result: {error}"
                                        ))
                                    });
                                    (output, 0, true)
                                } else {
                                    let message = result.error.unwrap_or_else(|| {
                                        String::from("binding invocation returned no result")
                                    });
                                    (format_binding_failure_output(&message), 1, false)
                                }
                            }
                            Ok(_) => (
                                format_binding_failure_output(
                                    "unexpected sidecar binding response",
                                ),
                                1,
                                false,
                            ),
                            Err(error) => {
                                (format_binding_failure_output(&error.to_string()), 1, false)
                            }
                        };
                        let output_event = if stdout {
                            ActiveExecutionEvent::Stdout(output)
                        } else {
                            ActiveExecutionEvent::Stderr(output)
                        };
                        if enqueue(output_event) {
                            let _ = enqueue(ActiveExecutionEvent::Exited(exit_code));
                        }
                    }
                }
            });
    if let Err(error) = submit_result {
        let enqueue_failure = |event| {
            send_binding_process_event_and_notify(
                &failure_cancelled,
                &failure_events,
                &failure_overflow_reason,
                &failure_event_bytes,
                &failure_event_count_limit,
                &failure_event_bytes_limit,
                &failure_vm_event_bytes_budget,
                &failure_notify,
                event,
            )
        };
        if enqueue_failure(ActiveExecutionEvent::Stderr(format_binding_failure_output(
            &error.to_string(),
        ))) {
            let _ = enqueue_failure(ActiveExecutionEvent::Exited(1));
        }
    }
}

static SYNC_RPC_STATS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::BTreeMap<String, u64>>,
> = std::sync::OnceLock::new();

#[derive(Default)]
struct ExecutePhaseStats {
    calls: u64,
    total_ns: u128,
    max_ns: u128,
}

static EXECUTE_PHASES: OnceLock<Mutex<BTreeMap<String, ExecutePhaseStats>>> = OnceLock::new();
static EXECUTE_LIFETIMES: OnceLock<Mutex<BTreeMap<String, Instant>>> = OnceLock::new();
static EXECUTE_EXIT_EVENT_QUEUED: OnceLock<Mutex<BTreeMap<String, Instant>>> = OnceLock::new();

fn execute_phases_enabled() -> bool {
    std::env::var("AGENTOS_EXECUTE_PHASES").as_deref() == Ok("1")
}

fn execute_phase_key(vm_id: &str, process_id: &str) -> String {
    format!("{vm_id}/{process_id}")
}

pub(crate) fn record_execute_phase(stage: &str, elapsed: Duration) {
    if !execute_phases_enabled() {
        return;
    }
    let phases = EXECUTE_PHASES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let Ok(mut phases) = phases.lock() else {
        return;
    };
    let stats = phases.entry(stage.to_string()).or_default();
    stats.calls += 1;
    let elapsed_ns = elapsed.as_nanos();
    stats.total_ns += elapsed_ns;
    stats.max_ns = stats.max_ns.max(elapsed_ns);

    let Some(path) = std::env::var_os("AGENTOS_EXECUTE_PHASES_FILE") else {
        return;
    };
    let mut output = String::new();
    for (stage, stats) in phases.iter() {
        let total_us = stats.total_ns / 1_000;
        let avg_us = if stats.calls == 0 {
            0
        } else {
            total_us / u128::from(stats.calls)
        };
        let max_us = stats.max_ns / 1_000;
        output.push_str(&format!(
            "stage={stage} calls={} total_us={total_us} avg_us={avg_us} max_us={max_us}\n",
            stats.calls
        ));
    }
    let _ = fs::write(path, output);
}

pub(super) fn mark_execute_response_ready(vm_id: &str, process_id: &str) {
    if !execute_phases_enabled() {
        return;
    }
    let lifetimes = EXECUTE_LIFETIMES.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(mut lifetimes) = lifetimes.lock() {
        lifetimes.insert(execute_phase_key(vm_id, process_id), Instant::now());
    }
}

pub(crate) fn mark_execute_exit_event_queued(vm_id: &str, process_id: &str) {
    if !execute_phases_enabled() {
        return;
    }
    let queued = EXECUTE_EXIT_EVENT_QUEUED.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(mut queued) = queued.lock() {
        let key = execute_phase_key(vm_id, process_id);
        if let std::collections::btree_map::Entry::Vacant(entry) = queued.entry(key) {
            record_execute_response_to_exit_milestone(
                "execute_response_to_exit_event_queued",
                vm_id,
                process_id,
            );
            entry.insert(Instant::now());
        }
    }
}

pub(crate) fn record_execute_exit_event_queue_wait(stage: &str, vm_id: &str, process_id: &str) {
    if !execute_phases_enabled() {
        return;
    }
    let Some(queued) = EXECUTE_EXIT_EVENT_QUEUED.get() else {
        return;
    };
    let Ok(mut queued) = queued.lock() else {
        return;
    };
    if let Some(started) = queued.remove(&execute_phase_key(vm_id, process_id)) {
        record_execute_phase(stage, started.elapsed());
    }
}

pub(crate) fn record_execute_response_to_exit_milestone(
    stage: &str,
    vm_id: &str,
    process_id: &str,
) {
    if !execute_phases_enabled() {
        return;
    }
    let Some(lifetimes) = EXECUTE_LIFETIMES.get() else {
        return;
    };
    let Ok(lifetimes) = lifetimes.lock() else {
        return;
    };
    if let Some(started) = lifetimes.get(&execute_phase_key(vm_id, process_id)) {
        record_execute_phase(stage, started.elapsed());
    }
}

fn record_execute_response_to_exit(vm_id: &str, process_id: &str) {
    if !execute_phases_enabled() {
        return;
    }
    let Some(lifetimes) = EXECUTE_LIFETIMES.get() else {
        return;
    };
    let Ok(mut lifetimes) = lifetimes.lock() else {
        return;
    };
    if let Some(started) = lifetimes.remove(&execute_phase_key(vm_id, process_id)) {
        record_execute_phase("execute_response_to_exit_event", started.elapsed());
    }
}

pub(super) fn sync_rpc_trace_enabled() -> bool {
    std::env::var("AGENTOS_SYNC_RPC_TRACE").as_deref() == Ok("1")
}

pub(super) fn record_sync_rpc(method: &str) {
    let stats =
        SYNC_RPC_STATS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let Ok(mut map) = stats.lock() else {
        return;
    };
    *map.entry(method.to_string()).or_insert(0) += 1;
    let total: u64 = map.values().sum();
    if total == 1 || total.is_multiple_of(50) {
        let mut top: Vec<(&String, &u64)> = map.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        let breakdown = top
            .iter()
            .take(8)
            .map(|(m, c)| format!("{m}={c}"))
            .collect::<Vec<_>>()
            .join(" ");
        tracing::info!(target: "agentos_native_sidecar::perf", total, %breakdown, "sync_rpc count");
    }
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    /// Move a thread-safe runtime completion back into the exact LocalSet-owned
    /// process queue. Public stdout/stderr/exit envelopes continue to the
    /// broker unchanged.
    fn route_received_internal_process_event(
        &mut self,
        envelope: ProcessEventEnvelope,
    ) -> Result<Option<ProcessEventEnvelope>, SidecarError> {
        if !Self::internal_execution_event(&envelope.event) {
            return Ok(Some(envelope));
        }
        self.validate_process_event_envelope_locator(&envelope)?;
        let target_label = if envelope.child_path.is_empty() {
            envelope.process_id.clone()
        } else {
            format!("{}/{}", envelope.process_id, envelope.child_path.join("/"))
        };
        let Some(mut vm) = self.vms.get_mut(&envelope.vm_id) else {
            tracing::debug!(
                vm_id = envelope.vm_id,
                process_id = target_label,
                "ERR_AGENTOS_STALE_PROCESS_EVENT: runtime completion targeted a disposed VM"
            );
            return Ok(None);
        };
        if vm.connection_id != envelope.connection_id || vm.session_id != envelope.session_id {
            return Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_PROCESS_EVENT_SCOPE_MISMATCH: runtime completion for VM {} carried connection/session {}/{}, expected {}/{}",
                envelope.vm_id,
                envelope.connection_id,
                envelope.session_id,
                vm.connection_id,
                vm.session_id
            )));
        }
        let Some(root) = vm.active_processes.get_mut(&envelope.process_id) else {
            tracing::debug!(
                vm_id = envelope.vm_id,
                process_id = target_label,
                "ERR_AGENTOS_STALE_PROCESS_EVENT: runtime completion targeted a reaped root process"
            );
            return Ok(None);
        };
        let Some(process) = Self::active_process_by_owned_path_mut(root, &envelope.child_path)
        else {
            tracing::debug!(
                vm_id = envelope.vm_id,
                process_id = target_label,
                "ERR_AGENTOS_STALE_PROCESS_EVENT: runtime completion targeted a reaped descendant"
            );
            return Ok(None);
        };
        let python_failure = match &envelope.event {
            ActiveExecutionEvent::PythonVfsRpcRequest(request) => {
                Some((request.id, process.execution.python_vfs_rpc_responder()?))
            }
            ActiveExecutionEvent::PythonSocketConnectCompletion(completion) => Some((
                completion.request_id,
                process.execution.python_vfs_rpc_responder()?,
            )),
            _ => None,
        };
        match process.try_queue_pending_execution_envelope(envelope) {
            Ok(()) => Ok(None),
            Err((error, _envelope)) => {
                if let Some((request_id, responder)) = python_failure {
                    if let Err(reply_error) = respond_owned_python_rpc(
                        &responder,
                        request_id,
                        Err(SidecarError::InvalidState(error.to_string())),
                    ) {
                        tracing::debug!(
                            request_id,
                            %reply_error,
                            "Python runtime completion failure reply was no longer pending"
                        );
                    }
                }
                Err(error)
            }
        }
    }

    /// Transfer channel events one at a time so an admission error cannot drop
    /// later envelopes that were already removed from the bounded channel.
    fn drain_runtime_process_event_channel_nowait(&mut self) -> Result<bool, SidecarError> {
        let transfer_limit = self.config.runtime.protocol.max_process_events.max(1);
        let mut transferred = 0usize;
        while transferred < transfer_limit {
            let envelope = if let Some(envelope) = self.deferred_process_event_envelope.take() {
                self.observe_pending_process_event_depth();
                envelope
            } else {
                if self.pending_process_event_capacity() == 0 {
                    break;
                }
                let next = {
                    let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "process event receiver unavailable",
                        ))
                    })?;
                    receiver.try_recv()
                };
                match next {
                    Ok(envelope) => envelope,
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                }
            };
            transferred = transferred.saturating_add(1);
            if let Some(envelope) = self.route_received_internal_process_event(envelope)? {
                self.validate_process_event_envelope_locator(&envelope)?;
                let event_byte_limit = self
                    .vms
                    .get(&envelope.vm_id)
                    .map(|vm| vm.limits.process.pending_event_bytes)
                    .unwrap_or(
                        agentos_native_sidecar_core::limits::DEFAULT_PROCESS_PENDING_EVENT_BYTES,
                    );
                if envelope.retained_bytes() > event_byte_limit {
                    return Err(SidecarError::InvalidState(format!(
                        "ERR_AGENTOS_PROCESS_EVENT_BYTES_LIMIT: process event for VM {} retains {} bytes, exceeding limits.process.pendingEventBytes ({event_byte_limit}); raise limits.process.pendingEventBytes",
                        envelope.vm_id,
                        envelope.retained_bytes()
                    )));
                }
                if let Err((error, envelope)) = self.try_queue_pending_process_event(envelope) {
                    debug_assert!(self.deferred_process_event_envelope.is_none());
                    self.deferred_process_event_envelope = Some(envelope);
                    self.observe_pending_process_event_depth();
                    tracing::debug!(
                        %error,
                        "process-event receiver paused at temporary public-queue capacity"
                    );
                    break;
                }
            }
        }
        let has_more = self
            .process_event_receiver
            .as_ref()
            .is_some_and(|receiver| !receiver.is_empty());
        if has_more && self.deferred_process_event_envelope.is_none() {
            self.process_event_notify.notify_one();
        }
        Ok(transferred > 0)
    }

    /// Perform one bounded, non-suspending process-event turn. Runtime-owned
    /// queues remain durable; this command only transfers events that are
    /// already ready and never waits for a producer.
    pub(crate) fn pump_process_events_nowait(
        &mut self,
        ownership: &OwnershipScope,
        max_service_claims: usize,
    ) -> Result<ProcessEventPumpTurn, SidecarError> {
        let mut emitted_any = false;
        let mut javascript_services = Vec::new();
        let mut python_services = Vec::new();
        let mut python_socket_completions = Vec::new();
        let mut child_bridge_services = Vec::new();
        let mut root_source_remains = false;
        self.expire_public_execution_deadlines()?;

        if self.drain_runtime_process_event_channel_nowait()? {
            emitted_any = true;
        }

        for vm_id in self.vm_ids_for_scope(ownership)? {
            let work_limit = self.config.runtime.fairness.vm_quantum_operations;
            let Some((connection_id, session_id, process_ids)) = self.vms.get(&vm_id).map(|vm| {
                vm.kernel.reap_due_zombies();
                (
                    vm.connection_id.clone(),
                    vm.session_id.clone(),
                    vm.active_processes.keys().cloned().collect::<Vec<_>>(),
                )
            }) else {
                continue;
            };
            let mut work = 0usize;
            for process_id in process_ids {
                if javascript_services
                    .len()
                    .saturating_add(python_services.len())
                    .saturating_add(python_socket_completions.len())
                    .saturating_add(child_bridge_services.len())
                    >= max_service_claims
                {
                    self.process_event_notify.notify_one();
                    break;
                }
                if work >= work_limit {
                    self.process_event_notify.notify_one();
                    break;
                }
                if self
                    .vms
                    .get(&vm_id)
                    .is_some_and(|vm| vm.detached_child_processes.contains(&process_id))
                {
                    continue;
                }
                enum PollResult {
                    Event(Option<PolledExecutionEvent>),
                    RecoverClosed,
                }
                let polled = {
                    let Some(mut vm) = self.vms.get_mut(&vm_id) else {
                        continue;
                    };
                    let Some(process) = vm.active_processes.get_mut(&process_id) else {
                        continue;
                    };
                    if let Some(event) = process.lease_pending_execution_event() {
                        PollResult::Event(Some(event))
                    } else {
                        match process.try_poll_execution_event() {
                            Ok(event) => PollResult::Event(event),
                            Err(SidecarError::Execution(message))
                                if (process.runtime == GuestRuntimeKind::JavaScript
                                    && closed_javascript_event_channel(&message))
                                    || (process.runtime == GuestRuntimeKind::Python
                                        && closed_python_event_channel(&message))
                                    || (process.runtime == GuestRuntimeKind::WebAssembly
                                        && closed_wasm_event_channel(&message)) =>
                            {
                                PollResult::RecoverClosed
                            }
                            Err(error) => return Err(error),
                        }
                    }
                };
                let event = match polled {
                    PollResult::Event(event) => event,
                    PollResult::RecoverClosed => self
                        .recover_closed_root_runtime_process_event(&vm_id, &process_id)?
                        .map(PolledExecutionEvent::unreserved),
                };
                let Some(event) = event else { continue };
                root_source_remains |= self.vms.get(&vm_id).is_some_and(|vm| {
                    vm.active_processes.get(&process_id).is_some_and(|process| {
                        !process.pending_execution_events.is_empty()
                            || process.execution.has_pending_events()
                    })
                });
                if Self::internal_execution_event(event.event()) {
                    let PolledExecutionEvent { event, reservation } = event;
                    match event {
                        ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
                            if let Some(vm) = self.vms.handle(&vm_id) {
                                javascript_services.push(OwnedJavascriptEventService::new(
                                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                                    vm_id.clone(),
                                    process_id,
                                    Vec::new(),
                                    vm,
                                    request,
                                    reservation,
                                ));
                            }
                        }
                        ActiveExecutionEvent::JavascriptSyncRpcCompletion(completion) => {
                            self.handle_javascript_sync_rpc_completion(
                                &vm_id,
                                &process_id,
                                completion,
                            )?;
                            drop(reservation);
                        }
                        ActiveExecutionEvent::PythonSocketConnectCompletion(completion) => {
                            let Some(vm) = self.vms.handle(&vm_id) else {
                                continue;
                            };
                            let responder =
                                vm.try_read("claim Python socket completion service", |state| {
                                    state
                                        .active_processes
                                        .get(&process_id)
                                        .map(|process| process.execution.python_vfs_rpc_responder())
                                })?;
                            let Some(responder) = responder else {
                                continue;
                            };
                            python_socket_completions.push(
                                OwnedPythonSocketCompletionService::new(
                                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                                    vm_id.clone(),
                                    process_id,
                                    Vec::new(),
                                    vm,
                                    responder?,
                                    *completion,
                                    reservation,
                                ),
                            );
                        }
                        ActiveExecutionEvent::SignalState {
                            signal,
                            registration,
                        } => {
                            if let Some(mut vm) = self.vms.get_mut(&vm_id) {
                                vm.signal_states
                                    .entry(process_id)
                                    .or_default()
                                    .insert(signal, registration);
                            }
                            drop(reservation);
                        }
                        ActiveExecutionEvent::PythonVfsRpcRequest(request) => {
                            let Some(vm) = self.vms.handle(&vm_id) else {
                                continue;
                            };
                            let responder =
                                vm.try_read("claim Python process-event service", |state| {
                                    state
                                        .active_processes
                                        .get(&process_id)
                                        .map(|process| process.execution.python_vfs_rpc_responder())
                                })?;
                            let Some(responder) = responder else {
                                continue;
                            };
                            python_services.push(OwnedPythonEventService::new(
                                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                                vm_id.clone(),
                                process_id,
                                Vec::new(),
                                vm,
                                responder?,
                                *request,
                                reservation,
                            ));
                        }
                        _ => unreachable!("internal event classification changed"),
                    }
                    emitted_any = true;
                    work = work.saturating_add(1);
                    continue;
                }
                let PolledExecutionEvent { event, reservation } = event;
                let envelope = ProcessEventEnvelope {
                    connection_id: connection_id.clone(),
                    session_id: session_id.clone(),
                    vm_id: vm_id.clone(),
                    child_path: Vec::new(),
                    process_id,
                    event,
                };
                if let Err(error) = self.check_pending_process_event_capacity(&envelope) {
                    let Some(mut vm) = self.vms.get_mut(&vm_id) else {
                        return Err(error);
                    };
                    if let Some(process) = vm.active_processes.get_mut(&envelope.process_id) {
                        process.requeue_pending_execution_event(PolledExecutionEvent {
                            event: envelope.event,
                            reservation,
                        })?;
                    }
                    return Err(error);
                }
                self.queue_pending_process_event(envelope)?;
                drop(reservation);
                emitted_any = true;
                work = work.saturating_add(1);
            }
            if self.pump_child_process_events_nowait(
                &vm_id,
                &mut javascript_services,
                &mut python_services,
                &mut python_socket_completions,
                &mut child_bridge_services,
                max_service_claims,
            )? {
                emitted_any = true;
            }
            if self.pump_detached_child_process_events_nowait(
                &vm_id,
                &mut javascript_services,
                &mut python_services,
                &mut python_socket_completions,
                &mut child_bridge_services,
                max_service_claims,
            )? {
                emitted_any = true;
            }
        }
        if self.route_claimed_pending_process_events()? > 0 {
            emitted_any = true;
        }
        let service_claims = javascript_services
            .len()
            .saturating_add(python_services.len())
            .saturating_add(python_socket_completions.len())
            .saturating_add(child_bridge_services.len());
        if max_service_claims > 0 && service_claims >= max_service_claims {
            // `Notify` coalesces producer edges. If this turn consumes the one
            // stored permit while filling the owned-service staging capacity,
            // source queues may still contain work but no producer will emit a
            // second edge. Preserve one continuation permit; an empty follow-up
            // turn does not rearm and therefore cannot hot-spin.
            self.process_event_notify.notify_one();
        }
        if root_source_remains {
            // A root process is probed once per bounded coordinator turn. Its
            // runtime producer uses a coalesced Notify, so one producer edge
            // can represent an arbitrary number of already-durable stdout,
            // stderr, exit, or internal events. Preserve one continuation
            // edge whenever the non-consuming post-claim probe still observes
            // source work. An exactly drained turn does not rearm and therefore
            // cannot hot-spin.
            self.process_event_notify.notify_one();
        }
        self.rearm_kernel_reaper_task()?;
        Ok(ProcessEventPumpTurn {
            emitted_any,
            javascript_services,
            python_services,
            python_socket_completions,
            child_bridge_services,
        })
    }

    /// Apply one already-polled public process event without suspending the
    /// protocol coordinator. Internal RPC events are never valid broker
    /// payloads; they stay on the owned VM event-service path.
    pub(crate) fn handle_public_execution_event_nowait(
        &mut self,
        vm_id: &str,
        process_id: &str,
        event: ActiveExecutionEvent,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let Some((connection_id, session_id, active)) = self.vms.get(vm_id).map(|vm| {
            (
                vm.connection_id.clone(),
                vm.session_id.clone(),
                vm.active_processes.contains_key(process_id),
            )
        }) else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "public event dispatch");
            return Ok(None);
        };
        if !active {
            log_stale_process_event(&self.bridge, vm_id, process_id, "public event dispatch");
            return Ok(None);
        }
        let ownership = OwnershipScope::vm(&connection_id, &session_id, vm_id);
        let public_execution = self.is_public_execution_process(vm_id, process_id);

        if self.capture_extension_process_output_event(vm_id, process_id, &event) {
            return Ok(None);
        }

        match event {
            ActiveExecutionEvent::Stdout(chunk) if public_execution => Ok(self
                .record_public_execution_output(
                    vm_id,
                    process_id,
                    ExecutionStreamChannel::Stdout,
                    chunk,
                )
                .map(|payload| EventFrame::new(ownership, payload))),
            ActiveExecutionEvent::Stderr(chunk) if public_execution => Ok(self
                .record_public_execution_output(
                    vm_id,
                    process_id,
                    ExecutionStreamChannel::Stderr,
                    chunk,
                )
                .map(|payload| EventFrame::new(ownership, payload))),
            ActiveExecutionEvent::Stdout(chunk) => Ok(Some(EventFrame::new(
                ownership,
                EventPayload::ProcessOutput(ProcessOutputEvent {
                    process_id: process_id.to_owned(),
                    channel: StreamChannel::Stdout,
                    chunk,
                }),
            ))),
            ActiveExecutionEvent::Stderr(chunk) => Ok(Some(EventFrame::new(
                ownership,
                EventPayload::ProcessOutput(ProcessOutputEvent {
                    process_id: process_id.to_owned(),
                    channel: StreamChannel::Stderr,
                    chunk,
                }),
            ))),
            ActiveExecutionEvent::Exited(exit_code) => {
                record_execute_response_to_exit_milestone(
                    "execute_response_to_exit_event_handle",
                    vm_id,
                    process_id,
                );
                record_execute_response_to_exit(vm_id, process_id);
                let park_resident = public_execution
                    && self.should_park_public_execution_process(vm_id, process_id);
                let became_idle = if park_resident {
                    false
                } else {
                    self.finish_active_process_exit(vm_id, process_id, exit_code)?
                        .unwrap_or(false)
                };
                if became_idle || (park_resident && !self.has_running_nonresident_processes(vm_id))
                {
                    self.bridge.emit_lifecycle(vm_id, LifecycleState::Ready)?;
                }
                if public_execution {
                    Ok(self
                        .complete_public_execution(vm_id, process_id, exit_code)
                        .map(|payload| EventFrame::new(ownership, payload)))
                } else {
                    Ok(Some(EventFrame::new(
                        ownership,
                        EventPayload::ProcessExited(ProcessExitedEvent {
                            process_id: process_id.to_owned(),
                            exit_code,
                        }),
                    )))
                }
            }
            other => Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_INTERNAL_EVENT_ON_PUBLIC_BROKER: process {process_id} produced internal event {other:?}"
            ))),
        }
    }

    pub async fn pump_process_events(
        &mut self,
        ownership: &OwnershipScope,
    ) -> Result<bool, SidecarError> {
        let mut emitted_any = false;
        self.expire_public_execution_deadlines()?;

        if self.drain_runtime_process_event_channel_nowait()? {
            emitted_any = true;
        }

        let vm_ids = self.vm_ids_for_scope(ownership)?;
        for vm_id in vm_ids {
            let vm_work_limit = self.config.runtime.fairness.vm_quantum_operations;
            let mut vm_work = 0usize;
            if let Some(vm) = self.vms.get(&vm_id) {
                vm.kernel.reap_due_zombies();
            }
            'vm_event_turn: while self.vms.contains_key(&vm_id) {
                let Some((connection_id, session_id, process_ids)) =
                    self.vms.get(&vm_id).map(|vm| {
                        (
                            vm.connection_id.clone(),
                            vm.session_id.clone(),
                            vm.active_processes.keys().cloned().collect::<Vec<_>>(),
                        )
                    })
                else {
                    break;
                };
                let mut emitted_this_pass = false;

                for process_id in process_ids {
                    if vm_work >= vm_work_limit {
                        self.process_event_notify.notify_one();
                        break 'vm_event_turn;
                    }
                    if self
                        .vms
                        .get(&vm_id)
                        .is_some_and(|vm| vm.detached_child_processes.contains(&process_id))
                    {
                        continue;
                    }
                    enum ProcessPollResult {
                        Event(Box<Option<PolledExecutionEvent>>),
                        RecoverClosedChannel,
                    }
                    let poll_result = {
                        let Some(mut vm) = self.vms.get_mut(&vm_id) else {
                            continue;
                        };
                        let Some(process) = vm.active_processes.get_mut(&process_id) else {
                            continue;
                        };
                        if let Some(event) = process.lease_pending_execution_event() {
                            ProcessPollResult::Event(Box::new(Some(event)))
                        } else {
                            match process.poll_execution_event(Duration::ZERO).await {
                                Ok(event) => ProcessPollResult::Event(Box::new(event)),
                                Err(SidecarError::Execution(message))
                                    if (process.runtime == GuestRuntimeKind::JavaScript
                                        && closed_javascript_event_channel(&message))
                                        || (process.runtime == GuestRuntimeKind::Python
                                            && closed_python_event_channel(&message))
                                        || (process.runtime == GuestRuntimeKind::WebAssembly
                                            && closed_wasm_event_channel(&message)) =>
                                {
                                    ProcessPollResult::RecoverClosedChannel
                                }
                                Err(other) => return Err(other),
                            }
                        }
                    };
                    let event = match poll_result {
                        ProcessPollResult::Event(event) => *event,
                        ProcessPollResult::RecoverClosedChannel => self
                            .recover_closed_root_runtime_process_event(&vm_id, &process_id)?
                            .map(PolledExecutionEvent::unreserved),
                    };

                    let Some(event) = event else {
                        continue;
                    };
                    if matches!(event.event(), ActiveExecutionEvent::Exited(_)) {
                        record_execute_response_to_exit_milestone(
                            "execute_response_to_exit_event_polled",
                            &vm_id,
                            &process_id,
                        );
                    }

                    if Self::internal_execution_event(event.event()) {
                        // These events are sidecar work items, not client-facing
                        // process events. Handle them immediately so a sibling
                        // process can service sync RPCs while another request
                        // waits on VM-local networking.
                        self.handle_execution_event(&vm_id, &process_id, event.into_event())
                            .await?;
                    } else {
                        let PolledExecutionEvent { event, reservation } = event;
                        let envelope = ProcessEventEnvelope {
                            connection_id: connection_id.clone(),
                            session_id: session_id.clone(),
                            vm_id: vm_id.clone(),
                            child_path: Vec::new(),
                            process_id: process_id.clone(),
                            event,
                        };
                        if let Err(error) = self.check_pending_process_event_capacity(&envelope) {
                            if let Some(mut vm) = self.vms.get_mut(&vm_id) {
                                if let Some(process) = vm.active_processes.get_mut(&process_id) {
                                    process.requeue_pending_execution_event(
                                        PolledExecutionEvent {
                                            event: envelope.event,
                                            reservation,
                                        },
                                    )?;
                                }
                            }
                            return Err(error);
                        }
                        self.queue_pending_process_event(envelope)?;
                        drop(reservation);
                    }
                    emitted_any = true;
                    emitted_this_pass = true;
                    vm_work += 1;
                }

                if !emitted_this_pass {
                    break;
                }
            }

            if self.pump_child_process_events(&vm_id).await? {
                emitted_any = true;
            }
            if self.pump_detached_child_process_events(&vm_id).await? {
                emitted_any = true;
            }
        }

        if self.route_claimed_pending_process_events()? > 0 {
            emitted_any = true;
        }
        self.rearm_kernel_reaper_task()?;
        Ok(emitted_any)
    }

    /// Arm exactly one sidecar task for the earliest zombie deadline across
    /// every VM. Kernel process tables remain runtime-neutral and are reaped on
    /// the next process-event turn after this coalesced wake.
    fn rearm_kernel_reaper_task(&mut self) -> Result<(), SidecarError> {
        if self
            .kernel_reaper_task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            self.kernel_reaper_task.take();
            self.kernel_reaper_deadline = None;
        }
        let next_deadline = self
            .vms
            .values()
            .filter_map(|vm| vm.kernel.next_zombie_reap_deadline())
            .min();
        let Some(next_deadline) = next_deadline else {
            if let Some(task) = self.kernel_reaper_task.take() {
                task.abort();
            }
            self.kernel_reaper_deadline = None;
            return Ok(());
        };
        if self.kernel_reaper_task.is_some()
            && self
                .kernel_reaper_deadline
                .is_some_and(|armed_deadline| armed_deadline <= next_deadline)
        {
            return Ok(());
        }
        if let Some(task) = self.kernel_reaper_task.take() {
            task.abort();
        }
        let runtime = self.runtime_context.clone().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_RUNTIME_UNAVAILABLE: kernel zombie reaper requires the process RuntimeContext",
            ))
        })?;
        let notify = Arc::clone(&self.process_event_notify);
        let delay = next_deadline.saturating_duration_since(Instant::now());
        self.kernel_reaper_task = Some(
            runtime
                .spawn(agentos_runtime::TaskClass::Timer, async move {
                    tokio::time::sleep(delay).await;
                    notify.notify_one();
                })
                .map_err(|error| SidecarError::Execution(error.to_string()))?,
        );
        self.kernel_reaper_deadline = Some(next_deadline);
        Ok(())
    }

    fn internal_execution_event(event: &ActiveExecutionEvent) -> bool {
        matches!(
            event,
            ActiveExecutionEvent::JavascriptSyncRpcRequest(_)
                | ActiveExecutionEvent::JavascriptSyncRpcCompletion(_)
                | ActiveExecutionEvent::PythonVfsRpcRequest(_)
                | ActiveExecutionEvent::PythonSocketConnectCompletion(_)
                | ActiveExecutionEvent::SignalState { .. }
        )
    }

    pub(super) fn recover_closed_root_runtime_process_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Ok(None);
        };
        let Some(process) = vm.active_processes.get_mut(process_id) else {
            return Ok(None);
        };
        if process.execution.uses_shared_v8_runtime() {
            return Ok(None);
        }
        if process.runtime != GuestRuntimeKind::JavaScript
            && process.runtime != GuestRuntimeKind::Python
            && process.runtime != GuestRuntimeKind::WebAssembly
        {
            return Ok(None);
        }
        let runtime_child_pid = process.execution.child_pid();
        if runtime_child_pid == 0 {
            return Ok(None);
        }
        match runtime_child_exit_status(runtime_child_pid)? {
            RuntimeChildStatusObservation::Exited(status) => {
                process.exit_signal = status.signal;
                process.exit_core_dumped = status.core_dumped;
                Ok(Some(ActiveExecutionEvent::Exited(status.status)))
            }
            RuntimeChildStatusObservation::Running => Ok(None),
            RuntimeChildStatusObservation::NotWaitable => Err(SidecarError::Execution(format!(
                "ECHILD: guest runtime process {runtime_child_pid} exited without an observable wait status"
            ))),
        }
    }

    pub(crate) fn active_process_by_path<'a>(
        process: &'a ActiveProcess,
        child_path: &[&str],
    ) -> Option<&'a ActiveProcess> {
        let mut current = process;
        for child_id in child_path {
            current = current.child_processes.get(*child_id)?;
        }
        Some(current)
    }

    pub(crate) fn active_process_by_path_mut<'a>(
        process: &'a mut ActiveProcess,
        child_path: &[&str],
    ) -> Option<&'a mut ActiveProcess> {
        let mut current = process;
        for child_id in child_path {
            current = current.child_processes.get_mut(*child_id)?;
        }
        Some(current)
    }

    pub(super) fn active_process_by_owned_path_mut<'a>(
        process: &'a mut ActiveProcess,
        child_path: &[String],
    ) -> Option<&'a mut ActiveProcess> {
        let mut current = process;
        for child_id in child_path {
            current = current.child_processes.get_mut(child_id)?;
        }
        Some(current)
    }

    pub(super) fn active_process_path_by_kernel_pid(
        process: &ActiveProcess,
        kernel_pid: u32,
    ) -> Option<Vec<String>> {
        if process.kernel_pid == kernel_pid {
            return Some(Vec::new());
        }

        for (child_id, child) in &process.child_processes {
            let Some(mut path) = Self::active_process_path_by_kernel_pid(child, kernel_pid) else {
                continue;
            };
            path.insert(0, child_id.clone());
            return Some(path);
        }

        None
    }

    pub(super) fn descendant_parent_process<'a>(
        vm: &'a VmState,
        process_id: &str,
        child_path: &[&str],
    ) -> Option<&'a ActiveProcess> {
        let root = vm.active_processes.get(process_id)?;
        Self::active_process_by_path(root, child_path)
    }

    pub(super) fn descendant_parent_process_mut<'a>(
        vm: &'a mut VmState,
        process_id: &str,
        child_path: &[&str],
    ) -> Option<&'a mut ActiveProcess> {
        let root = vm.active_processes.get_mut(process_id)?;
        Self::active_process_by_path_mut(root, child_path)
    }

    pub(super) fn child_process_path_label(process_id: &str, child_path: &[&str]) -> String {
        if child_path.is_empty() {
            process_id.to_owned()
        } else {
            format!("{process_id}/{}", child_path.join("/"))
        }
    }

    pub(super) fn adopt_detached_child_processes(
        current_process_id: &str,
        process: &mut ActiveProcess,
    ) -> Vec<(String, ActiveProcess)> {
        let mut adopted = Vec::new();
        let child_ids = process.child_processes.keys().cloned().collect::<Vec<_>>();
        for child_id in child_ids {
            let child_process_id = format!("{current_process_id}/{child_id}");
            let Some(mut child) = process.child_processes.remove(&child_id) else {
                continue;
            };
            if child.detached {
                adopted.push((child_process_id, child));
                continue;
            }

            adopted.extend(Self::adopt_detached_child_processes(
                &child_process_id,
                &mut child,
            ));
            process.child_processes.insert(child_id, child);
        }
        adopted
    }

    pub(super) fn child_process_signal_key<'a>(
        process_id: &'a str,
        child_path: &[&'a str],
    ) -> &'a str {
        child_path.last().copied().unwrap_or(process_id)
    }

    pub(super) fn resolve_detached_child_process_path(
        vm: &VmState,
        detached_process_id: &str,
    ) -> Option<(String, Vec<String>)> {
        let root_process_id = vm
            .active_processes
            .keys()
            .filter(|candidate| {
                detached_process_id == candidate.as_str()
                    || detached_process_id
                        .strip_prefix(candidate.as_str())
                        .is_some_and(|remainder| remainder.starts_with('/'))
            })
            .max_by_key(|candidate| candidate.len())?
            .clone();

        let remainder = detached_process_id
            .strip_prefix(root_process_id.as_str())
            .unwrap_or_default();
        if remainder.is_empty() {
            return Some((root_process_id, Vec::new()));
        }

        Some((
            root_process_id,
            remainder
                .trim_start_matches('/')
                .split('/')
                .map(str::to_owned)
                .collect(),
        ))
    }

    pub(super) fn collect_attached_child_paths(
        process: &ActiveProcess,
        parent_path: &mut Vec<String>,
        paths: &mut Vec<Vec<String>>,
    ) {
        for (child_id, child) in &process.child_processes {
            // `detached` changes the child's process-group/session and lets it
            // survive its parent. Until the parent exits and adopts it into
            // `detached_child_processes`, it still lives in this tree and its
            // stdio, sync RPCs, and descendants must be pumped here.
            parent_path.push(child_id.clone());
            paths.push(parent_path.clone());
            Self::collect_attached_child_paths(child, parent_path, paths);
            parent_path.pop();
        }
    }

    /// Drain attached child runtimes from the same coalesced process wake used
    /// by top-level executions. Event data stays in runtime-owned bounded
    /// queues; this turn merely routes a bounded batch into the parent VM.
    pub(crate) async fn handle_execution_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
        event: ActiveExecutionEvent,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let Some((connection_id, session_id, active)) = self.vms.get(vm_id).map(|vm| {
            (
                vm.connection_id.clone(),
                vm.session_id.clone(),
                vm.active_processes.contains_key(process_id),
            )
        }) else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "execution event dispatch");
            return Ok(None);
        };
        if !active {
            log_stale_process_event(&self.bridge, vm_id, process_id, "execution event dispatch");
            return Ok(None);
        }
        let ownership = OwnershipScope::vm(&connection_id, &session_id, vm_id);
        let public_execution = self.is_public_execution_process(vm_id, process_id);

        if self.capture_extension_process_output_event(vm_id, process_id, &event) {
            return Ok(None);
        }

        match event {
            ActiveExecutionEvent::Stdout(chunk) if public_execution => Ok(self
                .record_public_execution_output(
                    vm_id,
                    process_id,
                    ExecutionStreamChannel::Stdout,
                    chunk,
                )
                .map(|payload| EventFrame::new(ownership, payload))),
            ActiveExecutionEvent::Stderr(chunk) if public_execution => Ok(self
                .record_public_execution_output(
                    vm_id,
                    process_id,
                    ExecutionStreamChannel::Stderr,
                    chunk,
                )
                .map(|payload| EventFrame::new(ownership, payload))),
            ActiveExecutionEvent::Stdout(chunk) => Ok(Some(EventFrame::new(
                ownership,
                EventPayload::ProcessOutput(ProcessOutputEvent {
                    process_id: process_id.to_owned(),
                    channel: StreamChannel::Stdout,
                    chunk,
                }),
            ))),
            ActiveExecutionEvent::Stderr(chunk) => Ok(Some(EventFrame::new(
                ownership,
                EventPayload::ProcessOutput(ProcessOutputEvent {
                    process_id: process_id.to_owned(),
                    channel: StreamChannel::Stderr,
                    chunk,
                }),
            ))),
            ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
                self.handle_javascript_sync_rpc_request(vm_id, process_id, request)
                    .await?;
                Ok(None)
            }
            ActiveExecutionEvent::JavascriptSyncRpcCompletion(completion) => {
                self.handle_javascript_sync_rpc_completion(vm_id, process_id, completion)?;
                Ok(None)
            }
            ActiveExecutionEvent::PythonVfsRpcRequest(request) => {
                self.handle_python_vfs_rpc_request(vm_id, process_id, *request)
                    .await?;
                Ok(None)
            }
            ActiveExecutionEvent::PythonSocketConnectCompletion(completion) => {
                self.handle_python_socket_connect_completion(vm_id, process_id, *completion)?;
                Ok(None)
            }
            ActiveExecutionEvent::SignalState {
                signal,
                registration,
            } => {
                let Some(mut vm) = self.vms.get_mut(vm_id) else {
                    return Ok(None);
                };
                if !vm.active_processes.contains_key(process_id) {
                    return Ok(None);
                }
                vm.signal_states
                    .entry(process_id.to_owned())
                    .or_default()
                    .insert(signal, registration);
                Ok(None)
            }
            ActiveExecutionEvent::Exited(exit_code) => {
                record_execute_response_to_exit_milestone(
                    "execute_response_to_exit_event_handle",
                    vm_id,
                    process_id,
                );
                record_execute_response_to_exit(vm_id, process_id);
                let park_resident = public_execution
                    && self.should_park_public_execution_process(vm_id, process_id);
                let phase_start = Instant::now();
                let became_idle = if park_resident {
                    false
                } else {
                    self.finish_active_process_exit(vm_id, process_id, exit_code)?
                        .unwrap_or(false)
                };
                record_execute_phase("process_exit_cleanup", phase_start.elapsed());

                let phase_start = Instant::now();
                if became_idle || (park_resident && !self.has_running_nonresident_processes(vm_id))
                {
                    self.bridge.emit_lifecycle(vm_id, LifecycleState::Ready)?;
                }
                record_execute_phase("process_exit_lifecycle_emit", phase_start.elapsed());

                if public_execution {
                    Ok(self
                        .complete_public_execution(vm_id, process_id, exit_code)
                        .map(|payload| EventFrame::new(ownership, payload)))
                } else {
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
    }

    pub(super) fn handle_javascript_sync_rpc_completion(
        &mut self,
        vm_id: &str,
        process_id: &str,
        completion: crate::state::JavascriptSyncRpcCompletion,
    ) -> Result<(), SidecarError> {
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let Some(process) = vm.active_processes.get_mut(process_id) else {
            return Ok(());
        };
        settle_javascript_sync_rpc_completion(
            process,
            &kernel_readiness,
            completion.request_id,
            completion.result,
        )
    }

    pub(super) fn handle_python_socket_connect_completion(
        &mut self,
        vm_id: &str,
        process_id: &str,
        completion: PythonSocketConnectCompletion,
    ) -> Result<(), SidecarError> {
        let request_id = completion.request_id;
        let connected = match completion.result {
            Ok(connected) => connected,
            Err(error) => {
                return self.respond_python_rpc(
                    vm_id,
                    process_id,
                    request_id,
                    Err(SidecarError::Execution(format!(
                        "{}: {}",
                        error.code, error.message
                    ))),
                );
            }
        };
        let result = {
            let Some(mut vm) = self.vms.get_mut(vm_id) else {
                return Ok(());
            };
            let vm = &mut *vm;
            let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
            let Some(process) = vm.active_processes.get_mut(process_id) else {
                return Ok(());
            };
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
                if let Err(close_error) = socket.close(&mut vm.kernel, process.kernel_pid) {
                    eprintln!(
                        "ERR_AGENTOS_PYTHON_SOCKET_CLOSE: deferred TCP connect rollback failed: {close_error}"
                    );
                }
                Err(error)
            } else if let Err(error) =
                socket.set_fairness_identity(process.capability_fairness_identity(&capability_key))
            {
                if let Err(release_error) = process.release_capability(&capability_key) {
                    eprintln!(
                        "ERR_AGENTOS_CAPABILITY_RELEASE: deferred Python TCP rollback failed: {release_error}"
                    );
                }
                if let Err(close_error) = socket.close(&mut vm.kernel, process.kernel_pid) {
                    eprintln!(
                        "ERR_AGENTOS_PYTHON_SOCKET_CLOSE: deferred TCP fairness rollback failed: {close_error}"
                    );
                }
                Err(error)
            } else {
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
            }
        };
        self.respond_python_rpc(vm_id, process_id, request_id, result)
    }

    pub(crate) fn finish_active_process_exit_owned(
        bridge: &SharedBridge<B>,
        vm_handle: &crate::state::VmHandle,
        vm_id: &str,
        process_id: &str,
        exit_code: i32,
    ) -> Result<Option<FinishedActiveProcessExit>, SidecarError> {
        let mut vm = vm_handle.try_borrow_mut("finish active process exit")?;
        if !vm.active_processes.contains_key(process_id) {
            log_stale_process_event(bridge, vm_id, process_id, "process exit cleanup");
            return Ok(None);
        }

        let phase_start = Instant::now();
        prune_exited_process_snapshots(&mut vm);
        record_execute_phase(
            "process_exit_cleanup_prune_snapshots",
            phase_start.elapsed(),
        );
        let phase_start = Instant::now();
        let process_table = vm.kernel.list_processes();
        record_execute_phase("process_exit_cleanup_list_processes", phase_start.elapsed());
        let phase_start = Instant::now();
        let Some(mut process) = vm.active_processes.remove(process_id) else {
            return Ok(None);
        };
        record_execute_phase("process_exit_cleanup_remove_active", phase_start.elapsed());
        let phase_start = Instant::now();
        if let Some(info) = process_table.get(&process.kernel_pid) {
            vm.exited_process_snapshots
                .push_back(ExitedProcessSnapshot {
                    captured_at: Instant::now(),
                    process: build_process_snapshot_entry(
                        process_id,
                        &process,
                        info,
                        Some(exit_code),
                    ),
                });
        }
        record_execute_phase("process_exit_cleanup_build_snapshot", phase_start.elapsed());
        let phase_start = Instant::now();
        let detached_children = Self::adopt_detached_child_processes(process_id, &mut process);
        record_execute_phase("process_exit_cleanup_adopt_detached", phase_start.elapsed());
        let phase_start = Instant::now();
        let should_sync_host_writes = process.host_write_dirty_recursive()
            || !process.clean_host_writes_are_observable_recursive();
        let host_sync_result = if should_sync_host_writes {
            sync_process_host_writes_to_kernel(&mut vm, &process)
        } else {
            record_execute_phase(
                "process_exit_cleanup_sync_host_writes_clean_skip",
                Duration::ZERO,
            );
            Ok(())
        };
        record_execute_phase(
            "process_exit_cleanup_sync_host_writes",
            phase_start.elapsed(),
        );
        let raw_mode_result = release_inherited_child_raw_mode(&mut vm.kernel, &process);
        let phase_start = Instant::now();
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let unix_address_registry = Arc::clone(&vm.unix_address_registry);
        terminate_child_process_tree(
            &mut vm.kernel,
            &mut process,
            &kernel_readiness,
            &unix_address_registry,
        );
        record_execute_phase(
            "process_exit_cleanup_terminate_child_tree",
            phase_start.elapsed(),
        );
        let phase_start = Instant::now();
        process.kernel_handle.finish(exit_code);
        record_execute_phase("process_exit_cleanup_kernel_finish", phase_start.elapsed());
        let phase_start = Instant::now();
        let _ = vm.kernel.wait_and_reap(process.kernel_pid);
        record_execute_phase("process_exit_cleanup_wait_and_reap", phase_start.elapsed());
        let phase_start = Instant::now();
        vm.signal_states.remove(process_id);
        record_execute_phase(
            "process_exit_cleanup_signal_state_remove",
            phase_start.elapsed(),
        );
        let phase_start = Instant::now();
        for (detached_process_id, detached_child) in detached_children {
            vm.detached_child_processes
                .insert(detached_process_id.clone());
            vm.active_processes
                .insert(detached_process_id, detached_child);
        }
        record_execute_phase(
            "process_exit_cleanup_reinsert_detached",
            phase_start.elapsed(),
        );
        let phase_start = Instant::now();
        let became_idle = vm.active_processes.is_empty();
        record_execute_phase("process_exit_cleanup_became_idle", phase_start.elapsed());
        drop(vm);

        // The process was removed from active_processes before the fallible
        // host/raw-mode cleanup. Surface those errors only after all process-
        // owned resources (especially host-materialized SQLite state) have
        // been copied back and finalized.
        host_sync_result?;
        raw_mode_result?;
        Ok(Some(FinishedActiveProcessExit {
            became_idle,
            process_id: process_id.to_owned(),
        }))
    }

    pub(crate) fn finish_active_process_exit(
        &mut self,
        vm_id: &str,
        process_id: &str,
        exit_code: i32,
    ) -> Result<Option<bool>, SidecarError> {
        let Some(vm_handle) = self.vms.handle(vm_id) else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "process exit cleanup");
            return Ok(None);
        };
        let finished = Self::finish_active_process_exit_owned(
            &self.bridge,
            &vm_handle,
            vm_id,
            process_id,
            exit_code,
        )?;
        if let Some(finished) = finished {
            let phase_start = Instant::now();
            self.prune_extension_process_resource(&finished.process_id);
            record_execute_phase("process_exit_cleanup_prune_resource", phase_start.elapsed());
            return Ok(Some(finished.became_idle));
        }
        Ok(None)
    }
}

pub(crate) struct FinishedActiveProcessExit {
    pub(crate) became_idle: bool,
    pub(crate) process_id: String,
}

#[cfg(test)]
mod process_event_channel_tests {
    use super::*;
    use crate::stdio::LocalBridge;
    use crate::NativeSidecarConfig;
    use std::future::Future as _;
    use std::task::{Context, Poll, Waker};

    #[test]
    fn receiver_admission_failure_does_not_drop_later_envelopes() {
        let config = NativeSidecarConfig::default();
        let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
            .expect("process-event channel test runtime");
        let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
            LocalBridge::default(),
            config,
            Vec::new(),
            runtime.context(),
        )
        .expect("process-event channel test sidecar");
        sidecar.config.runtime.protocol.max_process_events = 2;
        let envelope = |child_path, byte| ProcessEventEnvelope {
            connection_id: String::from("connection"),
            session_id: String::from("session"),
            vm_id: String::from("vm"),
            child_path,
            process_id: String::from("process"),
            event: ActiveExecutionEvent::Stdout(vec![byte]),
        };
        sidecar
            .process_event_sender
            .try_send(envelope(
                vec![String::from("a"), String::from("b"), String::from("c")],
                1,
            ))
            .expect("queue invalid first envelope");
        sidecar
            .process_event_sender
            .try_send(envelope(Vec::new(), 2))
            .expect("queue valid later envelope");

        let error = sidecar
            .drain_runtime_process_event_channel_nowait()
            .expect_err("invalid locator must fail admission");
        assert!(error
            .to_string()
            .contains("ERR_AGENTOS_PROCESS_EVENT_PATH_LIMIT"));
        assert_eq!(
            sidecar
                .process_event_receiver
                .as_ref()
                .expect("process event receiver")
                .len(),
            1,
            "the later envelope must remain in the bounded channel"
        );
    }

    #[test]
    fn deferred_current_envelope_retries_before_later_channel_envelopes() {
        let config = NativeSidecarConfig::default();
        let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
            .expect("process-event retry-order test runtime");
        let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
            LocalBridge::default(),
            config,
            Vec::new(),
            runtime.context(),
        )
        .expect("process-event retry-order test sidecar");
        let envelope = |byte| ProcessEventEnvelope {
            connection_id: String::from("connection"),
            session_id: String::from("session"),
            vm_id: String::from("vm"),
            child_path: Vec::new(),
            process_id: String::from("process"),
            event: ActiveExecutionEvent::Stdout(vec![byte]),
        };
        sidecar.deferred_process_event_envelope = Some(envelope(1));
        sidecar
            .process_event_sender
            .try_send(envelope(2))
            .expect("queue later envelope");

        assert!(sidecar
            .drain_runtime_process_event_channel_nowait()
            .expect("retry staged and later envelopes"));
        let bytes = sidecar
            .pending_process_events
            .drain(..)
            .map(|envelope| match envelope.event {
                ActiveExecutionEvent::Stdout(bytes) => bytes[0],
                other => panic!("expected stdout, received {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(bytes, vec![1, 2]);
        assert!(sidecar.deferred_process_event_envelope.is_none());
        assert!(sidecar
            .process_event_receiver
            .as_ref()
            .expect("process event receiver")
            .is_empty());
    }

    #[test]
    fn temporary_rejection_stays_open_and_rearms_only_after_capacity_release() {
        let config = NativeSidecarConfig::default();
        let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
            .expect("process-event no-spin test runtime");
        let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
            LocalBridge::default(),
            config,
            Vec::new(),
            runtime.context(),
        )
        .expect("process-event no-spin test sidecar");
        sidecar.config.runtime.protocol.max_process_events = 2;
        let envelope = |byte| ProcessEventEnvelope {
            connection_id: String::from("connection"),
            session_id: String::from("session"),
            vm_id: String::from("vm"),
            child_path: Vec::new(),
            process_id: String::from("process"),
            event: ActiveExecutionEvent::Stdout(vec![byte]),
        };
        sidecar.pending_process_events.push_back(envelope(8));
        sidecar.pending_process_events.push_back(envelope(9));
        sidecar.deferred_process_event_envelope = Some(envelope(1));
        sidecar
            .process_event_sender
            .try_send(envelope(2))
            .expect("queue later envelope");

        assert!(sidecar
            .drain_runtime_process_event_channel_nowait()
            .expect("temporary saturation must not close the protocol"));
        assert!(sidecar.deferred_process_event_envelope.is_some());
        assert_eq!(sidecar.pending_process_events.len(), 2);
        assert_eq!(
            sidecar
                .process_event_receiver
                .as_ref()
                .expect("process event receiver")
                .len(),
            1
        );

        let mut notified = Box::pin(sidecar.process_event_notify.notified());
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            notified.as_mut().poll(&mut context),
            Poll::Pending
        ));

        sidecar.pending_process_events.pop_front();
        sidecar.observe_pending_process_event_depth();
        sidecar.rearm_deferred_process_event_after_capacity_release();
        assert!(matches!(
            notified.as_mut().poll(&mut context),
            Poll::Ready(())
        ));
        drop(notified);

        sidecar
            .drain_runtime_process_event_channel_nowait()
            .expect("retry staged current envelope");
        let queued = sidecar
            .pending_process_events
            .iter()
            .map(|envelope| match &envelope.event {
                ActiveExecutionEvent::Stdout(bytes) => bytes[0],
                other => panic!("expected stdout, received {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(queued, vec![9, 1]);
        assert_eq!(
            sidecar
                .process_event_receiver
                .as_ref()
                .expect("process event receiver")
                .len(),
            1,
            "later envelope remains behind the retried current envelope"
        );
    }
}
