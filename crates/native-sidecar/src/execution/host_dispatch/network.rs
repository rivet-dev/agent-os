use super::*;
use agentos_kernel::poll::{PollEvents, PollFd};
use agentos_kernel::socket_table::SocketType;
use std::time::Instant;

pub(super) struct NetworkCapability;

impl SidecarHostCapability<NetworkOperation> for NetworkCapability {
    fn requires_claim(operation: &NetworkOperation) -> bool {
        matches!(
            operation,
            NetworkOperation::SocketPair { .. } | NetworkOperation::Shutdown { .. }
        )
    }

    fn execute(
        kernel: &mut SidecarKernel,
        process: &mut ActiveProcess,
        operation: NetworkOperation,
    ) -> Result<HostCallReply, HostServiceError> {
        match operation {
            NetworkOperation::SocketPair {
                kind,
                nonblocking,
                close_on_exec,
            } => {
                let socket_type = match kind {
                    SocketKind::Stream => SocketType::Stream,
                    SocketKind::Datagram => SocketType::Datagram,
                    SocketKind::SeqPacket => SocketType::SeqPacket,
                };
                let (first_fd, second_fd) = kernel
                    .fd_socketpair(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        socket_type,
                        nonblocking,
                        close_on_exec,
                    )
                    .map_err(kernel_host_error)?;
                Ok(HostCallReply::Json(json!({
                    "firstFd": first_fd,
                    "secondFd": second_fd,
                })))
            }
            NetworkOperation::Shutdown { fd, how } => {
                let how = match how {
                    SocketShutdown::Read => KernelSocketShutdown::Read,
                    SocketShutdown::Write => KernelSocketShutdown::Write,
                    SocketShutdown::Both => KernelSocketShutdown::Both,
                };
                kernel
                    .fd_socket_shutdown(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, how)
                    .map_err(kernel_host_error)?;
                Ok(HostCallReply::Json(Value::Null))
            }
            other => Err(unsupported("network", other)),
        }
    }
}

/// Typed kernel poll: owner-thread probes are synchronous and waits retain
/// only the cloneable notifier, an absolute deadline, typed interests, and the
/// generation-bound direct reply capability.
pub(super) fn dispatch_context_kernel_poll<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    operation: NetworkOperation,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(());
    }
    let NetworkOperation::KernelPoll {
        interests,
        timeout_ms,
    } = operation
    else {
        return Err(SidecarError::host(
            "EINVAL",
            "kernel poll dispatcher received a different network operation",
        ));
    };

    let deadline = match timeout_ms
        .map(checked_deferred_guest_wait_deadline)
        .transpose()
    {
        Ok(deadline) => deadline,
        Err(error) => {
            reply.fail(error).map_err(SidecarError::from)?;
            return Ok(());
        }
    };
    let notify = Arc::clone(&sidecar.process_event_notify);
    let vm = sidecar
        .vms
        .get_mut(vm_id)
        .expect("validated kernel-poll VM remains registered");
    let runtime = vm.runtime_context.clone();
    let wait_handle = vm.kernel.poll_wait_handle();
    let generation = vm.generation;
    let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
    let process = active_processes
        .get_mut(process_id)
        .expect("validated kernel-poll process remains registered");
    service_deferred_kernel_poll(
        generation,
        &runtime,
        wait_handle,
        notify,
        kernel,
        process,
        Some((interests, deadline, reply)),
    )
}

pub(super) fn typed_kernel_poll_response(
    kernel: &SidecarKernel,
    kernel_pid: u32,
    interests: &[KernelPollInterest],
) -> Result<Value, SidecarError> {
    let fds = interests
        .iter()
        .map(|entry| PollFd {
            fd: entry.fd,
            events: PollEvents::from_bits(entry.events),
            revents: PollEvents::empty(),
        })
        .collect();
    let result = kernel
        .poll_fds(EXECUTION_DRIVER_NAME, kernel_pid, fds, 0)
        .map_err(kernel_error)?;
    Ok(json!({
        "readyCount": result.ready_count,
        "fds": result.fds.into_iter().map(|entry| json!({
            "fd": entry.fd,
            "events": entry.events.bits(),
            "revents": entry.revents.bits(),
        })).collect::<Vec<_>>(),
    }))
}

