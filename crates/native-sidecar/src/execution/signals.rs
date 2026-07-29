use super::*;

/// Applies a kill signal to a tracked child execution. Shared-runtime
/// executions for lethal signals are terminated directly with a synthetic
/// signal exit so child polls observe a prompt close; everything else routes
/// through the kernel process table.
pub(super) fn terminate_tracked_child_process_for_signal(
    kernel: &mut SidecarKernel,
    child: &mut ActiveProcess,
    signal: i32,
    registration: Option<&SignalHandlerRegistration>,
) -> Result<(), SidecarError> {
    if signal == 0 {
        return kernel
            .kill_process(EXECUTION_DRIVER_NAME, child.kernel_pid, signal)
            .map_err(kernel_error);
    }

    // The runtime may have published its terminal event before the parent has
    // polled and reaped it. Keep that queued exit authoritative and make a
    // cleanup kill idempotent instead of sending a late terminate command to a
    // completed V8 session.
    if child.execution.has_exited() {
        return Ok(());
    }

    if signal == libc::SIGCONT {
        apply_active_process_default_signal(kernel, child, signal)?;
        match registration.map(|registration| &registration.action) {
            Some(SignalDispositionAction::User) => {
                if matches!(&child.execution, ActiveExecution::Wasm(execution) if execution.uses_shared_v8_runtime())
                {
                    child.queue_pending_wasm_signal(signal)?;
                } else if let Some(session) = child.execution.javascript_v8_session_handle() {
                    dispatch_v8_session_signal(session, signal);
                } else if !dispatch_v8_process_signal(child, signal)? {
                    return Err(SidecarError::InvalidState(format!(
                        "unsupported guest SIGCONT handler delivery for pid {}",
                        child.kernel_pid
                    )));
                }
            }
            Some(SignalDispositionAction::Default | SignalDispositionAction::Ignore) | None => {}
        }
        return Ok(());
    }

    // SIGKILL and SIGSTOP are uncatchable. Every other signal first honors the
    // guest disposition. Shared WASM consumes user signals cooperatively at
    // syscall boundaries instead of receiving an OS signal on a Tokio worker.
    if !matches!(signal, libc::SIGKILL | libc::SIGSTOP) {
        match registration.map(|registration| &registration.action) {
            Some(SignalDispositionAction::Ignore) => return Ok(()),
            Some(SignalDispositionAction::User) => {
                if matches!(&child.execution, ActiveExecution::Wasm(execution) if execution.uses_shared_v8_runtime())
                {
                    return child.queue_pending_wasm_signal(signal);
                }
                if let Some(session) = child.execution.javascript_v8_session_handle().filter(|_| {
                    matches!(&child.execution, ActiveExecution::Javascript(execution) if execution.uses_shared_v8_runtime())
                }) {
                    dispatch_v8_session_signal(session, signal);
                    return Ok(());
                }
                if dispatch_v8_process_signal(child, signal)? {
                    return Ok(());
                }
                return Err(SidecarError::InvalidState(format!(
                    "unsupported guest signal handler delivery for pid {}",
                    child.kernel_pid
                )));
            }
            Some(SignalDispositionAction::Default) | None => {}
        }
    }

    if matches!(
        canonical_signal_name(signal),
        Some("SIGWINCH" | "SIGCHLD" | "SIGURG")
    ) {
        return Ok(());
    }
    apply_active_process_default_signal(kernel, child, signal)
}

fn sidecar_error_is_esrch(error: &SidecarError) -> bool {
    error.to_string().contains("ESRCH")
}

