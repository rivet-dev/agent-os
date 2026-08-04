use super::super::*;

/// Build the complete owned service for one claimed Python process event.
/// Keeping the claimed target alive here also retains its queue reservation
/// until the request has either been deferred or replied to.
pub(crate) fn prepare_owned_python_process_event_service<B>(
    sidecar: &NativeSidecar<B>,
    target: OwnedPythonEventService,
) -> Pin<Box<dyn Future<Output = Result<(), SidecarError>> + 'static>>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if target.request.method == PythonVfsRpcMethod::SubprocessRun {
        let vm = target.vm.clone();
        let vm_id = target.vm_id.clone();
        let process_id = target.process_id.clone();
        let child_path = target.child_path.clone();
        let responder = target.responder.clone();
        let request = target.request.clone();
        let process_event_capacity = sidecar.config.runtime.protocol.max_process_events;
        let sidecar_requests = SharedSidecarRequestClient::clone(&sidecar.sidecar_requests);
        let process_event_notify = Arc::clone(&sidecar.process_event_notify);
        let cache_root = sidecar.cache_root.clone();
        return Box::pin(async move {
            let service = prepare_owned_python_subprocess_run::<B>(
                vm,
                vm_id,
                process_id,
                child_path,
                responder,
                request,
                process_event_capacity,
                sidecar_requests,
                process_event_notify,
                cache_root,
            );
            let result = service.await;
            drop(target);
            result
        });
    }

    let bridge = sidecar.bridge.clone();
    let process_event_sender = sidecar.process_event_sender.clone();
    let process_event_notify = Arc::clone(&sidecar.process_event_notify);
    Box::pin(async move {
        let result = service_owned_python_vfs_rpc_request(
            bridge,
            target.vm.clone(),
            target.vm_id.clone(),
            target.process_id.clone(),
            target.child_path.clone(),
            target.responder.clone(),
            target.request.clone(),
            process_event_sender,
            process_event_notify,
        )
        .await;
        drop(target);
        result
    })
}

