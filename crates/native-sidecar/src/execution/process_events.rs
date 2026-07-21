use super::*;
use crate::protocol::ExecutionStreamChannel;

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
    pub async fn pump_process_events(
        &mut self,
        ownership: &OwnershipScope,
    ) -> Result<bool, SidecarError> {
        let mut emitted_any = false;
        self.expire_public_execution_deadlines()?;

        let mut queued_envelopes = Vec::new();
        {
            let pending_capacity = self.pending_process_event_capacity();
            let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
                SidecarError::InvalidState(String::from("process event receiver unavailable"))
            })?;
            loop {
                if queued_envelopes.len() >= pending_capacity {
                    if receiver.is_empty() {
                        break;
                    }
                    return Err(process_event_queue_overflow_error(
                        self.config.runtime.protocol.max_process_events,
                    ));
                }
                match receiver.try_recv() {
                    Ok(envelope) => {
                        queued_envelopes.push(envelope);
                        emitted_any = true;
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
        }
        for envelope in queued_envelopes {
            self.queue_pending_process_event(envelope)?;
        }

        let vm_ids = self.vm_ids_for_scope(ownership)?;
        for vm_id in vm_ids {
            let vm_work_limit = self.config.runtime.fairness.vm_quantum_operations;
            let mut vm_work = 0usize;
            if let Some(vm) = self.vms.get(&vm_id) {
                vm.kernel.reap_due_zombies();
            }
            'vm_event_turn: while let Some(vm) = self.vms.get(&vm_id) {
                let connection_id = vm.connection_id.clone();
                let session_id = vm.session_id.clone();
                let process_ids = self
                    .vms
                    .get(&vm_id)
                    .map(|vm| vm.active_processes.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
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
                        let Some(vm) = self.vms.get_mut(&vm_id) else {
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
                            process_id: process_id.clone(),
                            event,
                        };
                        if let Err(error) = self.check_pending_process_event_capacity(&envelope) {
                            if let Some(process) = self
                                .vms
                                .get_mut(&vm_id)
                                .and_then(|vm| vm.active_processes.get_mut(&process_id))
                            {
                                process.requeue_pending_execution_event(PolledExecutionEvent {
                                    event: envelope.event,
                                    reservation,
                                })?;
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
        let Some(vm) = self.vms.get_mut(vm_id) else {
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

    pub(super) fn active_process_by_path<'a>(
        process: &'a ActiveProcess,
        child_path: &[&str],
    ) -> Option<&'a ActiveProcess> {
        let mut current = process;
        for child_id in child_path {
            current = current.child_processes.get(*child_id)?;
        }
        Some(current)
    }

    pub(super) fn active_process_by_path_mut<'a>(
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
        let Some(vm) = self.vms.get(vm_id) else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "execution event dispatch");
            return Ok(None);
        };
        if !vm.active_processes.contains_key(process_id) {
            log_stale_process_event(&self.bridge, vm_id, process_id, "execution event dispatch");
            return Ok(None);
        }
        let (connection_id, session_id) = { (vm.connection_id.clone(), vm.session_id.clone()) };
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
                let Some(vm) = self.vms.get_mut(vm_id) else {
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
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let Some(process) = vm.active_processes.get_mut(process_id) else {
            return Ok(());
        };
        let connected = process
            .pending_javascript_net_connects
            .remove(&completion.request_id);
        let completion_result = match (completion.result, connected) {
            (Ok(_), Some(connected)) => {
                finalize_javascript_net_connect(process, &kernel_readiness, connected).map_err(
                    |error| crate::state::DeferredRpcError {
                        code: javascript_sync_rpc_error_code(&error),
                        message: javascript_sync_rpc_error_message(&error),
                    },
                )
            }
            (result @ Err(_), Some(connected)) => {
                restore_pending_bound_unix_connect(process, &connected)?;
                result
            }
            (result, None) => result,
        };
        let result = match completion_result {
            Ok(value) => process
                .execution
                .respond_javascript_sync_rpc_success(completion.request_id, value),
            Err(error) => process.execution.respond_javascript_sync_rpc_error(
                completion.request_id,
                error.code,
                error.message,
            ),
        };
        result.or_else(ignore_stale_javascript_sync_rpc_response)
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
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
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
            return self.respond_python_rpc(vm_id, process_id, request_id, Err(error));
        }
        if let Err(error) =
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
            return self.respond_python_rpc(vm_id, process_id, request_id, Err(error));
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
        self.respond_python_rpc(
            vm_id,
            process_id,
            request_id,
            Ok(PythonVfsRpcResponsePayload::SocketCreated {
                socket_id: python_socket_id,
            }),
        )
    }

    pub(crate) fn finish_active_process_exit(
        &mut self,
        vm_id: &str,
        process_id: &str,
        exit_code: i32,
    ) -> Result<Option<bool>, SidecarError> {
        let Some(vm) = self.vms.get_mut(vm_id) else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "process exit cleanup");
            return Ok(None);
        };
        if !vm.active_processes.contains_key(process_id) {
            log_stale_process_event(&self.bridge, vm_id, process_id, "process exit cleanup");
            return Ok(None);
        }

        let phase_start = Instant::now();
        prune_exited_process_snapshots(vm);
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
            sync_process_host_writes_to_kernel(vm, &process)
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
        let phase_start = Instant::now();
        self.prune_extension_process_resource(process_id);
        record_execute_phase("process_exit_cleanup_prune_resource", phase_start.elapsed());

        // The process was removed from active_processes before the fallible
        // host/raw-mode cleanup. Surface those errors only after all process-
        // owned resources (especially host-materialized SQLite state) have
        // been copied back and finalized.
        host_sync_result?;
        raw_mode_result?;
        Ok(Some(became_idle))
    }
}