pub(crate) fn apply_active_process_default_signal(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    signal: i32,
) -> Result<(), SidecarError> {
    if matches!(
        signal,
        libc::SIGSTOP | libc::SIGTSTP | libc::SIGTTIN | libc::SIGTTOU
    ) {
        if process.execution.uses_shared_v8_runtime() {
            process.execution.pause()?;
        } else {
            signal_runtime_process(process.execution.child_pid(), signal)?;
        }
        return kernel
            .kill_process(EXECUTION_DRIVER_NAME, process.kernel_pid, signal)
            .map_err(kernel_error);
    }
    if signal == libc::SIGCONT {
        // Linux resumes a stopped process even when SIGCONT is ignored or has
        // a handler. Handler delivery is layered on by the caller afterwards.
        if process.execution.uses_shared_v8_runtime() {
            process.execution.resume()?;
        } else {
            signal_runtime_process(process.execution.child_pid(), signal)?;
        }
        return kernel
            .kill_process(EXECUTION_DRIVER_NAME, process.kernel_pid, signal)
            .map_err(kernel_error);
    }

    if signal != 0 && matches!(process.execution, ActiveExecution::Python(_)) {
        close_kernel_process_stdin(kernel, process)?;
    }

    if process.execution.uses_shared_v8_runtime() {
        process.exit_signal = (signal != 0).then_some(signal);
        process.exit_core_dumped = false;
        process.execution.terminate()?;
        if signal != 0 && matches!(process.execution, ActiveExecution::Wasm(_)) {
            process.queue_pending_execution_event(ActiveExecutionEvent::Exited(128 + signal))?;
        }
        return Ok(());
    }

    signal_runtime_process(process.execution.child_pid(), signal)
}

pub(super) fn map_wasm_signal_registration(
    registration: agentos_execution::wasm::WasmSignalHandlerRegistration,
) -> SignalHandlerRegistration {
    SignalHandlerRegistration {
        action: match registration.action {
            agentos_execution::wasm::WasmSignalDispositionAction::Default => {
                crate::protocol::SignalDispositionAction::Default
            }
            agentos_execution::wasm::WasmSignalDispositionAction::Ignore => {
                crate::protocol::SignalDispositionAction::Ignore
            }
            agentos_execution::wasm::WasmSignalDispositionAction::User => {
                crate::protocol::SignalDispositionAction::User
            }
        },
        mask: registration.mask,
        flags: registration.flags,
    }
}

pub(super) fn map_node_signal_registration(
    registration: NodeSignalHandlerRegistration,
) -> SignalHandlerRegistration {
    SignalHandlerRegistration {
        action: match registration.action {
            NodeSignalDispositionAction::Default => SignalDispositionAction::Default,
            NodeSignalDispositionAction::Ignore => SignalDispositionAction::Ignore,
            NodeSignalDispositionAction::User => SignalDispositionAction::User,
        },
        mask: registration.mask,
        flags: registration.flags,
    }
}

fn process_signal_state_key<'a>(process_id: &'a str, child_path: &[&'a str]) -> &'a str {
    child_path.last().copied().unwrap_or(process_id)
}

pub(super) fn reset_caught_signal_dispositions_after_exec(
    signal_states: &mut BTreeMap<String, BTreeMap<u32, SignalHandlerRegistration>>,
    process_id: &str,
    child_path: &[&str],
) -> String {
    let signal_key = process_signal_state_key(process_id, child_path).to_owned();
    if let Some(registrations) = signal_states.get_mut(&signal_key) {
        registrations
            .retain(|_, registration| registration.action == SignalDispositionAction::Ignore);
    }
    signal_key
}

pub(super) fn javascript_child_process_sync_input_bytes(
    value: Option<&Value>,
) -> Result<Option<Vec<u8>>, SidecarError> {
    let Some(value) = value else {
        return Ok(None);
    };

    match value {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.as_bytes().to_vec())),
        other => javascript_sync_rpc_bytes_arg(
            std::slice::from_ref(other),
            0,
            "child_process.spawn_sync input",
        )
        .map(Some),
    }
}

// bridge_permissions moved to crate::bridge

// reconcile_mounts, resolve_cwd moved to crate::vm

fn signal_name_for_stream_event(signal: i32) -> Option<&'static str> {
    match signal {
        libc::SIGHUP => Some("SIGHUP"),
        libc::SIGINT => Some("SIGINT"),
        libc::SIGUSR1 => Some("SIGUSR1"),
        libc::SIGALRM => Some("SIGALRM"),
        libc::SIGCONT => Some("SIGCONT"),
        libc::SIGTERM => Some("SIGTERM"),
        libc::SIGCHLD => Some("SIGCHLD"),
        libc::SIGWINCH => Some("SIGWINCH"),
        _ => None,
    }
}