/// Prepare a Python `subprocess.run` service whose child startup and eventual
/// response are owned by the request rather than the protocol dispatcher.
/// Every VM-state access is a short synchronous command; the returned future
/// never retains `NativeSidecar` or a `VmState` borrow across an await.
pub(crate) fn prepare_owned_python_subprocess_run<B>(
    vm: VmHandle,
    vm_id: String,
    process_id: String,
    child_path: Vec<String>,
    responder: PythonVfsRpcResponder,
    request: PythonVfsRpcRequest,
    process_event_capacity: usize,
    sidecar_requests: SharedSidecarRequestClient,
    process_event_notify: Arc<tokio::sync::Notify>,
    cache_root: PathBuf,
) -> Pin<Box<dyn Future<Output = Result<(), SidecarError>> + 'static>>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let request_id = request.id;
    let prepared = (|| {
        let command = request.command.clone().ok_or_else(|| {
            SidecarError::InvalidState(String::from("python subprocessRun requires a command"))
        })?;
        let (internal_bootstrap_env, cwd, max_buffer) =
            vm.try_command("prepare owned Python subprocess", |vm| {
                let root = vm
                    .active_processes
                    .get(&process_id)
                    .ok_or_else(|| missing_process_error(&vm_id, &process_id))?;
                let path = child_path.iter().map(String::as_str).collect::<Vec<_>>();
                let process = NativeSidecar::<B>::active_process_by_path(root, &path)
                    .ok_or_else(|| javascript_child_process_gone_error(&process_id, &path))?;
                let virtual_home = guest_virtual_home(vm);
                let cwd = request.cwd.clone().or_else(|| {
                    guest_runtime_path_for_host_path(
                        &vm.guest_env,
                        &virtual_home,
                        &vm.host_cwd,
                        &process.host_cwd.to_string_lossy(),
                    )
                });
                Ok((
                    sanitize_javascript_child_process_internal_bootstrap_env(&vm.guest_env),
                    cwd,
                    NativeSidecar::<B>::child_process_sync_max_buffer(process, request.max_buffer)?,
                ))
            })?;
        let spawn = NativeSidecar::<B>::build_owned_descendant_javascript_child_process_spawn(
            vm.clone(),
            vm_id.clone(),
            process_id.clone(),
            child_path.clone(),
            JavascriptChildProcessSpawnRequest {
                command,
                args: request.args.clone(),
                options: JavascriptChildProcessSpawnOptions {
                    cwd,
                    env: request.env.clone(),
                    input: None,
                    internal_bootstrap_env,
                    shell: request.shell,
                    detached: false,
                    stdio: vec![
                        String::from("pipe"),
                        String::from("pipe"),
                        String::from("pipe"),
                    ],
                    timeout: None,
                    kill_signal: None,
                    ..JavascriptChildProcessSpawnOptions::default()
                },
            },
            process_event_capacity,
            sidecar_requests,
            process_event_notify,
            cache_root,
        );
        Ok::<_, SidecarError>((spawn, max_buffer))
    })();

    Box::pin(async move {
        let (spawn, max_buffer) = prepared?;
        let spawned = spawn.await?;
        let child_process_id = spawned
            .get("childId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "python subprocessRun spawn response is missing childId",
                ))
            })?
            .to_owned();
        let pid = spawned
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "python subprocessRun spawn response is missing a valid pid",
                ))
            })?;
        let mut pending_subprocess = PendingOwnedRegisteredChild::new(
            vm.clone(),
            process_id.clone(),
            child_path.clone(),
            child_process_id.clone(),
        );

        vm.try_command("close owned Python subprocess stdin", |vm| {
            let VmState {
                kernel,
                active_processes,
                ..
            } = vm;
            let root = active_processes
                .get_mut(&process_id)
                .ok_or_else(|| missing_process_error(&vm_id, &process_id))?;
            let path = child_path.iter().map(String::as_str).collect::<Vec<_>>();
            let process = NativeSidecar::<B>::active_process_by_path_mut(root, &path)
                .ok_or_else(|| javascript_child_process_gone_error(&process_id, &path))?;
            let child = process
                .child_processes
                .get_mut(&child_process_id)
                .ok_or_else(|| {
                    javascript_child_process_gone_error(&process_id, &[child_process_id.as_str()])
                })?;
            child.execution.close_stdin()?;
            close_kernel_process_stdin(kernel, child)
        })?;

        vm.try_command("register owned Python subprocess completion", |vm| {
            let root = vm
                .active_processes
                .get_mut(&process_id)
                .ok_or_else(|| missing_process_error(&vm_id, &process_id))?;
            let path = child_path.iter().map(String::as_str).collect::<Vec<_>>();
            let process = NativeSidecar::<B>::active_process_by_path_mut(root, &path)
                .ok_or_else(|| javascript_child_process_gone_error(&process_id, &path))?;
            process.pending_child_process_sync.insert(
                child_process_id,
                PendingChildProcessSync {
                    pid,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    max_buffer,
                    deadline: None,
                    timeout_signal: String::from("SIGTERM"),
                    kill_sent: false,
                    timed_out: false,
                    max_buffer_exceeded: false,
                    completion: PendingChildProcessSyncCompletion::Python {
                        request_id,
                        responder,
                    },
                },
            );
            Ok(())
        })?;
        pending_subprocess.disarm();
        Ok(())
    })
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(in crate::execution) async fn handle_python_subprocess_rpc_request(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: PythonVfsRpcRequest,
    ) -> Result<(), SidecarError> {
        let Some(command) = request.command.clone() else {
            return self.respond_python_rpc(
                vm_id,
                process_id,
                request.id,
                Err(SidecarError::InvalidState(String::from(
                    "python subprocessRun requires a command",
                ))),
            );
        };
        let (internal_bootstrap_env, cwd, responder) = {
            let Some(vm) = self.vms.get(vm_id) else {
                return Ok(());
            };
            let Some(process) = vm.active_processes.get(process_id) else {
                return Ok(());
            };
            let virtual_home = guest_virtual_home(&vm);
            let cwd = request.cwd.clone().or_else(|| {
                guest_runtime_path_for_host_path(
                    &vm.guest_env,
                    &virtual_home,
                    &vm.host_cwd,
                    &process.host_cwd.to_string_lossy(),
                )
            });
            (
                sanitize_javascript_child_process_internal_bootstrap_env(&vm.guest_env),
                cwd,
                process.execution.python_vfs_rpc_responder()?,
            )
        };
        let result = self
            .begin_javascript_child_process_sync(
                vm_id,
                process_id,
                JavascriptChildProcessSpawnRequest {
                    command,
                    args: request.args.clone(),
                    options: JavascriptChildProcessSpawnOptions {
                        cwd,
                        env: request.env.clone(),
                        input: None,
                        internal_bootstrap_env,
                        shell: request.shell,
                        detached: false,
                        stdio: vec![
                            String::from("pipe"),
                            String::from("pipe"),
                            String::from("pipe"),
                        ],
                        timeout: None,
                        kill_signal: None,
                        ..JavascriptChildProcessSpawnOptions::default()
                    },
                },
                request.max_buffer,
                PendingChildProcessSyncCompletion::Python {
                    request_id: request.id,
                    responder,
                },
            )
            .await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.respond_python_rpc(vm_id, process_id, request.id, Err(error)),
        }
    }
}