/// Park a descendant executor's typed kernel poll without blocking the
/// sidecar actor or a Tokio worker. Readiness is always re-probed on the owner
/// thread; the spawned task retains only the cloneable kernel notifier and an
/// optional absolute deadline.
pub(in crate::execution) fn service_deferred_kernel_poll(
    generation: u64,
    runtime: &agentos_runtime::RuntimeContext,
    wait_handle: agentos_kernel::poll::PollWaitHandle,
    notify: Arc<tokio::sync::Notify>,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    incoming: Option<(
        BoundedVec<KernelPollInterest>,
        Option<Instant>,
        DirectHostReplyHandle,
    )>,
) -> Result<(), SidecarError> {
    let newly_admitted = incoming.is_some();
    if let Some((interests, deadline, reply)) = incoming {
        if process.deferred_kernel_poll.is_some() {
            reply
                .fail(HostServiceError::new(
                    "EBUSY",
                    "process already owns a deferred kernel poll",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        let identity = reply.identity();
        if identity.generation != generation || identity.pid != process.kernel_pid {
            reply
                .fail(HostServiceError::new(
                    "ESTALE",
                    "deferred kernel poll identity does not match the active kernel process",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        if !reply.claim().map_err(SidecarError::from)? {
            return Ok(());
        }
        process.deferred_kernel_poll = Some(DeferredKernelPoll {
            interests,
            reply,
            deadline,
            wake_task: None,
            temporary_signal_mask_token: None,
            temporary_signal_thread_id: None,
            combined: false,
        });
    }

    let now = Instant::now();
    let should_probe = process.deferred_kernel_poll.as_ref().is_some_and(|poll| {
        newly_admitted
            || poll.deadline.is_some_and(|deadline| now >= deadline)
            || poll
                .wake_task
                .as_ref()
                .is_none_or(tokio::task::JoinHandle::is_finished)
    });
    if !should_probe {
        return Ok(());
    }

    let mut poll = process
        .deferred_kernel_poll
        .take()
        .expect("deferred kernel poll checked above");
    if let Some(task) = poll.wake_task.take() {
        task.abort();
    }
    // Snapshot before probing so a readiness edge racing the probe cannot be
    // absorbed before the off-actor waiter starts.
    let observed = wait_handle.snapshot();
    let response =
        match typed_kernel_poll_response(kernel, process.kernel_pid, poll.interests.as_slice()) {
            Ok(response) => response,
            Err(error) => {
                return poll
                    .reply
                    .fail(host_service_error(&error))
                    .map_err(SidecarError::from);
            }
        };
    let ready = response
        .get("readyCount")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        > 0;
    if ready || poll.deadline.is_some_and(|deadline| now >= deadline) {
        return poll
            .reply
            .succeed(HostCallReply::Json(response))
            .map_err(SidecarError::from);
    }

    let deadline = poll.deadline;
    let wake_task = runtime.spawn(agentos_runtime::TaskClass::Vm, async move {
        match deadline {
            Some(deadline) => {
                let delay = deadline.saturating_duration_since(Instant::now());
                tokio::select! {
                    _ = wait_handle.wait_for_change_async(observed) => {}
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            None => {
                wait_handle.wait_for_change_async(observed).await;
            }
        }
        notify.notify_one();
    });
    match wake_task {
        Ok(task) => {
            poll.wake_task = Some(task);
            process.deferred_kernel_poll = Some(poll);
            Ok(())
        }
        Err(error) => poll
            .reply
            .fail(host_service_error(&SidecarError::from(error)))
            .map_err(SidecarError::from),
    }
}