pub(crate) fn canonical_signal_name(signal: i32) -> Option<&'static str> {
    agentos_native_sidecar_core::canonical_signal_name(signal)
}

pub(super) fn dispatch_v8_process_signal(
    process: &ActiveProcess,
    signal: i32,
) -> Result<bool, SidecarError> {
    if signal_name_for_stream_event(signal).is_none() {
        return Ok(false);
    }
    let Some(session) = process.execution.javascript_v8_session_handle() else {
        return Ok(false);
    };
    session
        .publish_signal(signal)
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    Ok(true)
}

pub(super) fn dispatch_v8_session_signal(session: V8SessionHandle, signal: i32) {
    if signal_name_for_stream_event(signal).is_none() {
        return;
    }
    if let Err(error) = session.publish_signal(signal) {
        eprintln!("ERR_AGENTOS_SIGNAL_DELIVERY: could not enqueue signal {signal}: {error}");
    }
}

pub(crate) fn parse_signal(signal: &str) -> Result<i32, SidecarError> {
    let trimmed = signal.trim();
    if trimmed.is_empty() {
        return Err(SidecarError::InvalidState(String::from(
            "kill_process requires a non-empty signal",
        )));
    }

    if let Ok(value) = trimmed.parse::<i32>() {
        return match value {
            0..=31 => Ok(value),
            _ => Err(SidecarError::InvalidState(format!(
                "unsupported kill_process signal {signal}"
            ))),
        };
    }

    agentos_native_sidecar_core::parse_posix_signal(trimmed).ok_or_else(|| {
        SidecarError::InvalidState(format!("unsupported kill_process signal {signal}"))
    })
}