#[cfg(test)]
mod owned_subprocess_tests {
    use super::*;
    use crate::protocol::{RequestFrame, RequestPayload, ResponsePayload};
    use crate::state::{BindingExecution, ConnectionState, SessionState};
    use crate::stdio::LocalBridge;
    use crate::NativeSidecarConfig;

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_python_subprocess_registration_removes_spawned_child() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("Python subprocess rollback runtime");
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config,
                    Vec::new(),
                    runtime.context(),
                )
                .expect("Python subprocess rollback sidecar");
                let connection_id = "python-subprocess-rollback-connection";
                let session_id = "python-subprocess-rollback-session";
                sidecar.connections.insert(
                    connection_id.to_owned(),
                    ConnectionState {
                        auth_token: String::new(),
                        sessions: BTreeSet::from([session_id.to_owned()]),
                    },
                );
                sidecar.sessions.insert(
                    session_id.to_owned(),
                    SessionState {
                        connection_id: connection_id.to_owned(),
                        placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                            crate::protocol::SidecarPlacementShared { pool: None },
                        ),
                        metadata: BTreeMap::new(),
                        vm_ids: BTreeSet::new(),
                    },
                );
                let request = RequestFrame::new(
                    1,
                    OwnershipScope::session(connection_id, session_id),
                    RequestPayload::CreateVm(crate::protocol::CreateVmRequest::legacy_test_config(
                        GuestRuntimeKind::JavaScript,
                        Default::default(),
                        Default::default(),
                        Some(crate::wire::PermissionsPolicy::allow_all()),
                    )),
                );
                let RequestPayload::CreateVm(payload) = request.payload.clone() else {
                    unreachable!("rollback fixture creates a VM");
                };
                let dispatch = sidecar
                    .create_vm(&request, payload)
                    .await
                    .expect("create rollback VM");
                let ResponsePayload::VmCreated(created) = dispatch.response.payload else {
                    panic!("rollback VM creation returned another response");
                };
                let vm_id = created.vm_id;
                let process_id = String::from("python-caller");
                let child_id = String::from("unregistered-subprocess");
                let (vm_handle, child_pid) = {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("rollback VM");
                    let guest_env = vm.guest_env.clone();
                    let root_handle = vm
                        .kernel
                        .create_virtual_process(
                            EXECUTION_DRIVER_NAME,
                            EXECUTION_DRIVER_NAME,
                            JAVASCRIPT_COMMAND,
                            vec![String::from(JAVASCRIPT_COMMAND)],
                            VirtualProcessOptions {
                                env: guest_env.clone(),
                                ..Default::default()
                            },
                        )
                        .expect("create rollback parent");
                    let mut root = ActiveProcess::new(
                        root_handle.pid(),
                        root_handle,
                        vm.runtime_context.clone(),
                        vm.limits.clone(),
                        agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Binding(BindingExecution::default()),
                    );
                    let child_handle = vm
                        .kernel
                        .create_virtual_process(
                            EXECUTION_DRIVER_NAME,
                            EXECUTION_DRIVER_NAME,
                            JAVASCRIPT_COMMAND,
                            vec![String::from(JAVASCRIPT_COMMAND)],
                            VirtualProcessOptions {
                                parent_pid: Some(root.kernel_pid),
                                env: guest_env,
                                ..Default::default()
                            },
                        )
                        .expect("create rollback child");
                    let child_pid = child_handle.pid();
                    root.child_processes.insert(
                        child_id.clone(),
                        ActiveProcess::new(
                            child_pid,
                            child_handle,
                            vm.runtime_context.clone(),
                            vm.limits.clone(),
                            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
                            GuestRuntimeKind::JavaScript,
                            ActiveExecution::Binding(BindingExecution::default()),
                        ),
                    );
                    vm.active_processes.insert(process_id.clone(), root);
                    (
                        sidecar.vms.handle(&vm_id).expect("rollback VM handle"),
                        child_pid,
                    )
                };

                let pending = PendingOwnedRegisteredChild::new(
                    vm_handle.clone(),
                    process_id.clone(),
                    Vec::new(),
                    child_id.clone(),
                );
                drop(pending);

                vm_handle
                    .try_read("verify Python subprocess rollback", |vm| {
                        assert!(vm
                            .active_processes
                            .get(&process_id)
                            .is_some_and(|root| !root.child_processes.contains_key(&child_id)));
                        assert!(vm
                            .kernel
                            .list_processes()
                            .get(&child_pid)
                            .is_none_or(|process| process.status == ProcessStatus::Exited));
                    })
                    .expect("verify Python subprocess rollback state");
            })
            .await;
    }
}