pub(crate) fn runtime_child_is_alive(child_pid: u32) -> Result<bool, SidecarError> {
    Ok(matches!(
        runtime_child_exit_status(child_pid)?,
        RuntimeChildStatusObservation::Running
    ))
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RuntimeChildExitStatus {
    pub(super) status: i32,
    pub(super) signal: Option<i32>,
    pub(super) core_dumped: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum RuntimeChildStatusObservation {
    Running,
    Exited(RuntimeChildExitStatus),
    /// The pid is not a waitable child (or its status was already consumed).
    /// This is not an exit status and must never be converted to exit(0).
    NotWaitable,
}

#[cfg(not(target_os = "macos"))]
pub(super) fn runtime_child_exit_status(
    child_pid: u32,
) -> Result<RuntimeChildStatusObservation, SidecarError> {
    if child_pid == 0 {
        return Ok(RuntimeChildStatusObservation::Exited(
            RuntimeChildExitStatus {
                status: 0,
                signal: None,
                core_dumped: false,
            },
        ));
    }

    let wait_flags = WaitPidFlag::WNOHANG
        | WaitPidFlag::WNOWAIT
        | WaitPidFlag::WEXITED
        | WaitPidFlag::WUNTRACED
        | WaitPidFlag::WCONTINUED;
    match wait_on_child(WaitId::Pid(Pid::from_raw(child_pid as i32)), wait_flags) {
        Ok(WaitStatus::StillAlive)
        | Ok(WaitStatus::Stopped(_, _))
        | Ok(WaitStatus::Continued(_)) => Ok(RuntimeChildStatusObservation::Running),
        Ok(WaitStatus::Exited(_, status)) => Ok(RuntimeChildStatusObservation::Exited(
            RuntimeChildExitStatus {
                status,
                signal: None,
                core_dumped: false,
            },
        )),
        Ok(WaitStatus::Signaled(_, signal, core_dumped)) => Ok(
            RuntimeChildStatusObservation::Exited(RuntimeChildExitStatus {
                status: 128 + signal as i32,
                signal: Some(signal as i32),
                core_dumped,
            }),
        ),
        #[cfg(any(target_os = "linux", target_os = "android"))]
        Ok(WaitStatus::PtraceEvent(_, _, _) | WaitStatus::PtraceSyscall(_)) => {
            Ok(RuntimeChildStatusObservation::Running)
        }
        Err(nix::errno::Errno::ECHILD) => Ok(RuntimeChildStatusObservation::NotWaitable),
        Err(error) => Err(SidecarError::Execution(format!(
            "failed to inspect guest runtime process {child_pid}: {error}"
        ))),
    }
}

// macOS nix exposes no `waitid`/`WNOWAIT`, so we poll with `waitpid(WNOHANG)`.
// NOTE: unlike Linux's `waitid(WNOWAIT)`, `waitpid` REAPS an exited child rather
// than leaving it waitable. That is correct for this poll (the sidecar is the
// reaping parent), but a second status query after exit returns ECHILD → treated
// as "exited(0)" below.
#[cfg(target_os = "macos")]
pub(super) fn runtime_child_exit_status(
    child_pid: u32,
) -> Result<RuntimeChildStatusObservation, SidecarError> {
    if child_pid == 0 {
        return Ok(RuntimeChildStatusObservation::Exited(
            RuntimeChildExitStatus {
                status: 0,
                signal: None,
                core_dumped: false,
            },
        ));
    }

    match waitpid(Pid::from_raw(child_pid as i32), Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::StillAlive)
        | Ok(WaitStatus::Stopped(_, _))
        | Ok(WaitStatus::Continued(_)) => Ok(RuntimeChildStatusObservation::Running),
        Ok(WaitStatus::Exited(_, status)) => Ok(RuntimeChildStatusObservation::Exited(
            RuntimeChildExitStatus {
                status,
                signal: None,
                core_dumped: false,
            },
        )),
        Ok(WaitStatus::Signaled(_, signal, core_dumped)) => Ok(
            RuntimeChildStatusObservation::Exited(RuntimeChildExitStatus {
                status: 128 + signal as i32,
                signal: Some(signal as i32),
                core_dumped,
            }),
        ),
        Err(nix::errno::Errno::ECHILD) => Ok(RuntimeChildStatusObservation::NotWaitable),
        Err(error) => Err(SidecarError::Execution(format!(
            "failed to inspect guest runtime process {child_pid}: {error}"
        ))),
    }
}

pub(crate) fn signal_runtime_process(child_pid: u32, signal: i32) -> Result<(), SidecarError> {
    if child_pid == 0 {
        return Ok(());
    }

    if !runtime_child_is_alive(child_pid)? {
        return Ok(());
    }

    if signal == 0 {
        return Ok(());
    }

    let parsed = Signal::try_from(signal).map_err(|_| {
        SidecarError::InvalidState(format!("unsupported kill_process signal {signal}"))
    })?;
    let result = send_signal(Pid::from_raw(child_pid as i32), Some(parsed));

    match result {
        Ok(()) => Ok(()),
        Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(error) => Err(SidecarError::Execution(format!(
            "failed to signal guest runtime process {child_pid}: {error}"
        ))),
    }
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) async fn kill_process(
        &mut self,
        request: &RequestFrame,
        payload: KillProcessRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        self.kill_process_internal(&vm_id, &payload.process_id, &payload.signal)?;

        Ok(DispatchResult {
            response: process_killed_response(request, payload.process_id),
            events: Vec::new(),
        })
    }

    pub(crate) fn kill_process_internal(
        &mut self,
        vm_id: &str,
        process_id: &str,
        signal: &str,
    ) -> Result<(), SidecarError> {
        let signal_name = signal.to_owned();
        let signal = parse_signal(signal)?;
        let vm = self
            .vms
            .get_mut(vm_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
        let signal_action = vm
            .signal_states
            .get(process_id)
            .and_then(|handlers| handlers.get(&(signal as u32)))
            .map(|registration| registration.action.clone())
            .unwrap_or(SignalDispositionAction::Default);
        let process = vm.active_processes.get_mut(process_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("VM {vm_id} has no active process {process_id}"))
        })?;
        let kernel_pid = process.kernel_pid;
        if !matches!(signal, 0 | libc::SIGCONT) {
            // A guest parked in a deferred kernel-wait sync RPC is blocked in a
            // native bridge wait the kill cannot interrupt; answer the parked
            // RPC first so the termination can take effect.
            flush_parked_kernel_wait_rpc(process);
        }

        enum KillBehavior {
            Binding,
            SharedV8StateOnly,
            SharedV8Pause,
            SharedV8Continue,
            SharedV8Terminate,
            SharedV8DispatchOrTerminate,
            Noop,
            HostPid(u32),
        }

        let behavior = match &process.execution {
            ActiveExecution::Binding(_) => KillBehavior::Binding,
            _ if process.execution.uses_shared_v8_runtime() && signal == 0 => {
                KillBehavior::SharedV8StateOnly
            }
            _ if process.execution.uses_shared_v8_runtime()
                && matches!(
                    signal,
                    libc::SIGSTOP | libc::SIGTSTP | libc::SIGTTIN | libc::SIGTTOU
                )
                && (signal == libc::SIGSTOP
                    || signal_action == SignalDispositionAction::Default) =>
            {
                KillBehavior::SharedV8Pause
            }
            _ if process.execution.uses_shared_v8_runtime()
                && matches!(signal, libc::SIGTSTP | libc::SIGTTIN | libc::SIGTTOU)
                && signal_action == SignalDispositionAction::Ignore =>
            {
                KillBehavior::Noop
            }
            _ if process.execution.uses_shared_v8_runtime() && signal == libc::SIGCONT => {
                KillBehavior::SharedV8Continue
            }
            ActiveExecution::Javascript(execution)
                if execution.uses_shared_v8_runtime() && signal == SIGKILL =>
            {
                KillBehavior::SharedV8Terminate
            }
            ActiveExecution::Wasm(execution)
                if execution.uses_shared_v8_runtime() && signal == SIGKILL =>
            {
                KillBehavior::SharedV8Terminate
            }
            ActiveExecution::Javascript(execution) if execution.uses_shared_v8_runtime() => {
                KillBehavior::SharedV8DispatchOrTerminate
            }
            ActiveExecution::Wasm(execution) if execution.uses_shared_v8_runtime() => {
                KillBehavior::SharedV8DispatchOrTerminate
            }
            ActiveExecution::Python(execution)
                if execution.uses_shared_v8_runtime()
                    && signal_action != SignalDispositionAction::Default =>
            {
                KillBehavior::SharedV8DispatchOrTerminate
            }
            ActiveExecution::Python(execution) if execution.uses_shared_v8_runtime() => {
                KillBehavior::SharedV8Terminate
            }
            ActiveExecution::Javascript(execution) if execution.child_pid() == 0 => {
                KillBehavior::Noop
            }
            _ => KillBehavior::HostPid(process.execution.child_pid()),
        };

        match behavior {
            KillBehavior::Binding => {
                let ActiveExecution::Binding(execution) = &process.execution else {
                    unreachable!("kill behavior must match tool execution");
                };
                if signal != 0 {
                    execution.cancelled.store(true, Ordering::Relaxed);
                    process.exit_signal = Some(signal);
                    process.exit_core_dumped = false;
                    process.queue_pending_execution_event(ActiveExecutionEvent::Exited(
                        128 + signal,
                    ))?;
                }
            }
            KillBehavior::SharedV8StateOnly => {
                vm.kernel
                    .kill_process(EXECUTION_DRIVER_NAME, kernel_pid, signal)
                    .map_err(kernel_error)?;
            }
            KillBehavior::SharedV8Pause => {
                process.execution.pause()?;
                vm.kernel
                    .kill_process(EXECUTION_DRIVER_NAME, kernel_pid, signal)
                    .map_err(kernel_error)?;
            }
            KillBehavior::SharedV8Continue => {
                process.execution.resume()?;
                vm.kernel
                    .kill_process(EXECUTION_DRIVER_NAME, kernel_pid, signal)
                    .map_err(kernel_error)?;
                if matches!(&process.execution, ActiveExecution::Javascript(_)) {
                    if !dispatch_v8_process_signal(process, signal)? {
                        return Err(SidecarError::InvalidState(format!(
                            "unsupported guest SIGCONT handler delivery for pid {kernel_pid}"
                        )));
                    }
                } else if signal_action == SignalDispositionAction::User {
                    if matches!(&process.execution, ActiveExecution::Wasm(execution) if execution.uses_shared_v8_runtime())
                    {
                        process.queue_pending_wasm_signal(signal)?;
                    } else {
                        return Err(SidecarError::InvalidState(format!(
                            "unsupported guest SIGCONT handler delivery for pid {kernel_pid}"
                        )));
                    }
                }
            }
            KillBehavior::SharedV8Terminate => {
                if signal != 0 && matches!(process.execution, ActiveExecution::Python(_)) {
                    close_kernel_process_stdin(&mut vm.kernel, process)?;
                }
                process.exit_signal = (signal != 0).then_some(signal);
                process.exit_core_dumped = false;
                process.execution.terminate()?;
                let needs_synthetic_exit = matches!(process.execution, ActiveExecution::Wasm(_))
                    || (signal == SIGKILL
                        && matches!(process.execution, ActiveExecution::Javascript(_)));
                if signal != 0 && needs_synthetic_exit {
                    process.queue_pending_execution_event(ActiveExecutionEvent::Exited(
                        128 + signal,
                    ))?;
                }
            }
            KillBehavior::SharedV8DispatchOrTerminate => {
                if signal != 0 {
                    let is_shared_wasm = matches!(
                        &process.execution,
                        ActiveExecution::Wasm(execution) if execution.uses_shared_v8_runtime()
                    );
                    if is_shared_wasm {
                        match signal_action {
                            SignalDispositionAction::Ignore => {}
                            SignalDispositionAction::User => {
                                process.queue_pending_wasm_signal(signal)?;
                            }
                            SignalDispositionAction::Default => {
                                if !matches!(
                                    canonical_signal_name(signal),
                                    Some("SIGWINCH" | "SIGCHLD" | "SIGURG")
                                ) {
                                    process.exit_signal = Some(signal);
                                    process.exit_core_dumped = false;
                                    process.execution.terminate()?;
                                    process.queue_pending_execution_event(
                                        ActiveExecutionEvent::Exited(128 + signal),
                                    )?;
                                }
                            }
                        }
                    } else if matches!(process.execution, ActiveExecution::Python(_)) {
                        match signal_action {
                            SignalDispositionAction::Ignore => {}
                            SignalDispositionAction::User => {
                                return Err(SidecarError::InvalidState(format!(
                                    "unsupported guest signal handler delivery for pid {kernel_pid}"
                                )));
                            }
                            SignalDispositionAction::Default => {
                                process.exit_signal = Some(signal);
                                process.exit_core_dumped = false;
                                process.execution.terminate()?;
                            }
                        }
                    } else if !dispatch_v8_process_signal(process, signal)? {
                        process.exit_signal = Some(signal);
                        process.exit_core_dumped = false;
                        process.execution.terminate()?;
                    }
                }
            }
            KillBehavior::Noop => {}
            KillBehavior::HostPid(pid) => {
                if signal != 0 && matches!(process.execution, ActiveExecution::Python(_)) {
                    close_kernel_process_stdin(&mut vm.kernel, process)?;
                }
                signal_runtime_process(pid, signal)?;
            }
        }
        emit_security_audit_event(
            &self.bridge,
            vm_id,
            "security.process.kill",
            audit_fields([
                (String::from("source"), String::from("control_plane")),
                (String::from("source_pid"), String::from("0")),
                (String::from("target_pid"), process.kernel_pid.to_string()),
                (String::from("process_id"), process_id.to_owned()),
                (String::from("signal"), signal_name),
                (
                    String::from("host_pid"),
                    process.execution.child_pid().to_string(),
                ),
            ]),
        );
        Ok(())
    }

    /// Delivers a signal to one kernel pid inside a VM, resolving the target
    /// through the active-process tree first so tracked sidecar executions get
    /// the same termination handling as a direct `child_process.kill`.
    /// Untracked kernel processes (for example WASM subprocess trees) receive
    /// the signal through the kernel process table directly.
    pub(crate) fn signal_vm_kernel_pid(
        &mut self,
        vm_id: &str,
        target_kernel_pid: u32,
        signal_name: &str,
    ) -> Result<(), SidecarError> {
        let signal = parse_signal(signal_name)?;
        let located = {
            let Some(vm) = self.vms.get(vm_id) else {
                return Err(SidecarError::InvalidState(String::from(
                    "ESRCH: unknown VM during process.kill",
                )));
            };
            let alive = vm
                .kernel
                .list_processes()
                .get(&target_kernel_pid)
                .is_some_and(|info| info.status != ProcessStatus::Exited);
            if !alive {
                return Err(SidecarError::InvalidState(format!(
                    "ESRCH: no such process {target_kernel_pid}"
                )));
            }
            vm.active_processes.iter().find_map(|(process_id, root)| {
                Self::active_process_path_by_kernel_pid(root, target_kernel_pid)
                    .map(|path| (process_id.clone(), path))
            })
        };

        match located {
            Some((process_id, path)) if path.is_empty() => {
                self.kill_process_internal(vm_id, &process_id, signal_name)
            }
            Some((process_id, path)) => {
                let Some(vm) = self.vms.get_mut(vm_id) else {
                    return Ok(());
                };
                let signal_key = path.last().map(String::as_str).unwrap_or(&process_id);
                let registration = vm
                    .signal_states
                    .get(signal_key)
                    .and_then(|handlers| handlers.get(&(signal as u32)))
                    .cloned();
                let Some(root) = vm.active_processes.get_mut(&process_id) else {
                    return Ok(());
                };
                let Some(target) = Self::active_process_by_owned_path_mut(root, &path) else {
                    return Err(SidecarError::InvalidState(format!(
                        "ESRCH: no such process {target_kernel_pid}"
                    )));
                };
                terminate_tracked_child_process_for_signal(
                    &mut vm.kernel,
                    target,
                    signal,
                    registration.as_ref(),
                )?;
                emit_security_audit_event(
                    &self.bridge,
                    vm_id,
                    "security.process.kill",
                    audit_fields([
                        (String::from("source"), String::from("guest_process")),
                        (String::from("target_pid"), target_kernel_pid.to_string()),
                        (String::from("process_id"), process_id),
                        (String::from("signal"), signal_name.to_owned()),
                    ]),
                );
                Ok(())
            }
            None => {
                let Some(vm) = self.vms.get_mut(vm_id) else {
                    return Ok(());
                };
                let target_pid = i32::try_from(target_kernel_pid).map_err(|_| {
                    SidecarError::InvalidState(format!(
                        "EINVAL: invalid process pid {target_kernel_pid}"
                    ))
                })?;
                vm.kernel
                    .signal_process(EXECUTION_DRIVER_NAME, target_pid, signal)
                    .map_err(kernel_error)?;
                emit_security_audit_event(
                    &self.bridge,
                    vm_id,
                    "security.process.kill",
                    audit_fields([
                        (String::from("source"), String::from("guest_process")),
                        (String::from("target_pid"), target_kernel_pid.to_string()),
                        (String::from("signal"), signal_name.to_owned()),
                    ]),
                );
                Ok(())
            }
        }
    }

    /// Delivers a signal to every live member of a VM process group, matching
    /// Linux `kill(-pgid, sig)` semantics. Returns whether the caller itself
    /// is a member of the group so entry points can apply self-signal
    /// delivery; the caller is intentionally skipped here.
    pub(crate) fn signal_vm_process_group(
        &mut self,
        vm_id: &str,
        caller_kernel_pid: u32,
        pgid: u32,
        signal_name: &str,
    ) -> Result<bool, SidecarError> {
        parse_signal(signal_name)?;
        let members = {
            let Some(vm) = self.vms.get(vm_id) else {
                return Err(SidecarError::InvalidState(String::from(
                    "ESRCH: unknown VM during process.kill",
                )));
            };
            vm.kernel
                .list_processes()
                .into_iter()
                .filter(|(_, info)| info.pgid == pgid && info.status != ProcessStatus::Exited)
                .map(|(pid, _)| pid)
                .collect::<Vec<_>>()
        };
        if members.is_empty() {
            return Err(SidecarError::InvalidState(format!(
                "ESRCH: no such process group {pgid}"
            )));
        }

        let mut caller_is_member = false;
        for member_pid in members {
            if member_pid == caller_kernel_pid {
                caller_is_member = true;
                continue;
            }
            match self.signal_vm_kernel_pid(vm_id, member_pid, signal_name) {
                Ok(()) => {}
                // Group members can exit while the group is being signaled. A
                // vanished member is not an error for the group kill overall.
                Err(error) if sidecar_error_is_esrch(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(caller_is_member)
    }

    /// Delivers a signal already generated by the kernel to the tracked
    /// runtimes in one process group. The kernel has already notified its
    /// process records, so this path deliberately excludes untracked members:
    /// signaling those through `signal_vm_kernel_pid` would deliver the same
    /// signal to the kernel twice.
    pub(crate) fn deliver_kernel_process_group_signal_to_tracked_runtimes(
        &mut self,
        vm_id: &str,
        pgid: u32,
        signal_name: &str,
    ) -> Result<(), SidecarError> {
        parse_signal(signal_name)?;
        let tracked_members = {
            let Some(vm) = self.vms.get(vm_id) else {
                return Err(SidecarError::InvalidState(format!(
                    "unknown sidecar VM {vm_id}"
                )));
            };
            vm.kernel
                .list_processes()
                .into_iter()
                .filter(|(_, info)| info.pgid == pgid && info.status != ProcessStatus::Exited)
                .filter_map(|(kernel_pid, _)| {
                    vm.active_processes
                        .values()
                        .any(|root| {
                            Self::active_process_path_by_kernel_pid(root, kernel_pid).is_some()
                        })
                        .then_some(kernel_pid)
                })
                .collect::<Vec<_>>()
        };

        for kernel_pid in tracked_members {
            match self.signal_vm_kernel_pid(vm_id, kernel_pid, signal_name) {
                Ok(()) => {}
                // A process can exit after the group snapshot but before the
                // tracked runtime is notified. Linux still considers the
                // process-group signal successful for the remaining members.
                Err(error) if sidecar_error_is_esrch(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};

    fn await_child_status(child: &mut Child) -> RuntimeChildExitStatus {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match runtime_child_exit_status(child.id()).expect("inspect child status") {
                RuntimeChildStatusObservation::Exited(status) => {
                    // Linux waitid(WNOWAIT) leaves the status waitable; macOS
                    // waitpid already reaped it. Either way, do not leak the
                    // test child.
                    let _ = child.wait();
                    return status;
                }
                RuntimeChildStatusObservation::Running if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                RuntimeChildStatusObservation::Running => panic!("child did not exit in time"),
                RuntimeChildStatusObservation::NotWaitable => {
                    panic!("child status became unobservable")
                }
            }
        }
    }

    #[test]
    fn native_wait_status_distinguishes_exit_137_from_sigkill() {
        let mut normal_exit = Command::new("sh")
            .args(["-c", "exit 137"])
            .spawn()
            .expect("spawn normal-exit child");
        let normal_status = await_child_status(&mut normal_exit);
        assert_eq!(normal_status.status, 137);
        assert_eq!(normal_status.signal, None);
        assert!(!normal_status.core_dumped);

        let mut signaled = Command::new("sh")
            .args(["-c", "kill -KILL $$"])
            .spawn()
            .expect("spawn signaled child");
        let signal_status = await_child_status(&mut signaled);
        assert_eq!(signal_status.status, 137);
        assert_eq!(signal_status.signal, Some(libc::SIGKILL));
    }
}
