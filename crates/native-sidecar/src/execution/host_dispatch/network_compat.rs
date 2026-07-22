use super::*;
use crate::state::{
    DeferredRpcError, ManagedHostNetDescription, ManagedHostNetDescriptionRegistry,
    ManagedHostNetRoute, ManagedStreamReadRecheck, ManagedUdpPollRecheck,
};
use agentos_execution::host::SocketDomain as HostSocketDomain;
use agentos_kernel::process_table::SignalSet;
use agentos_kernel::socket_table::SocketType;
use std::time::{Duration, Instant};

const MANAGED_ID_BYTES: usize = 256;
const HOST_BYTES: usize = 253;
const UNIX_PATH_BYTES: usize = 4096;

const POSIX_POLLIN: u16 = 0x001;
const POSIX_POLLOUT: u16 = 0x004;
const POSIX_POLLNVAL: u16 = 0x020;
const POSIX_POLLRDNORM: u16 = 0x040;
const POSIX_POLLWRNORM: u16 = 0x100;
const POSIX_READ_EVENTS: u16 = POSIX_POLLIN | POSIX_POLLRDNORM;
const POSIX_WRITE_EVENTS: u16 = POSIX_POLLOUT | POSIX_POLLWRNORM;

/// Durable return lane for an off-owner POSIX-poll readiness/deadline task.
///
/// `process_event_notify` is a coalesced broker shared by the owner pump and
/// readiness waiters, so a notification alone can be consumed by a different
/// waiter. Queueing an internal event first preserves the wake until the owner
/// lane re-enters and probes the process-owned poll state.
#[derive(Clone)]
pub(in crate::execution) struct DeferredPosixPollWakeLane {
    sender: tokio::sync::mpsc::Sender<ProcessEventEnvelope>,
    notify: Arc<tokio::sync::Notify>,
    connection_id: String,
    session_id: String,
    vm_id: String,
    process_id: String,
}

impl DeferredPosixPollWakeLane {
    async fn publish(self) {
        let envelope = ProcessEventEnvelope {
            connection_id: self.connection_id,
            session_id: self.session_id,
            vm_id: self.vm_id,
            process_id: self.process_id,
            event: ActiveExecutionEvent::DeferredPosixPollWake,
        };
        // Wake the owner before waiting for bounded queue admission. If the
        // lane is already full, the owner may be asleep with the event that
        // would free capacity sitting in this queue. Notifying only after
        // `send` would then delay a poll deadline until some unrelated wake.
        self.notify.notify_waiters();
        self.notify.notify_one();
        if let Err(error) = self.sender.send(envelope).await {
            eprintln!(
                "ERR_AGENTOS_POSIX_POLL_WAKE_DROPPED: owner event lane closed before deferred poll wake: {error}"
            );
            return;
        }
        // `Notify` is shared by the owner pump and the bounded set of active
        // deferred waiters. Wake every waiter already registered so one poll
        // cannot steal another poll's deadline edge, then retain one coalesced
        // permit for an owner pump currently between select turns.
        self.notify.notify_waiters();
        self.notify.notify_one();
    }
}

pub(in crate::execution) fn deferred_posix_poll_wake_lane<B>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
) -> Result<DeferredPosixPollWakeLane, SidecarError> {
    let vm = sidecar
        .vms
        .get(vm_id)
        .ok_or_else(|| missing_vm_error(vm_id))?;
    Ok(DeferredPosixPollWakeLane {
        sender: sidecar.process_event_sender.clone(),
        notify: Arc::clone(&sidecar.process_event_notify),
        connection_id: vm.connection_id.clone(),
        session_id: vm.session_id.clone(),
        vm_id: vm_id.to_owned(),
        process_id: process_id.to_owned(),
    })
}

fn posix_signal_set(set: SignalSetValue) -> Result<SignalSet, SidecarError> {
    SignalSet::from_signals((1..=64).filter(|signal| set.0 & (1_u64 << (signal - 1)) != 0))
        .map_err(|error| SidecarError::host(error.code(), error.to_string()))
}

fn managed_posix_poll_response(
    socket_paths: &SocketPathContext,
    kernel_readiness: KernelSocketReadinessRegistry,
    capabilities: CapabilityRegistry,
    managed_descriptions: &ManagedHostNetDescriptionRegistry,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    interests: &[KernelPollInterest],
) -> Result<Value, SidecarError> {
    let mut kernel_interests = Vec::new();
    let mut kernel_interest_indexes = Vec::new();
    let mut revents_by_index = vec![None; interests.len()];
    let trace_enabled = net_tcp_trace_enabled(&process.env);

    for (index, interest) in interests.iter().enumerate() {
        let description_id = match kernel.fd_description_identity(
            EXECUTION_DRIVER_NAME,
            process.kernel_pid,
            interest.fd,
        ) {
            Ok((description_id, _)) => description_id,
            Err(error) if error.code() == "EBADF" => {
                revents_by_index[index] = Some(POSIX_POLLNVAL);
                continue;
            }
            Err(error) => return Err(kernel_error(error)),
        };
        let route = managed_descriptions
            .lock()
            .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?
            .get(&description_id)
            .and_then(|description| description.route_for(process.kernel_pid).cloned());
        let Some(route) = route else {
            kernel_interests.push(*interest);
            kernel_interest_indexes.push(index);
            continue;
        };

        let mut revents = 0_u16;
        if interest.events & POSIX_WRITE_EVENTS != 0
            && matches!(
                route,
                ManagedHostNetRoute::TcpSocket(_)
                    | ManagedHostNetRoute::UnixSocket(_)
                    | ManagedHostNetRoute::UdpSocket(_)
            )
        {
            revents |= interest.events & POSIX_WRITE_EVENTS;
        }
        if interest.events & POSIX_READ_EVENTS != 0 {
            let readable = match route {
                ManagedHostNetRoute::TcpSocket(ref socket_id)
                | ManagedHostNetRoute::UnixSocket(ref socket_id) => {
                    let mut context = ManagedNetworkServiceContext {
                        vm_id: "posix-poll",
                        socket_paths,
                        kernel,
                        kernel_readiness: kernel_readiness.clone(),
                        process,
                        capabilities: capabilities.clone(),
                    };
                    probe_managed_socket_readable(&mut context, socket_id)?
                }
                ManagedHostNetRoute::TcpListener(ref listener_id) => process
                    .tcp_listeners
                    .get_mut(listener_id)
                    .ok_or_else(|| SidecarError::host("EBADF", "managed TCP listener is stale"))?
                    .probe_readable(kernel, process.kernel_pid, trace_enabled)?,
                ManagedHostNetRoute::UnixListener(ref listener_id) => process
                    .unix_listeners
                    .get_mut(listener_id)
                    .ok_or_else(|| SidecarError::host("EBADF", "managed Unix listener is stale"))?
                    .probe_readable()?,
                ManagedHostNetRoute::UdpSocket(ref socket_id) => {
                    let socket = process.udp_sockets.get(socket_id).ok_or_else(|| {
                        SidecarError::host("EBADF", "managed UDP socket is stale")
                    })?;
                    socket
                        .pending_datagram
                        .lock()
                        .map_err(|_| {
                            SidecarError::host("EIO", "UDP pending datagram lock poisoned")
                        })?
                        .is_some()
                        || socket
                            .native_read_wake_pending
                            .load(std::sync::atomic::Ordering::Acquire)
                        || socket.kernel_readable(kernel, process.kernel_pid)?
                }
                ManagedHostNetRoute::Unbound
                | ManagedHostNetRoute::TcpBound { .. }
                | ManagedHostNetRoute::UnixBound { .. } => false,
            };
            if readable {
                revents |= interest.events & POSIX_READ_EVENTS;
            }
        }
        revents_by_index[index] = Some(revents);
    }

    let kernel_response =
        super::network::typed_kernel_poll_response(kernel, process.kernel_pid, &kernel_interests)?;
    let kernel_fds = kernel_response
        .get("fds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (kernel_index, interest_index) in kernel_interest_indexes.into_iter().enumerate() {
        revents_by_index[interest_index] = Some(
            kernel_fds
                .get(kernel_index)
                .and_then(|entry| entry.get("revents"))
                .and_then(Value::as_u64)
                .and_then(|value| u16::try_from(value).ok())
                .unwrap_or_default(),
        );
    }
    Ok(indexed_posix_poll_response(interests, &revents_by_index))
}

fn indexed_posix_poll_response(
    interests: &[KernelPollInterest],
    revents_by_index: &[Option<u16>],
) -> Value {
    debug_assert_eq!(interests.len(), revents_by_index.len());
    let mut ready_count = 0_u64;
    let fds = interests
        .iter()
        .zip(revents_by_index)
        .map(|(interest, revents)| {
            let revents = revents.unwrap_or_default();
            if revents != 0 {
                ready_count += 1;
            }
            json!({
                "fd": interest.fd,
                "events": interest.events,
                "revents": revents,
            })
        })
        .collect::<Vec<_>>();
    json!({ "readyCount": ready_count, "fds": fds })
}

fn native_tcp_listener_waiters(
    managed_descriptions: &ManagedHostNetDescriptionRegistry,
    kernel: &SidecarKernel,
    process: &ActiveProcess,
    interests: &[KernelPollInterest],
) -> Result<Vec<tokio::io::unix::AsyncFd<std::net::TcpListener>>, SidecarError> {
    let descriptions = managed_descriptions
        .lock()
        .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?;
    let mut waiters = Vec::new();
    for interest in interests
        .iter()
        .filter(|interest| interest.events & POSIX_READ_EVENTS != 0)
    {
        let Ok((description_id, _)) =
            kernel.fd_description_identity(EXECUTION_DRIVER_NAME, process.kernel_pid, interest.fd)
        else {
            continue;
        };
        let Some(ManagedHostNetRoute::TcpListener(listener_id)) = descriptions
            .get(&description_id)
            .and_then(|description| description.route_for(process.kernel_pid))
        else {
            continue;
        };
        let Some(listener) = process
            .tcp_listeners
            .get(listener_id)
            .and_then(|listener| listener.listener.as_ref())
        else {
            continue;
        };
        waiters.push(
            tokio::io::unix::AsyncFd::new(listener.try_clone().map_err(|error| {
                SidecarError::host("EIO", format!("clone TCP listener for poll: {error}"))
            })?)
            .map_err(|error| {
                SidecarError::host("EIO", format!("register TCP listener poll waiter: {error}"))
            })?,
        );
    }
    Ok(waiters)
}

fn managed_posix_poll_read_notifies(
    managed_descriptions: &ManagedHostNetDescriptionRegistry,
    kernel: &SidecarKernel,
    process: &ActiveProcess,
    interests: &[KernelPollInterest],
) -> Result<Vec<Arc<tokio::sync::Notify>>, SidecarError> {
    let descriptions = managed_descriptions
        .lock()
        .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?;
    let mut notifies = Vec::new();
    for interest in interests
        .iter()
        .filter(|interest| interest.events & POSIX_READ_EVENTS != 0)
    {
        let Ok((description_id, _)) =
            kernel.fd_description_identity(EXECUTION_DRIVER_NAME, process.kernel_pid, interest.fd)
        else {
            continue;
        };
        let Some(route) = descriptions
            .get(&description_id)
            .and_then(|description| description.route_for(process.kernel_pid))
        else {
            continue;
        };
        let notify = match route {
            ManagedHostNetRoute::TcpSocket(socket_id) => process
                .tcp_sockets
                .get(socket_id)
                .map(|socket| Arc::clone(&socket.read_event_notify)),
            ManagedHostNetRoute::UnixSocket(socket_id) => process
                .unix_sockets
                .get(socket_id)
                .map(|socket| Arc::clone(&socket.read_event_notify)),
            ManagedHostNetRoute::UdpSocket(socket_id) => process
                .udp_sockets
                .get(socket_id)
                .map(|socket| Arc::clone(&socket.read_event_notify)),
            ManagedHostNetRoute::Unbound
            | ManagedHostNetRoute::TcpBound { .. }
            | ManagedHostNetRoute::UnixBound { .. }
            | ManagedHostNetRoute::TcpListener(_)
            | ManagedHostNetRoute::UnixListener(_) => None,
        };
        if let Some(notify) = notify {
            if !notifies
                .iter()
                .any(|existing| Arc::ptr_eq(existing, &notify))
            {
                notifies.push(notify);
            }
        }
    }
    Ok(notifies)
}

async fn wait_for_managed_posix_poll_readiness(notifies: Vec<Arc<tokio::sync::Notify>>) {
    if notifies.is_empty() {
        std::future::pending::<()>().await;
        return;
    }
    let mut waiters = notifies
        .into_iter()
        .map(|notify| Box::pin(notify.notified_owned()))
        .collect::<Vec<_>>();
    std::future::poll_fn(move |context| {
        if waiters
            .iter_mut()
            .any(|waiter| std::future::Future::poll(waiter.as_mut(), context).is_ready())
        {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
}

pub(super) fn dispatch_context_posix_poll<B>(
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
    let NetworkOperation::PosixPoll {
        interests,
        timeout_ms,
        signal_mask,
        signal_thread_id,
    } = operation
    else {
        return Err(SidecarError::host(
            "EINVAL",
            "POSIX poll dispatcher received a different network operation",
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
    let wake_lane = deferred_posix_poll_wake_lane(sidecar, vm_id, process_id)?;
    let socket_paths = build_socket_path_context(
        sidecar
            .vms
            .get(vm_id)
            .expect("validated POSIX-poll VM remains registered"),
    )?;
    let notify = Arc::clone(&sidecar.process_event_notify);
    let vm = sidecar
        .vms
        .get_mut(vm_id)
        .expect("validated POSIX-poll VM remains registered");
    let generation = vm.generation;
    let runtime = vm.runtime_context.clone();
    let wait_handle = vm.kernel.poll_wait_handle();
    let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
    let capabilities = vm.capabilities.clone();
    let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
    let kernel_pid = reply.identity().pid;
    let process = active_process_by_kernel_pid_mut(&mut vm.active_processes, kernel_pid)
        .ok_or_else(|| {
            SidecarError::host(
                "ESTALE",
                format!("active process for kernel pid {kernel_pid} disappeared"),
            )
        })?;
    service_deferred_posix_poll(
        generation,
        &runtime,
        wait_handle,
        notify,
        &socket_paths,
        kernel_readiness,
        capabilities,
        managed_descriptions,
        wake_lane,
        &mut vm.kernel,
        process,
        Some((interests, deadline, signal_mask, signal_thread_id, reply)),
    )
}

fn restore_deferred_posix_mask(
    process: &mut ActiveProcess,
    poll: &mut DeferredKernelPoll,
) -> Result<(), HostServiceError> {
    let Some(token) = poll.temporary_signal_mask_token.take() else {
        return Ok(());
    };
    let result = if let Some(thread_id) = poll.temporary_signal_thread_id.take() {
        process
            .kernel_handle
            .end_temporary_signal_mask_for_thread(thread_id, token)
    } else {
        process.kernel_handle.end_temporary_signal_mask(token)
    };
    result.map_err(|error| HostServiceError::new(error.code(), error.to_string()))
}

fn fail_deferred_posix_poll(
    process: &mut ActiveProcess,
    mut poll: DeferredKernelPoll,
    failure: HostServiceError,
) -> Result<(), SidecarError> {
    let failure = match restore_deferred_posix_mask(process, &mut poll) {
        Ok(()) => failure,
        Err(restore_error) => {
            eprintln!(
                "ERR_AGENTOS_PPOLL_MASK_RESTORE: {}; original poll failure: {}",
                restore_error, failure
            );
            restore_error
        }
    };
    poll.reply.fail(failure).map_err(SidecarError::from)
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub(in crate::execution) fn service_deferred_posix_poll(
    generation: u64,
    runtime: &agentos_runtime::RuntimeContext,
    wait_handle: agentos_kernel::poll::PollWaitHandle,
    notify: Arc<tokio::sync::Notify>,
    socket_paths: &SocketPathContext,
    kernel_readiness: KernelSocketReadinessRegistry,
    capabilities: CapabilityRegistry,
    managed_descriptions: ManagedHostNetDescriptionRegistry,
    wake_lane: DeferredPosixPollWakeLane,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    incoming: Option<(
        BoundedVec<KernelPollInterest>,
        Option<Instant>,
        Option<SignalSetValue>,
        Option<u32>,
        DirectHostReplyHandle,
    )>,
) -> Result<(), SidecarError> {
    let newly_admitted = incoming.is_some();
    if let Some((interests, deadline, signal_mask, signal_thread_id, reply)) = incoming {
        if process.deferred_kernel_poll.is_some() {
            reply
                .fail(HostServiceError::new(
                    "EBUSY",
                    "process already owns a deferred POSIX poll",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        let identity = reply.identity();
        if identity.generation != generation || identity.pid != process.kernel_pid {
            reply
                .fail(HostServiceError::new(
                    "ESTALE",
                    "deferred POSIX poll identity does not match the active kernel process",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        // A checkpoint published before admission already owns a handler frame;
        // a temporary mask cannot retroactively block it.
        if process.guest_signal_checkpoint_pending || process.runtime_control.pending().checkpoint {
            reply
                .fail(HostServiceError::new(
                    "EINTR",
                    "caught signal was pending before POSIX poll admission",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        if !reply.claim().map_err(SidecarError::from)? {
            return Ok(());
        }
        let temporary_signal_mask_token = match signal_mask {
            Some(mask) => {
                let mask = match posix_signal_set(mask) {
                    Ok(mask) => mask,
                    Err(error) => {
                        reply
                            .fail(host_service_error(&error))
                            .map_err(SidecarError::from)?;
                        return Ok(());
                    }
                };
                let result = if let Some(thread_id) = signal_thread_id {
                    process
                        .kernel_handle
                        .begin_temporary_signal_mask_for_thread(thread_id, mask)
                } else {
                    process.kernel_handle.begin_temporary_signal_mask(mask)
                };
                match result {
                    Ok(token) => Some(token),
                    Err(error) => {
                        reply
                            .fail(HostServiceError::new(error.code(), error.to_string()))
                            .map_err(SidecarError::from)?;
                        return Ok(());
                    }
                }
            }
            None => None,
        };
        process.deferred_kernel_poll = Some(DeferredKernelPoll {
            interests,
            reply,
            deadline,
            wake_task: None,
            temporary_signal_mask_token,
            temporary_signal_thread_id: temporary_signal_mask_token.and(signal_thread_id),
            combined: true,
        });
        // Installing a ppoll mask can make a previously blocked signal
        // deliverable. Publish it while the guest is still parked.
        if let Err(error) = process.apply_runtime_controls() {
            if let Some(poll) = process.clear_deferred_kernel_poll() {
                fail_deferred_posix_poll(
                    process,
                    poll,
                    HostServiceError::new(
                        "EIO",
                        format!("failed to publish POSIX-poll signal checkpoint: {error}"),
                    ),
                )?;
            }
            return Err(error);
        }
        if process.deferred_kernel_poll.is_none() {
            return Ok(());
        }
    }

    let now = Instant::now();
    let should_probe = process.deferred_kernel_poll.as_ref().is_some_and(|poll| {
        poll.combined
            && (newly_admitted
                || poll.deadline.is_some_and(|deadline| now >= deadline)
                || poll
                    .wake_task
                    .as_ref()
                    .is_none_or(tokio::task::JoinHandle::is_finished))
    });
    if !should_probe {
        return Ok(());
    }
    let mut poll = process
        .deferred_kernel_poll
        .take()
        .expect("deferred POSIX poll checked above");
    if let Some(task) = poll.wake_task.take() {
        task.abort();
    }
    let observed = wait_handle.snapshot();
    let response = managed_posix_poll_response(
        socket_paths,
        kernel_readiness,
        capabilities,
        &managed_descriptions,
        kernel,
        process,
        poll.interests.as_slice(),
    );
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return fail_deferred_posix_poll(process, poll, host_service_error(&error));
        }
    };
    let ready = response
        .get("readyCount")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        > 0;
    if ready || poll.deadline.is_some_and(|deadline| now >= deadline) {
        if let Err(error) = restore_deferred_posix_mask(process, &mut poll) {
            return poll.reply.fail(error).map_err(SidecarError::from);
        }
        if poll.combined {
            // Publish signals released only by mask restoration before the
            // successful result wakes guest code. Such a signal does not
            // rewrite the already-observed poll result to EINTR.
            if let Err(error) = process.apply_runtime_controls() {
                poll.reply
                    .fail(HostServiceError::new(
                        "EIO",
                        format!("failed to publish restored ppoll signal checkpoint: {error}"),
                    ))
                    .map_err(SidecarError::from)?;
                return Err(error);
            }
        }
        return poll
            .reply
            .succeed(HostCallReply::Json(response))
            .map_err(SidecarError::from);
    }

    let deadline = poll.deadline;
    let native_listener_waiters = match native_tcp_listener_waiters(
        &managed_descriptions,
        kernel,
        process,
        poll.interests.as_slice(),
    ) {
        Ok(waiters) => waiters,
        Err(error) => {
            return fail_deferred_posix_poll(process, poll, host_service_error(&error));
        }
    };
    let managed_read_notifies = match managed_posix_poll_read_notifies(
        &managed_descriptions,
        kernel,
        process,
        poll.interests.as_slice(),
    ) {
        Ok(notifies) => notifies,
        Err(error) => {
            return fail_deferred_posix_poll(process, poll, host_service_error(&error));
        }
    };
    let task_notify = Arc::clone(&notify);
    let task_wake_lane = wake_lane.clone();
    let wake_task = runtime.spawn(agentos_runtime::TaskClass::Vm, async move {
        let native_listener_ready = std::future::poll_fn(|cx| {
            if native_listener_waiters.is_empty() {
                return std::task::Poll::Pending;
            }
            for waiter in &native_listener_waiters {
                if waiter.poll_read_ready(cx).is_ready() {
                    return std::task::Poll::Ready(());
                }
            }
            std::task::Poll::Pending
        });
        tokio::pin!(native_listener_ready);
        let managed_read_ready = wait_for_managed_posix_poll_readiness(managed_read_notifies);
        tokio::pin!(managed_read_ready);
        match deadline {
            Some(deadline) => {
                let delay = deadline.saturating_duration_since(Instant::now());
                tokio::select! {
                    _ = wait_handle.wait_for_change_async(observed) => {}
                    _ = task_notify.notified() => {}
                    _ = &mut native_listener_ready => {}
                    _ = &mut managed_read_ready => {}
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            None => {
                tokio::select! {
                    _ = wait_handle.wait_for_change_async(observed) => {}
                    _ = task_notify.notified() => {}
                    _ = &mut native_listener_ready => {}
                    _ = &mut managed_read_ready => {}
                }
            }
        }
        task_wake_lane.publish().await;
    });
    match wake_task {
        Ok(task) => {
            poll.wake_task = Some(task);
            process.deferred_kernel_poll = Some(poll);
            Ok(())
        }
        Err(error) => fail_deferred_posix_poll(
            process,
            poll,
            host_service_error(&SidecarError::from(error)),
        ),
    }
}

pub(super) fn dispatch_context_close_with_managed_retirement<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    fd: u32,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(());
    }
    let kernel_pid = reply.identity().pid;
    let socket_paths = build_socket_path_context(
        sidecar
            .vms
            .get(vm_id)
            .expect("validated close VM remains registered"),
    )?;
    if !reply.claim().map_err(SidecarError::from)? {
        return Ok(());
    }
    let bridge = sidecar.bridge.clone();
    let vm = sidecar
        .vms
        .get_mut(vm_id)
        .expect("validated close VM remains registered");
    let result = close_with_managed_retirement(&bridge, vm_id, &socket_paths, vm, kernel_pid, fd);
    match result {
        Ok(response) => reply.succeed(response).map_err(SidecarError::from),
        Err(error) => reply
            .fail(host_service_error(&error))
            .map_err(SidecarError::from),
    }
}

pub(super) fn dispatch_context_fd_snapshot<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(());
    }
    let pid = reply.identity().pid;
    if !reply.claim().map_err(SidecarError::from)? {
        return Ok(());
    }
    let vm = sidecar
        .vms
        .get(vm_id)
        .expect("validated fd-snapshot VM remains registered");
    let result = fd_snapshot_with_managed_routes(vm, pid);
    match result {
        Ok(response) => reply.succeed(response).map_err(SidecarError::from),
        Err(error) => reply
            .fail(host_service_error(&error))
            .map_err(SidecarError::from),
    }
}

pub(in crate::execution) fn fd_snapshot_with_managed_routes(
    vm: &VmState,
    pid: u32,
) -> Result<HostCallReply, SidecarError> {
    let entries = vm
        .kernel
        .fd_snapshot(EXECUTION_DRIVER_NAME, pid)
        .map_err(kernel_error)?;
    let managed_ids = vm
        .managed_host_net_descriptions
        .lock()
        .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?
        .iter()
        .filter_map(|(description_id, description)| {
            description
                .route_for(pid)
                .is_some()
                .then_some(*description_id)
        })
        .collect::<BTreeSet<_>>();
    Ok(HostCallReply::Json(Value::Array(
        entries
            .into_iter()
            .map(|entry| {
                json!({
                    "fd": entry.fd,
                    "descriptionId": entry.description_id.to_string(),
                    "managedHostNet": managed_ids.contains(&entry.description_id),
                    "fdFlags": entry.fd_flags,
                    "statusFlags": entry.status_flags,
                    "filetype": entry.filetype,
                    "rightsBase": entry.rights_base,
                    "rightsInheriting": entry.rights_inheriting,
                    "kind": if entry.is_socket {
                        "socket"
                    } else if entry.is_pipe {
                        "pipe"
                    } else if entry.is_pty {
                        "pty"
                    } else {
                        "file"
                    },
                })
            })
            .collect(),
    )))
}

pub(in crate::execution) fn close_with_managed_retirement<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    socket_paths: &SocketPathContext,
    vm: &mut VmState,
    kernel_pid: u32,
    fd: u32,
) -> Result<HostCallReply, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let description_id = vm
        .kernel
        .fd_description_identity(EXECUTION_DRIVER_NAME, kernel_pid, fd)
        .ok()
        .map(|identity| identity.0);
    vm.kernel
        .fd_close(EXECUTION_DRIVER_NAME, kernel_pid, fd)
        .map_err(kernel_error)?;
    prune_managed_descriptions_after_fd_mutation(
        bridge,
        vm_id,
        socket_paths,
        vm,
        kernel_pid,
        description_id,
    )?;
    Ok(HostCallReply::Json(Value::Null))
}

pub(super) fn dispatch_context_closefrom_with_managed_retirement<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    min_fd: u32,
    exact_fds: Option<BoundedVec<u32>>,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(());
    }
    let kernel_pid = reply.identity().pid;
    let socket_paths = build_socket_path_context(
        sidecar
            .vms
            .get(vm_id)
            .expect("validated closefrom VM remains registered"),
    )?;
    if !reply.claim().map_err(SidecarError::from)? {
        return Ok(());
    }
    let bridge = sidecar.bridge.clone();
    let vm = sidecar
        .vms
        .get_mut(vm_id)
        .expect("validated closefrom VM remains registered");
    let response = closefrom_with_managed_retirement(
        &bridge,
        vm_id,
        &socket_paths,
        vm,
        kernel_pid,
        min_fd,
        exact_fds,
    );
    match response {
        Ok(response) => reply.succeed(response).map_err(SidecarError::from),
        Err(error) => reply
            .fail(host_service_error(&error))
            .map_err(SidecarError::from),
    }
}

pub(in crate::execution) fn closefrom_with_managed_retirement<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    socket_paths: &SocketPathContext,
    vm: &mut VmState,
    kernel_pid: u32,
    min_fd: u32,
    exact_fds: Option<BoundedVec<u32>>,
) -> Result<HostCallReply, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if let (Some(fds), Some(limit)) = (exact_fds.as_ref(), vm.kernel.resource_limits().max_open_fds)
    {
        if fds.len() > limit {
            return Err(SidecarError::host(
                "E2BIG",
                format!(
                    "fd_closefrom canonical target list has {} entries, exceeding limits.resources.maxOpenFds ({limit}); raise limits.resources.maxOpenFds",
                    fds.len()
                ),
            ));
        }
    }
    let exact_set = exact_fds
        .as_ref()
        .map(|fds| fds.as_slice().iter().copied().collect::<BTreeSet<_>>());
    let candidate_descriptions = vm
        .kernel
        .fd_snapshot(EXECUTION_DRIVER_NAME, kernel_pid)
        .map_err(kernel_error)?
        .into_iter()
        .filter_map(|entry| {
            exact_set
                .as_ref()
                .map_or(entry.fd >= min_fd, |fds| fds.contains(&entry.fd))
                .then_some(entry.description_id)
        })
        .collect::<BTreeSet<_>>();

    // `fd_close_from` removes all matching table entries before it performs
    // fallible resource cleanup. Always reconcile managed routes from the
    // post-mutation alias counts, including that cleanup-error path.
    let close_result = if let Some(fds) = exact_fds {
        vm.kernel
            .fd_close_exact(EXECUTION_DRIVER_NAME, kernel_pid, fds.into_vec())
    } else {
        vm.kernel
            .fd_close_from(EXECUTION_DRIVER_NAME, kernel_pid, min_fd)
    }
    .map_err(kernel_error);
    let prune_result = prune_managed_descriptions_after_fd_mutation(
        bridge,
        vm_id,
        socket_paths,
        vm,
        kernel_pid,
        candidate_descriptions,
    );
    match (close_result, prune_result) {
        (Ok(closed_fds), Ok(())) => Ok(HostCallReply::Json(json!({
            "closedFds": closed_fds,
        }))),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

pub(super) fn dispatch_context_descriptor_replacement_with_managed_retirement<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    operation: FilesystemOperation,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(());
    }
    let pid = reply.identity().pid;
    if !reply.claim().map_err(SidecarError::from)? {
        return Ok(());
    }
    let socket_paths = build_socket_path_context(
        sidecar
            .vms
            .get(vm_id)
            .expect("validated descriptor-mutation VM remains registered"),
    )?;
    let bridge = sidecar.bridge.clone();
    let vm = sidecar
        .vms
        .get_mut(vm_id)
        .expect("validated descriptor-mutation VM remains registered");
    let result = replace_descriptor_with_managed_retirement(
        &bridge,
        vm_id,
        &socket_paths,
        vm,
        pid,
        operation,
    );
    match result {
        Ok(response) => reply.succeed(response),
        Err(error) => reply.fail(host_service_error(&error)),
    }
    .map_err(SidecarError::from)
}

pub(in crate::execution) fn replace_descriptor_with_managed_retirement<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    socket_paths: &SocketPathContext,
    vm: &mut VmState,
    pid: u32,
    operation: FilesystemOperation,
) -> Result<HostCallReply, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let replaced_description_id = match operation {
        FilesystemOperation::Renumber { to, .. }
        | FilesystemOperation::DuplicateTo { target_fd: to, .. } => vm
            .kernel
            .fd_description_identity(EXECUTION_DRIVER_NAME, pid, to)
            .ok()
            .map(|identity| identity.0),
        FilesystemOperation::Move {
            replaced_fd: Some(to),
            ..
        } => vm
            .kernel
            .fd_description_identity(EXECUTION_DRIVER_NAME, pid, to)
            .ok()
            .map(|identity| identity.0),
        FilesystemOperation::Move {
            replaced_fd: None, ..
        } => None,
        _ => {
            return Err(SidecarError::host(
                "EINVAL",
                "invalid descriptor replacement operation",
            ))
        }
    };
    let result = match operation {
        FilesystemOperation::Renumber { from, to } => vm
            .kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, pid, from, to)
            .and_then(|()| vm.kernel.fd_close(EXECUTION_DRIVER_NAME, pid, from))
            .map(|()| Value::Null),
        FilesystemOperation::DuplicateTo { fd, target_fd } => vm
            .kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, pid, fd, target_fd)
            .map(|()| Value::Null),
        FilesystemOperation::Move { fd, replaced_fd } => vm
            .kernel
            .fd_renumber_projection(EXECUTION_DRIVER_NAME, pid, fd, replaced_fd)
            .map(Value::from),
        _ => unreachable!("validated descriptor replacement operation"),
    }
    .map_err(kernel_error);
    match result {
        Ok(value) => {
            prune_managed_descriptions_after_fd_mutation(
                bridge,
                vm_id,
                socket_paths,
                vm,
                pid,
                replaced_description_id,
            )?;
            Ok(HostCallReply::Json(value))
        }
        Err(error) => Err(error),
    }
}

fn prune_managed_descriptions_after_fd_mutation<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    socket_paths: &SocketPathContext,
    vm: &mut VmState,
    kernel_pid: u32,
    candidates: impl IntoIterator<Item = u64>,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let candidates = candidates.into_iter().collect::<BTreeSet<_>>();
    if candidates.is_empty() {
        return Ok(());
    }
    let alias_counts = candidates
        .into_iter()
        .map(|description_id| {
            vm.kernel
                .fd_description_alias_count(EXECUTION_DRIVER_NAME, kernel_pid, description_id)
                .map(|aliases| (description_id, aliases))
                .map_err(kernel_error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (retired_routes, retired) = {
        let mut descriptions = vm
            .managed_host_net_descriptions
            .lock()
            .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?;
        prune_managed_registry_routes(&mut descriptions, kernel_pid, alias_counts)
    };
    for description in retired_routes {
        retire_managed_description_routes(bridge, vm_id, socket_paths, vm, description);
    }
    for description in retired {
        retire_managed_description_routes(bridge, vm_id, socket_paths, vm, description);
    }
    Ok(())
}

fn prune_managed_registry_routes(
    descriptions: &mut BTreeMap<u64, ManagedHostNetDescription>,
    kernel_pid: u32,
    alias_counts: impl IntoIterator<Item = (u64, usize)>,
) -> (
    Vec<ManagedHostNetDescription>,
    Vec<ManagedHostNetDescription>,
) {
    let mut retired_routes = Vec::new();
    let mut retired_descriptions = Vec::new();
    for (description_id, process_aliases) in alias_counts {
        if process_aliases == 0 {
            if let Some(description) = descriptions.get_mut(&description_id) {
                if let Some(route) = description.routes.remove(&kernel_pid) {
                    let mut route_description = description.clone();
                    route_description.routes.clear();
                    route_description.routes.insert(kernel_pid, route);
                    retired_routes.push(route_description);
                }
            }
        }
        if descriptions
            .get(&description_id)
            .is_some_and(|description| description.lease.ref_count() == 1)
        {
            if let Some(description) = descriptions.remove(&description_id) {
                retired_descriptions.push(description);
            }
        }
    }
    (retired_routes, retired_descriptions)
}

fn active_process_by_kernel_pid_mut(
    processes: &mut BTreeMap<String, ActiveProcess>,
    kernel_pid: u32,
) -> Option<&mut ActiveProcess> {
    for process in processes.values_mut() {
        if process.kernel_pid == kernel_pid {
            return Some(process);
        }
        if let Some(found) =
            active_process_by_kernel_pid_mut(&mut process.child_processes, kernel_pid)
        {
            return Some(found);
        }
    }
    None
}

fn retire_managed_description_routes<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    socket_paths: &SocketPathContext,
    vm: &mut VmState,
    description: ManagedHostNetDescription,
) where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let capabilities = vm.capabilities.clone();
    let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
    let dns = vm.dns.clone();
    for (kernel_pid, route) in description.routes {
        let Some(process) = active_process_by_kernel_pid_mut(&mut vm.active_processes, kernel_pid)
        else {
            continue;
        };
        let result = match route {
            ManagedHostNetRoute::Unbound => Ok(Value::Null.into()),
            ManagedHostNetRoute::TcpBound { reservation_id } => {
                process.tcp_port_reservations.remove(&reservation_id);
                Ok(Value::Null.into())
            }
            ManagedHostNetRoute::TcpSocket(socket_id)
            | ManagedHostNetRoute::UnixSocket(socket_id) => {
                service_managed_network_operation(
                    ManagedNetworkServiceContext {
                        vm_id,
                        socket_paths,
                        kernel: &mut vm.kernel,
                        kernel_readiness: kernel_readiness.clone(),
                        process,
                        capabilities: capabilities.clone(),
                    },
                    NetworkOperation::ManagedDestroy {
                        socket_id: match bounded_managed_id(socket_id) {
                            Ok(id) => id,
                            Err(error) => {
                                eprintln!("ERR_AGENTOS_SOCKET_CLEANUP: invalid retired socket id: {error}");
                                continue;
                            }
                        },
                    },
                )
            }
            ManagedHostNetRoute::TcpListener(listener_id)
            | ManagedHostNetRoute::UnixListener(listener_id)
            | ManagedHostNetRoute::UnixBound { listener_id } => {
                service_managed_network_operation(
                    ManagedNetworkServiceContext {
                        vm_id,
                        socket_paths,
                        kernel: &mut vm.kernel,
                        kernel_readiness: kernel_readiness.clone(),
                        process,
                        capabilities: capabilities.clone(),
                    },
                    NetworkOperation::ManagedCloseListener {
                        listener_id: match bounded_managed_id(listener_id) {
                            Ok(id) => id,
                            Err(error) => {
                                eprintln!("ERR_AGENTOS_SOCKET_CLEANUP: invalid retired listener id: {error}");
                                continue;
                            }
                        },
                    },
                )
            }
            ManagedHostNetRoute::UdpSocket(socket_id) => service_managed_udp_operation(
                ManagedUdpServiceRequest {
                    bridge,
                    kernel: &mut vm.kernel,
                    vm_id,
                    dns: &dns,
                    socket_paths,
                    process,
                    kernel_readiness: kernel_readiness.clone(),
                    capabilities: capabilities.clone(),
                },
                NetworkOperation::ManagedUdpClose {
                    socket_id: match bounded_managed_id(socket_id) {
                        Ok(id) => id,
                        Err(error) => {
                            eprintln!(
                                "ERR_AGENTOS_SOCKET_CLEANUP: invalid retired UDP id: {error}"
                            );
                            continue;
                        }
                    },
                },
            ),
        };
        match result {
            Ok(HostServiceResponse::Deferred { receiver, task_class, .. }) => {
                if let Err(error) = vm.runtime_context.spawn(task_class, async move {
                    if let Err(error) = receiver.await.unwrap_or_else(|_| {
                        Err(DeferredRpcError {
                            code: "ECANCELED".to_owned(),
                            message: "retired network cleanup completion channel closed".to_owned(),
                            details: None,
                        })
                    }) {
                        eprintln!(
                            "ERR_AGENTOS_SOCKET_CLEANUP: deferred retired network cleanup failed: {}: {}",
                            error.code, error.message
                        );
                    }
                }) {
                    eprintln!("ERR_AGENTOS_SOCKET_CLEANUP: failed to schedule retired cleanup: {error}");
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!(
                "ERR_AGENTOS_SOCKET_CLEANUP: failed to retire managed description route: {error}"
            ),
        }
    }
}

pub(in crate::execution) fn retire_managed_process_routes<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
    kernel_pid: u32,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let socket_paths = build_socket_path_context(vm)?;
    let retired_routes = {
        let mut descriptions = vm
            .managed_host_net_descriptions
            .lock()
            .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?;
        descriptions
            .values_mut()
            .filter_map(|description| {
                let route = description.routes.remove(&kernel_pid)?;
                let mut route_description = description.clone();
                route_description.routes.clear();
                route_description.routes.insert(kernel_pid, route);
                Some(route_description)
            })
            .collect::<Vec<_>>()
    };
    for description in retired_routes {
        retire_managed_description_routes(bridge, vm_id, &socket_paths, vm, description);
    }
    Ok(())
}

pub(in crate::execution) fn prune_managed_process_routes_without_aliases<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
    kernel_pid: u32,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let candidates = vm
        .managed_host_net_descriptions
        .lock()
        .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?
        .iter()
        .filter_map(|(description_id, description)| {
            description
                .routes
                .contains_key(&kernel_pid)
                .then_some(*description_id)
        })
        .collect::<Vec<_>>();
    let socket_paths = build_socket_path_context(vm)?;
    prune_managed_descriptions_after_fd_mutation(
        bridge,
        vm_id,
        &socket_paths,
        vm,
        kernel_pid,
        candidates,
    )
}

pub(in crate::execution) fn retire_orphaned_managed_descriptions(
    vm: &mut VmState,
) -> Result<(), SidecarError> {
    vm.managed_host_net_descriptions
        .lock()
        .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?
        .retain(|_, description| {
            !(description.routes.is_empty() && description.lease.ref_count() == 1)
        });
    Ok(())
}

#[derive(serde::Deserialize)]
struct ManagedConnectWire {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    path: Option<String>,
    #[serde(rename = "abstractPathHex", default)]
    abstract_path_hex: Option<String>,
    #[serde(rename = "boundServerId", default)]
    bound_server_id: Option<String>,
    #[serde(rename = "localAddress", default)]
    local_address: Option<String>,
    #[serde(rename = "localPort", default)]
    local_port: Option<u16>,
    #[serde(rename = "localReservation", default)]
    local_reservation: Option<String>,
}

#[derive(serde::Deserialize)]
struct ManagedBindConnectedUnixWire {
    #[serde(rename = "socketId")]
    socket_id: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(rename = "abstractPathHex", default)]
    abstract_path_hex: Option<String>,
    #[serde(default)]
    autobind: bool,
}

#[derive(serde::Deserialize)]
struct ManagedReserveTcpPortWire {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
}

#[derive(serde::Deserialize)]
struct ManagedListenWire {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    path: Option<String>,
    #[serde(rename = "abstractPathHex", default)]
    abstract_path_hex: Option<String>,
    #[serde(rename = "boundServerId", default)]
    bound_server_id: Option<String>,
    #[serde(default)]
    autobind: bool,
    #[serde(default)]
    backlog: Option<u32>,
    #[serde(rename = "localReservation", default)]
    local_reservation: Option<String>,
}

#[derive(serde::Deserialize)]
struct ManagedUdpCreateWire {
    #[serde(rename = "type")]
    socket_type: String,
}

#[derive(serde::Deserialize)]
struct ManagedUdpBindWire {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    port: u16,
}

#[derive(serde::Deserialize)]
struct ManagedUdpSendWire {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    port: Option<u16>,
}

pub(super) fn decode_managed(
    request: &HostRpcRequest,
    max_payload_bytes: usize,
) -> Result<Option<NetworkOperation>, SidecarError> {
    let id_limit = PayloadLimit::new("runtime.network.maxCapabilityIdBytes", MANAGED_ID_BYTES)
        .map_err(SidecarError::Host)?;
    let host_limit = PayloadLimit::new("runtime.network.maxHostBytes", HOST_BYTES)
        .map_err(SidecarError::Host)?;
    let path_limit = PayloadLimit::new("runtime.network.maxUnixPathBytes", UNIX_PATH_BYTES)
        .map_err(SidecarError::Host)?;
    let payload_limit =
        PayloadLimit::new("limits.reactor.maxBridgeRequestBytes", max_payload_bytes)
            .map_err(SidecarError::Host)?;
    let id = |index, label| bounded_arg(request, index, label, &id_limit);
    let host = |value: Option<String>| bounded_optional(value, &host_limit);
    let unix = |path, abstract_hex, autobind| {
        decode_unix_address(path, abstract_hex, autobind, &path_limit)
    };
    let operation = match request.method.as_str() {
        "net.bind_unix" => {
            let payload: ManagedListenWire = decode_value_arg(request, 0, "net.bind_unix")?;
            NetworkOperation::ManagedBindUnix {
                address: unix(payload.path, payload.abstract_path_hex, payload.autobind)?,
            }
        }
        "net.bind_connected_unix" => {
            let payload: ManagedBindConnectedUnixWire =
                decode_value_arg(request, 0, "net.bind_connected_unix")?;
            NetworkOperation::ManagedBindConnectedUnix {
                socket_id: BoundedString::try_new(payload.socket_id, &id_limit)
                    .map_err(SidecarError::Host)?,
                address: unix(payload.path, payload.abstract_path_hex, payload.autobind)?,
            }
        }
        "net.reserve_tcp_port" => {
            let payload: ManagedReserveTcpPortWire =
                decode_value_arg(request, 0, "net.reserve_tcp_port")?;
            NetworkOperation::ManagedReserveTcpPort {
                host: host(payload.host)?,
                port: payload.port,
            }
        }
        "net.release_tcp_port" => NetworkOperation::ManagedReleaseTcpPort {
            reservation_id: id(0, "reservation id")?,
        },
        "net.connect" => {
            let payload: ManagedConnectWire = decode_value_arg(request, 0, "net.connect")?;
            NetworkOperation::ManagedConnect {
                endpoint: ManagedTcpEndpoint {
                    host: host(payload.host)?,
                    port: payload.port,
                    unix: decode_optional_unix(
                        payload.path,
                        payload.abstract_path_hex,
                        false,
                        &path_limit,
                    )?,
                    bound_server_id: bounded_optional(payload.bound_server_id, &id_limit)?,
                    local_address: host(payload.local_address)?,
                    local_port: payload.local_port,
                    local_reservation: bounded_optional(payload.local_reservation, &id_limit)?,
                    backlog: None,
                },
            }
        }
        "net.listen" => {
            let payload: ManagedListenWire = decode_value_or_json_arg(request, 0, "net.listen")?;
            NetworkOperation::ManagedListen {
                endpoint: ManagedTcpEndpoint {
                    host: host(payload.host)?,
                    port: payload.port,
                    unix: decode_optional_unix(
                        payload.path,
                        payload.abstract_path_hex,
                        payload.autobind,
                        &path_limit,
                    )?,
                    bound_server_id: bounded_optional(payload.bound_server_id, &id_limit)?,
                    local_address: None,
                    local_port: None,
                    local_reservation: bounded_optional(payload.local_reservation, &id_limit)?,
                    backlog: payload.backlog,
                },
            }
        }
        "net.poll" => NetworkOperation::ManagedPoll {
            socket_id: id(0, "socket id")?,
            wait_ms: optional_u64(request, 1, "wait ms")?,
        },
        "net.socket_wait_connect" => NetworkOperation::ManagedWaitConnect {
            socket_id: id(0, "socket id")?,
        },
        "net.socket_read" => NetworkOperation::ManagedRead {
            socket_id: id(0, "socket id")?,
            max_bytes: optional_u64(request, 1, "maximum read bytes")?,
            peek: optional_bool(request, 2, "peek flag")?,
            wait_ms: optional_u64(request, 3, "wait ms")?,
        },
        "net.write" => NetworkOperation::ManagedWrite {
            socket_id: id(0, "socket id")?,
            bytes: bounded_bytes_arg(request, 1, "net.write bytes", &payload_limit)?,
        },
        "net.destroy" => NetworkOperation::ManagedDestroy {
            socket_id: id(0, "socket id")?,
        },
        "net.server_accept" => NetworkOperation::ManagedAccept {
            listener_id: id(0, "listener id")?,
        },
        "net.server_close" => NetworkOperation::ManagedCloseListener {
            listener_id: id(0, "listener id")?,
        },
        "net.socket_upgrade_tls" => {
            let options = javascript_sync_rpc_arg_str(&request.args, 1, "TLS options")?;
            serde_json::from_str::<TlsBridgeOptions>(options).map_err(|error| {
                SidecarError::host("EINVAL", format!("invalid TLS options: {error}"))
            })?;
            NetworkOperation::ManagedTlsUpgrade {
                socket_id: id(0, "socket id")?,
                options_json: BoundedString::try_new(options.to_owned(), &payload_limit)
                    .map_err(SidecarError::Host)?,
            }
        }
        "dgram.createSocket" => {
            let payload: ManagedUdpCreateWire = decode_value_arg(request, 0, "dgram.createSocket")?;
            NetworkOperation::ManagedUdpCreate {
                family: match payload.socket_type.as_str() {
                    "udp4" => ManagedUdpFamily::Inet4,
                    "udp6" => ManagedUdpFamily::Inet6,
                    other => {
                        return Err(SidecarError::host(
                            "EINVAL",
                            format!("unsupported UDP type {other}"),
                        ))
                    }
                },
            }
        }
        "dgram.bind" => {
            let payload: ManagedUdpBindWire = decode_value_arg(request, 1, "dgram.bind")?;
            NetworkOperation::ManagedUdpBind {
                socket_id: id(0, "socket id")?,
                host: host(payload.address)?,
                port: payload.port,
            }
        }
        "dgram.send" => {
            let payload: ManagedUdpSendWire = decode_value_arg(request, 2, "dgram.send")?;
            NetworkOperation::ManagedUdpSend {
                socket_id: id(0, "socket id")?,
                bytes: bounded_bytes_arg(request, 1, "UDP bytes", &payload_limit)?,
                host: host(payload.address)?,
                port: payload.port,
            }
        }
        "dgram.poll" => NetworkOperation::ManagedUdpPoll {
            socket_id: id(0, "socket id")?,
            wait_ms: optional_u64(request, 1, "wait ms")?,
            peek: optional_bool(request, 2, "peek flag")?,
            max_bytes: None,
        },
        "dgram.close" => NetworkOperation::ManagedUdpClose {
            socket_id: id(0, "socket id")?,
        },
        _ => return Ok(None),
    };
    Ok(Some(operation))
}

fn bounded_arg(
    request: &HostRpcRequest,
    index: usize,
    label: &str,
    limit: &PayloadLimit,
) -> Result<BoundedString, SidecarError> {
    BoundedString::try_new(
        javascript_sync_rpc_arg_str(&request.args, index, label)?.to_owned(),
        limit,
    )
    .map_err(SidecarError::Host)
}
fn bounded_optional(
    value: Option<String>,
    limit: &PayloadLimit,
) -> Result<Option<BoundedString>, SidecarError> {
    value
        .map(|value| BoundedString::try_new(value, limit).map_err(SidecarError::Host))
        .transpose()
}
fn bounded_bytes_arg(
    request: &HostRpcRequest,
    index: usize,
    label: &str,
    limit: &PayloadLimit,
) -> Result<BoundedBytes, SidecarError> {
    BoundedBytes::try_new(
        javascript_sync_rpc_request_bytes_arg(request, index, label)?,
        limit,
    )
    .map_err(SidecarError::Host)
}
fn optional_u64(request: &HostRpcRequest, index: usize, label: &str) -> Result<u64, SidecarError> {
    Ok(javascript_sync_rpc_arg_u64_optional(&request.args, index, label)?.unwrap_or_default())
}
fn optional_bool(
    request: &HostRpcRequest,
    index: usize,
    label: &str,
) -> Result<bool, SidecarError> {
    match request.args.get(index) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(SidecarError::host(
            "EINVAL",
            format!("{label} must be a boolean"),
        )),
    }
}
fn decode_value_arg<T: serde::de::DeserializeOwned>(
    request: &HostRpcRequest,
    index: usize,
    label: &str,
) -> Result<T, SidecarError> {
    serde_json::from_value(
        request
            .args
            .get(index)
            .cloned()
            .ok_or_else(|| SidecarError::host("EINVAL", format!("{label} payload is required")))?,
    )
    .map_err(|error| SidecarError::host("EINVAL", format!("invalid {label} payload: {error}")))
}
fn decode_value_or_json_arg<T: serde::de::DeserializeOwned>(
    request: &HostRpcRequest,
    index: usize,
    label: &str,
) -> Result<T, SidecarError> {
    let value = request
        .args
        .get(index)
        .cloned()
        .ok_or_else(|| SidecarError::host("EINVAL", format!("{label} payload is required")))?;
    match value {
        Value::String(json) => serde_json::from_str(&json),
        other => serde_json::from_value(other),
    }
    .map_err(|error| SidecarError::host("EINVAL", format!("invalid {label} payload: {error}")))
}
fn decode_optional_unix(
    path: Option<String>,
    abstract_hex: Option<String>,
    autobind: bool,
    limit: &PayloadLimit,
) -> Result<Option<ManagedUnixAddress>, SidecarError> {
    if path.is_none() && abstract_hex.is_none() && !autobind {
        Ok(None)
    } else {
        decode_unix_address(path, abstract_hex, autobind, limit).map(Some)
    }
}
fn decode_unix_address(
    path: Option<String>,
    abstract_hex: Option<String>,
    autobind: bool,
    limit: &PayloadLimit,
) -> Result<ManagedUnixAddress, SidecarError> {
    match (path, abstract_hex, autobind) {
        (Some(path), None, false) => BoundedString::try_new(path, limit)
            .map(ManagedUnixAddress::Path)
            .map_err(SidecarError::Host),
        (None, Some(hex), false) => BoundedString::try_new(hex, limit)
            .map(ManagedUnixAddress::AbstractHex)
            .map_err(SidecarError::Host),
        (None, None, true) => Ok(ManagedUnixAddress::Autobind),
        _ => Err(SidecarError::host(
            "EINVAL",
            "exactly one Unix address is required",
        )),
    }
}

#[allow(dead_code)]
pub(super) struct NetworkCapability;

#[allow(clippy::too_many_arguments)]
fn dispatch_context_stream_read<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    socket_id: String,
    max_bytes: u64,
    peek: bool,
    wait_ms: u64,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let kernel_pid = reply.identity().pid;
    let process_path = sidecar
        .vms
        .get(vm_id)
        .and_then(|vm| vm.active_processes.get(process_id))
        .and_then(|root| NativeSidecar::<B>::active_process_path_by_kernel_pid(root, kernel_pid))
        .ok_or_else(|| {
            SidecarError::host(
                "ESTALE",
                format!("active process for kernel pid {kernel_pid} disappeared"),
            )
        })?;
    if !reply.claim().map_err(SidecarError::from)? {
        return Ok(());
    }
    let deadline = match checked_deferred_guest_wait_deadline(wait_ms) {
        Ok(deadline) => deadline,
        Err(error) => {
            reply.fail(error).map_err(SidecarError::from)?;
            return Ok(());
        }
    };
    dispatch_claimed_context_stream_read(
        sidecar,
        vm_id,
        process_id,
        ManagedStreamReadRecheck {
            root_process_id: process_id.to_owned(),
            process_path,
            socket_id,
            max_bytes,
            peek,
            deadline,
            reply,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::execution) fn dispatch_descendant_context_stream_read<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    root_process_id: &str,
    process_path: &[&str],
    socket_id: String,
    max_bytes: u64,
    peek: bool,
    wait_ms: u64,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !reply.claim().map_err(SidecarError::from)? {
        return Ok(());
    }
    let deadline = match checked_deferred_guest_wait_deadline(wait_ms) {
        Ok(deadline) => deadline,
        Err(error) => {
            reply.fail(error).map_err(SidecarError::from)?;
            return Ok(());
        }
    };
    dispatch_claimed_context_stream_read(
        sidecar,
        vm_id,
        root_process_id,
        ManagedStreamReadRecheck {
            root_process_id: root_process_id.to_owned(),
            process_path: process_path
                .iter()
                .map(|segment| (*segment).to_owned())
                .collect(),
            socket_id,
            max_bytes,
            peek,
            deadline,
            reply,
        },
    )
}

pub(in crate::execution) fn dispatch_claimed_context_stream_read<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    root_process_id: &str,
    pending: ManagedStreamReadRecheck,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if pending.root_process_id != root_process_id {
        pending
            .reply
            .fail(HostServiceError::new(
                "ESTALE",
                "managed stream read re-entry used the wrong root process lane",
            ))
            .map_err(SidecarError::from)?;
        return Ok(());
    }
    let socket_paths = build_socket_path_context(
        sidecar
            .vms
            .get(vm_id)
            .ok_or_else(|| missing_vm_error(vm_id))?,
    )?;
    let (runtime, connection_id, session_id, notify, response) = {
        let vm = sidecar
            .vms
            .get_mut(vm_id)
            .ok_or_else(|| missing_vm_error(vm_id))?;
        let root = vm
            .active_processes
            .get_mut(root_process_id)
            .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
        let process =
            NativeSidecar::<B>::active_process_by_owned_path_mut(root, &pending.process_path)
                .ok_or_else(|| {
                    SidecarError::host("ESTALE", "managed stream read target process disappeared")
                })?;
        let identity = pending.reply.identity();
        if identity.generation != vm.generation || identity.pid != process.kernel_pid {
            pending
                .reply
                .fail(HostServiceError::new(
                    "ESTALE",
                    "managed stream read identity no longer matches its process",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        let notify = if let Some(socket) = process.tcp_sockets.get(&pending.socket_id) {
            Arc::clone(&socket.read_event_notify)
        } else if let Some(socket) = process.unix_sockets.get(&pending.socket_id) {
            Arc::clone(&socket.read_event_notify)
        } else {
            pending
                .reply
                .fail(HostServiceError::new(
                    "EBADF",
                    format!("unknown managed socket {}", pending.socket_id),
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        };
        let runtime = vm.runtime_context.clone();
        let connection_id = vm.connection_id.clone();
        let session_id = vm.session_id.clone();
        let capabilities = vm.capabilities.clone();
        let response = service_managed_network_operation(
            ManagedNetworkServiceContext {
                vm_id,
                socket_paths: &socket_paths,
                kernel: &mut vm.kernel,
                kernel_readiness: Arc::clone(&vm.kernel_socket_readiness),
                process,
                capabilities,
            },
            NetworkOperation::ManagedRead {
                socket_id: bounded_managed_id(pending.socket_id.clone())?,
                max_bytes: pending.max_bytes,
                peek: pending.peek,
                wait_ms: 0,
            },
        );
        (runtime, connection_id, session_id, notify, response)
    };
    let would_block = matches!(
        response.as_ref().ok().and_then(response_value),
        Some(Value::Object(fields)) if fields.get("kind").and_then(Value::as_str) == Some("wouldBlock")
    );
    if !would_block || Instant::now() >= pending.deadline {
        return settle_execution_host_call(&pending.reply, response);
    }

    let sender = sidecar.process_event_sender.clone();
    let event_notify = Arc::clone(&sidecar.process_event_notify);
    let vm_id_owned = vm_id.to_owned();
    let root_process_id_owned = root_process_id.to_owned();
    let task_reply = pending.reply.clone();
    let spawn = runtime.spawn(agentos_runtime::TaskClass::Socket, async move {
        let remaining = pending.deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            tokio::select! {
                _ = notify.notified() => {}
                _ = tokio::time::sleep(remaining) => {}
            }
        }
        let envelope = ProcessEventEnvelope {
            connection_id,
            session_id,
            vm_id: vm_id_owned,
            process_id: root_process_id_owned,
            event: ActiveExecutionEvent::ManagedStreamReadRecheck(Box::new(pending)),
        };
        if let Err(error) = sender.send(envelope).await {
            if let ActiveExecutionEvent::ManagedStreamReadRecheck(pending) = error.0.event {
                if let Err(error) = pending.reply.fail(HostServiceError::new(
                    "ECANCELED",
                    "managed stream read re-entry lane closed",
                )) {
                    eprintln!(
                        "ERR_AGENTOS_HOST_REPLY_SETTLEMENT: failed to cancel managed stream read: {error}"
                    );
                }
            }
        } else {
            event_notify.notify_one();
        }
    });
    if let Err(error) = spawn {
        task_reply
            .fail(host_service_error(&SidecarError::from(error)))
            .map_err(SidecarError::from)?;
    }
    Ok(())
}

pub(super) async fn dispatch_context_managed_network_operation<B>(
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
    let kernel_pid = reply.identity().pid;
    if let NetworkOperation::ManagedRead {
        socket_id,
        max_bytes,
        peek,
        wait_ms,
    } = &operation
    {
        return dispatch_context_stream_read(
            sidecar,
            vm_id,
            process_id,
            socket_id.as_str().to_owned(),
            *max_bytes,
            *peek,
            *wait_ms,
            reply,
        );
    }
    if let NetworkOperation::Receive {
        fd,
        max_bytes,
        flags,
        deadline_ms,
        ..
    } = &operation
    {
        let vm = sidecar
            .vms
            .get(vm_id)
            .expect("validated fd-network VM remains registered");
        let description_id = vm
            .kernel
            .fd_description_identity(EXECUTION_DRIVER_NAME, kernel_pid, *fd)
            .map_err(kernel_error)?
            .0;
        let route = vm
            .managed_host_net_descriptions
            .lock()
            .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?
            .get(&description_id)
            .and_then(|description| description.route_for(kernel_pid).cloned());
        if let Some(ManagedHostNetRoute::UdpSocket(socket_id)) = route {
            return dispatch_context_udp_poll(
                sidecar,
                vm_id,
                process_id,
                NetworkOperation::ManagedUdpPoll {
                    socket_id: bounded_managed_id(socket_id)?,
                    wait_ms: deadline_ms.unwrap_or_default(),
                    peek: flags & 0x0002 != 0,
                    max_bytes: Some(*max_bytes),
                },
                reply,
            );
        }
        if let Some(
            ManagedHostNetRoute::TcpSocket(socket_id) | ManagedHostNetRoute::UnixSocket(socket_id),
        ) = route
        {
            return dispatch_context_stream_read(
                sidecar,
                vm_id,
                process_id,
                socket_id,
                max_bytes.get() as u64,
                flags & 0x0002 != 0,
                deadline_ms.unwrap_or_default(),
                reply,
            );
        }
    }
    if !reply.claim().map_err(SidecarError::from)? {
        return Ok(());
    }
    if is_managed_fd_operation(&operation) {
        let bridge = sidecar.bridge.clone();
        let socket_paths = build_socket_path_context(
            sidecar
                .vms
                .get(vm_id)
                .expect("validated fd-network VM remains registered"),
        )?;
        let vm = sidecar
            .vms
            .get_mut(vm_id)
            .expect("validated fd-network VM remains registered");
        let runtime = vm.runtime_context.clone();
        let capabilities = vm.capabilities.clone();
        let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
        let dns = vm.dns.clone();
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let process = active_process_by_kernel_pid_mut(&mut vm.active_processes, kernel_pid)
            .ok_or_else(|| {
                SidecarError::host(
                    "ESTALE",
                    format!("active process for kernel pid {kernel_pid} disappeared"),
                )
            })?;
        let response = service_managed_fd_network_operation(
            ManagedFdNetworkServiceContext {
                bridge: &bridge,
                vm_id,
                dns: &dns,
                socket_paths: &socket_paths,
                kernel: &mut vm.kernel,
                kernel_readiness,
                process,
                capabilities,
                managed_descriptions,
                call_id: reply.identity().call_id,
            },
            operation,
        );
        return settle_managed_network_response(
            sidecar,
            vm_id,
            process_id,
            runtime,
            reply,
            "managed fd network",
            response,
        );
    }
    if matches!(
        operation,
        NetworkOperation::ManagedUdpCreate { .. }
            | NetworkOperation::ManagedUdpBind { .. }
            | NetworkOperation::ManagedUdpSend { .. }
            | NetworkOperation::ManagedUdpClose { .. }
    ) {
        let bridge = sidecar.bridge.clone();
        let socket_paths = build_socket_path_context(
            sidecar
                .vms
                .get(vm_id)
                .expect("validated managed UDP VM remains registered"),
        )?;
        let vm = sidecar
            .vms
            .get_mut(vm_id)
            .expect("validated managed UDP VM remains registered");
        let runtime = vm.runtime_context.clone();
        let capabilities = vm.capabilities.clone();
        let dns = vm.dns.clone();
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let process = active_process_by_kernel_pid_mut(&mut vm.active_processes, kernel_pid)
            .ok_or_else(|| {
                SidecarError::host(
                    "ESTALE",
                    format!("active process for kernel pid {kernel_pid} disappeared"),
                )
            })?;
        let response = service_managed_udp_operation(
            ManagedUdpServiceRequest {
                bridge: &bridge,
                kernel: &mut vm.kernel,
                vm_id,
                dns: &dns,
                socket_paths: &socket_paths,
                process,
                kernel_readiness,
                capabilities,
            },
            operation,
        );
        return settle_managed_network_response(
            sidecar,
            vm_id,
            process_id,
            runtime,
            reply,
            "managed UDP",
            response,
        );
    }
    if matches!(
        operation,
        NetworkOperation::ManagedPoll { .. }
            | NetworkOperation::ManagedWaitConnect { .. }
            | NetworkOperation::ManagedRead { .. }
            | NetworkOperation::ManagedWrite { .. }
            | NetworkOperation::ManagedDestroy { .. }
            | NetworkOperation::ManagedAccept { .. }
            | NetworkOperation::ManagedCloseListener { .. }
            | NetworkOperation::ManagedTlsUpgrade { .. }
    ) {
        let socket_paths = build_socket_path_context(
            sidecar
                .vms
                .get(vm_id)
                .expect("validated managed-network VM remains registered"),
        )?;
        let vm = sidecar
            .vms
            .get_mut(vm_id)
            .expect("validated managed-network VM remains registered");
        let runtime = vm.runtime_context.clone();
        let capabilities = vm.capabilities.clone();
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let process = active_process_by_kernel_pid_mut(&mut vm.active_processes, kernel_pid)
            .ok_or_else(|| {
                SidecarError::host(
                    "ESTALE",
                    format!("active process for kernel pid {kernel_pid} disappeared"),
                )
            })?;
        let response = service_managed_network_operation(
            ManagedNetworkServiceContext {
                vm_id,
                socket_paths: &socket_paths,
                kernel: &mut vm.kernel,
                kernel_readiness,
                process,
                capabilities,
            },
            operation,
        );
        return settle_managed_network_response(
            sidecar,
            vm_id,
            process_id,
            runtime,
            reply,
            "managed network",
            response,
        );
    }
    if is_managed_endpoint_operation(&operation) {
        let bridge = sidecar.bridge.clone();
        let socket_paths = build_socket_path_context(
            sidecar
                .vms
                .get(vm_id)
                .expect("validated managed-endpoint VM remains registered"),
        )?;
        let vm = sidecar
            .vms
            .get_mut(vm_id)
            .expect("validated managed-endpoint VM remains registered");
        let runtime = vm.runtime_context.clone();
        let capabilities = vm.capabilities.clone();
        let dns = vm.dns.clone();
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let process = active_process_by_kernel_pid_mut(&mut vm.active_processes, kernel_pid)
            .ok_or_else(|| {
                SidecarError::host(
                    "ESTALE",
                    format!("active process for kernel pid {kernel_pid} disappeared"),
                )
            })?;
        let response = service_managed_endpoint_operation(
            ManagedEndpointServiceContext {
                bridge: &bridge,
                vm_id,
                dns: &dns,
                socket_paths: &socket_paths,
                kernel: &mut vm.kernel,
                kernel_readiness,
                process,
                capabilities,
                call_id: reply.identity().call_id,
            },
            operation,
        );
        return settle_managed_network_response(
            sidecar,
            vm_id,
            process_id,
            runtime,
            reply,
            "managed endpoint",
            response,
        );
    }
    if is_direct_managed_operation(&operation) {
        return Err(SidecarError::host(
            "EINVAL",
            "typed managed-network operation missed its direct executor",
        ));
    }
    let bridge = sidecar.bridge.clone();
    let socket_paths = build_socket_path_context(
        sidecar
            .vms
            .get(vm_id)
            .expect("validated managed-network VM remains registered"),
    )?;
    let vm = sidecar
        .vms
        .get_mut(vm_id)
        .expect("validated managed-network VM remains registered");
    let runtime = vm.runtime_context.clone();
    let capabilities = vm.capabilities.clone();
    let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
    let dns = vm.dns.clone();
    let process = active_process_by_kernel_pid_mut(&mut vm.active_processes, kernel_pid)
        .ok_or_else(|| {
            SidecarError::host(
                "ESTALE",
                format!("active process for kernel pid {kernel_pid} disappeared"),
            )
        })?;
    let response = service_descriptor_rights_compat_operation(
        &bridge,
        vm_id,
        &dns,
        &socket_paths,
        &mut vm.kernel,
        kernel_readiness,
        process,
        capabilities,
        Arc::clone(&vm.managed_host_net_descriptions),
        reply.identity().call_id,
        operation,
    )
    .await;

    settle_managed_network_response(
        sidecar,
        vm_id,
        process_id,
        runtime,
        reply,
        "descriptor rights",
        response,
    )
}

struct ManagedFdNetworkServiceContext<'a, B> {
    bridge: &'a SharedBridge<B>,
    vm_id: &'a str,
    dns: &'a VmDnsConfig,
    socket_paths: &'a SocketPathContext,
    kernel: &'a mut SidecarKernel,
    kernel_readiness: KernelSocketReadinessRegistry,
    process: &'a mut ActiveProcess,
    capabilities: CapabilityRegistry,
    managed_descriptions: crate::state::ManagedHostNetDescriptionRegistry,
    call_id: u64,
}

#[allow(clippy::too_many_arguments)]
pub(in crate::execution) fn service_descendant_managed_fd_network_operation<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    socket_paths: &SocketPathContext,
    kernel: &mut SidecarKernel,
    kernel_readiness: KernelSocketReadinessRegistry,
    process: &mut ActiveProcess,
    capabilities: CapabilityRegistry,
    managed_descriptions: crate::state::ManagedHostNetDescriptionRegistry,
    call_id: u64,
    operation: NetworkOperation,
) -> Result<HostServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    service_managed_fd_network_operation(
        ManagedFdNetworkServiceContext {
            bridge,
            vm_id,
            dns,
            socket_paths,
            kernel,
            kernel_readiness,
            process,
            capabilities,
            managed_descriptions,
            call_id,
        },
        operation,
    )
}

fn managed_fd_description_id<B>(
    context: &ManagedFdNetworkServiceContext<'_, B>,
    fd: u32,
) -> Result<u64, SidecarError> {
    let (description_id, _) = context
        .kernel
        .fd_description_identity(EXECUTION_DRIVER_NAME, context.process.kernel_pid, fd)
        .map_err(kernel_error)?;
    if !context
        .managed_descriptions
        .lock()
        .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?
        .contains_key(&description_id)
    {
        return Err(SidecarError::host(
            "ENOTSOCK",
            format!("fd {fd} is not a managed socket"),
        ));
    }
    Ok(description_id)
}

fn managed_description<B>(
    context: &ManagedFdNetworkServiceContext<'_, B>,
    description_id: u64,
) -> Result<ManagedHostNetDescription, SidecarError> {
    context
        .managed_descriptions
        .lock()
        .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?
        .get(&description_id)
        .cloned()
        .ok_or_else(|| SidecarError::host("ENOTSOCK", "managed socket description disappeared"))
}

fn update_managed_description<B>(
    context: &ManagedFdNetworkServiceContext<'_, B>,
    description_id: u64,
    update: impl FnOnce(&mut ManagedHostNetDescription),
) -> Result<(), SidecarError> {
    let mut descriptions = context
        .managed_descriptions
        .lock()
        .map_err(|_| SidecarError::host("EIO", "managed description registry lock poisoned"))?;
    let description = descriptions
        .get_mut(&description_id)
        .ok_or_else(|| SidecarError::host("ENOTSOCK", "managed socket description disappeared"))?;
    update(description);
    Ok(())
}

fn managed_route<B>(
    context: &ManagedFdNetworkServiceContext<'_, B>,
    description_id: u64,
) -> Result<ManagedHostNetRoute, SidecarError> {
    managed_description(context, description_id)?
        .route_for(context.process.kernel_pid)
        .cloned()
        .ok_or_else(|| {
            SidecarError::host(
                "ESTALE",
                "managed socket has no reactor projection for this process",
            )
        })
}

fn managed_endpoint_context<'a, B>(
    context: &'a mut ManagedFdNetworkServiceContext<'_, B>,
) -> ManagedEndpointServiceContext<'a, B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    ManagedEndpointServiceContext {
        bridge: context.bridge,
        vm_id: context.vm_id,
        dns: context.dns,
        socket_paths: context.socket_paths,
        kernel: &mut *context.kernel,
        kernel_readiness: context.kernel_readiness.clone(),
        process: &mut *context.process,
        capabilities: context.capabilities.clone(),
        call_id: context.call_id,
    }
}

fn managed_socket_context<'a, B>(
    context: &'a mut ManagedFdNetworkServiceContext<'_, B>,
) -> ManagedNetworkServiceContext<'a> {
    ManagedNetworkServiceContext {
        vm_id: context.vm_id,
        socket_paths: context.socket_paths,
        kernel: &mut *context.kernel,
        kernel_readiness: context.kernel_readiness.clone(),
        process: &mut *context.process,
        capabilities: context.capabilities.clone(),
    }
}

fn endpoint_for_address(address: &SocketAddress) -> ManagedTcpEndpoint {
    match address {
        SocketAddress::Inet { host, port } => ManagedTcpEndpoint {
            host: Some(host.clone()),
            port: Some(*port),
            unix: None,
            bound_server_id: None,
            local_address: None,
            local_port: None,
            local_reservation: None,
            backlog: None,
        },
        SocketAddress::UnixPath(path) => ManagedTcpEndpoint {
            host: None,
            port: None,
            unix: Some(ManagedUnixAddress::Path(path.clone())),
            bound_server_id: None,
            local_address: None,
            local_port: None,
            local_reservation: None,
            backlog: None,
        },
        SocketAddress::UnixAbstract(bytes) => ManagedTcpEndpoint {
            host: None,
            port: None,
            unix: Some(ManagedUnixAddress::AbstractHex(
                BoundedString::try_new(
                    encode_hex_bytes(bytes.as_slice()),
                    &PayloadLimit::new("runtime.filesystem.maxPathBytes", 8192)
                        .expect("static nonzero path limit"),
                )
                .expect("bounded abstract address remains bounded after hex encoding"),
            )),
            bound_server_id: None,
            local_address: None,
            local_port: None,
            local_reservation: None,
            backlog: None,
        },
        SocketAddress::UnixAutobind => ManagedTcpEndpoint {
            host: None,
            port: None,
            unix: Some(ManagedUnixAddress::Autobind),
            bound_server_id: None,
            local_address: None,
            local_port: None,
            local_reservation: None,
            backlog: None,
        },
    }
}

fn managed_unix_address(address: &SocketAddress) -> Result<ManagedUnixAddress, SidecarError> {
    match endpoint_for_address(address).unix {
        Some(address) => Ok(address),
        None => Err(SidecarError::host(
            "EAFNOSUPPORT",
            "expected an AF_UNIX address",
        )),
    }
}

fn response_value(response: &HostServiceResponse) -> Option<Value> {
    let HostServiceResponse::Json(value) = response else {
        return None;
    };
    match value {
        Value::String(encoded) => serde_json::from_str(encoded).ok(),
        value => Some(value.clone()),
    }
}

fn response_string_field(response: &HostServiceResponse, field: &str) -> Option<String> {
    response_value(response)?
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(in crate::execution) fn managed_socket_address_from_info(
    info: &Value,
    peer: bool,
) -> Result<Option<SocketAddress>, SidecarError> {
    let (inet_address, inet_port, unix_path, unix_abstract) = if peer {
        (
            "remoteAddress",
            "remotePort",
            "remotePath",
            "remoteAbstractPathHex",
        )
    } else {
        (
            "localAddress",
            "localPort",
            "localPath",
            "localAbstractPathHex",
        )
    };
    if let (Some(host), Some(port)) = (
        info.get(inet_address).and_then(Value::as_str),
        info.get(inet_port).and_then(Value::as_u64),
    ) {
        let port = u16::try_from(port)
            .map_err(|_| SidecarError::host("EINVAL", "socket endpoint port exceeds u16"))?;
        let host = BoundedString::try_new(
            host.to_owned(),
            &PayloadLimit::new("runtime.network.maxHostBytes", HOST_BYTES)
                .expect("static nonzero host limit"),
        )
        .map_err(SidecarError::from)?;
        return Ok(Some(SocketAddress::Inet { host, port }));
    }
    if let Some(hex) = info.get(unix_abstract).and_then(Value::as_str) {
        let bytes = decode_abstract_unix_name(hex)?;
        return BoundedBytes::try_new(
            bytes,
            &PayloadLimit::new("runtime.network.maxUnixAddressBytes", UNIX_PATH_BYTES)
                .expect("static nonzero Unix address limit"),
        )
        .map(SocketAddress::UnixAbstract)
        .map(Some)
        .map_err(SidecarError::from);
    }
    if let Some(path) = info.get(unix_path).and_then(Value::as_str) {
        let path = BoundedString::try_new(
            path.to_owned(),
            &PayloadLimit::new("runtime.network.maxUnixPathBytes", UNIX_PATH_BYTES)
                .expect("static nonzero Unix path limit"),
        )
        .map_err(SidecarError::from)?;
        return Ok(Some(SocketAddress::UnixPath(path)));
    }
    Ok(None)
}

fn response_socket_address(
    response: &HostServiceResponse,
    peer: bool,
) -> Result<Option<SocketAddress>, SidecarError> {
    response_value(response)
        .as_ref()
        .map(|value| managed_socket_address_from_info(value, peer))
        .transpose()
        .map(Option::flatten)
}

fn rollback_managed_stream_socket<B>(
    context: &mut ManagedFdNetworkServiceContext<'_, B>,
    socket_id: &str,
) {
    if let Some(socket) = context.process.tcp_sockets.remove(socket_id) {
        release_tcp_socket_handle(
            context.process,
            socket_id,
            socket,
            context.kernel,
            &context.kernel_readiness,
        );
    } else if let Some(socket) = context.process.unix_sockets.remove(socket_id) {
        release_unix_socket_handle(
            context.process,
            socket_id,
            socket,
            &context.socket_paths.unix_bound_addresses,
        );
    }
}

fn service_managed_fd_network_operation<B>(
    mut context: ManagedFdNetworkServiceContext<'_, B>,
    operation: NetworkOperation,
) -> Result<HostServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    match operation {
        NetworkOperation::Validate { fd, requirement } => {
            if requirement == SocketValidationRequirement::Socket {
                context
                    .kernel
                    .fd_validate_socket(
                        EXECUTION_DRIVER_NAME,
                        context.process.kernel_pid,
                        fd,
                        false,
                    )
                    .map_err(kernel_error)?;
                return Ok(Value::Null.into());
            }

            let description_id = context
                .kernel
                .fd_description_identity(EXECUTION_DRIVER_NAME, context.process.kernel_pid, fd)
                .map_err(kernel_error)?
                .0;
            let managed_route = context
                .managed_descriptions
                .lock()
                .map_err(|_| {
                    SidecarError::host("EIO", "managed description registry lock poisoned")
                })?
                .get(&description_id)
                .and_then(|description| description.route_for(context.process.kernel_pid))
                .cloned();
            match managed_route {
                Some(
                    ManagedHostNetRoute::TcpListener(_) | ManagedHostNetRoute::UnixListener(_),
                ) => Ok(Value::Null.into()),
                Some(_) => Err(SidecarError::host(
                    "EINVAL",
                    format!("socket file descriptor {fd} is not listening"),
                )),
                None => {
                    context
                        .kernel
                        .fd_validate_socket(
                            EXECUTION_DRIVER_NAME,
                            context.process.kernel_pid,
                            fd,
                            true,
                        )
                        .map_err(kernel_error)?;
                    Ok(Value::Null.into())
                }
            }
        }
        NetworkOperation::Socket {
            domain,
            kind,
            nonblocking,
            close_on_exec,
        } => {
            let registry = Arc::clone(&context.managed_descriptions);
            let mut descriptions = registry.lock().map_err(|_| {
                SidecarError::host("EIO", "managed description registry lock poisoned")
            })?;
            let (fd, description_id) = context
                .kernel
                .fd_open_external_socket(
                    EXECUTION_DRIVER_NAME,
                    context.process.kernel_pid,
                    kind == SocketKind::Datagram,
                    nonblocking,
                    close_on_exec,
                )
                .map_err(kernel_error)?;
            let lease = match context.kernel.fd_transfer(
                EXECUTION_DRIVER_NAME,
                context.process.kernel_pid,
                fd,
            ) {
                Ok(lease) => lease,
                Err(error) => {
                    if let Err(close_error) = context.kernel.fd_close(
                        EXECUTION_DRIVER_NAME,
                        context.process.kernel_pid,
                        fd,
                    ) {
                        eprintln!(
                            "ERR_AGENTOS_SOCKET_CLEANUP: failed to roll back managed socket fd after transfer failure: {close_error}"
                        );
                    }
                    return Err(kernel_error(error));
                }
            };
            if descriptions.contains_key(&description_id) {
                if let Err(close_error) =
                    context
                        .kernel
                        .fd_close(EXECUTION_DRIVER_NAME, context.process.kernel_pid, fd)
                {
                    eprintln!(
                        "ERR_AGENTOS_SOCKET_CLEANUP: failed to close duplicate managed socket description after registry collision: {close_error}"
                    );
                }
                return Err(SidecarError::host(
                    "EEXIST",
                    "kernel reused a live managed socket description id",
                ));
            }
            descriptions.insert(
                description_id,
                ManagedHostNetDescription::new(domain, kind, lease, context.process.kernel_pid),
            );
            if kind == SocketKind::Datagram {
                let family = if domain == HostSocketDomain::Inet6 {
                    ManagedUdpFamily::Inet6
                } else {
                    ManagedUdpFamily::Inet4
                };
                let existing_udp_ids = context
                    .process
                    .udp_sockets
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>();
                let created = match service_managed_udp_operation(
                    ManagedUdpServiceRequest {
                        bridge: context.bridge,
                        kernel: &mut *context.kernel,
                        vm_id: context.vm_id,
                        dns: context.dns,
                        socket_paths: context.socket_paths,
                        process: &mut *context.process,
                        kernel_readiness: context.kernel_readiness.clone(),
                        capabilities: context.capabilities.clone(),
                    },
                    NetworkOperation::ManagedUdpCreate { family },
                ) {
                    Ok(created) => created,
                    Err(error) => {
                        descriptions.remove(&description_id);
                        if let Err(close_error) = context.kernel.fd_close(
                            EXECUTION_DRIVER_NAME,
                            context.process.kernel_pid,
                            fd,
                        ) {
                            eprintln!(
                                "ERR_AGENTOS_SOCKET_CLEANUP: failed to roll back managed UDP fd after create failure: {close_error}"
                            );
                        }
                        return Err(error);
                    }
                };
                let socket_id = response_string_field(&created, "socketId").filter(|socket_id| {
                    context.process.udp_sockets.contains_key(socket_id)
                        && !existing_udp_ids.contains(socket_id)
                });
                let Some(socket_id) = socket_id else {
                    let new_ids = context
                        .process
                        .udp_sockets
                        .keys()
                        .filter(|socket_id| !existing_udp_ids.contains(*socket_id))
                        .cloned()
                        .collect::<Vec<_>>();
                    for socket_id in new_ids {
                        if let Some(socket) = context.process.udp_sockets.remove(&socket_id) {
                            if let Err(error) = release_udp_socket_handle(
                                context.process,
                                &socket_id,
                                socket,
                                context.kernel,
                                &context.kernel_readiness,
                            ) {
                                eprintln!(
                                    "ERR_AGENTOS_SOCKET_CLEANUP: failed to roll back malformed UDP create result: {error}"
                                );
                            }
                        }
                    }
                    descriptions.remove(&description_id);
                    if let Err(error) = context.kernel.fd_close(
                        EXECUTION_DRIVER_NAME,
                        context.process.kernel_pid,
                        fd,
                    ) {
                        eprintln!(
                            "ERR_AGENTOS_SOCKET_CLEANUP: failed to roll back malformed managed UDP fd: {error}"
                        );
                    }
                    return Err(SidecarError::host(
                        "EIO",
                        "UDP create returned an invalid socket id",
                    ));
                };
                descriptions
                    .get_mut(&description_id)
                    .expect("new managed UDP description remains locked")
                    .routes
                    .insert(
                        context.process.kernel_pid,
                        ManagedHostNetRoute::UdpSocket(socket_id),
                    );
            }
            Ok(json!({
                "fd": fd,
                "descriptionId": description_id.to_string(),
            })
            .into())
        }
        NetworkOperation::Bind { fd, address } => {
            let description_id = context
                .kernel
                .fd_description_identity(EXECUTION_DRIVER_NAME, context.process.kernel_pid, fd)
                .map_err(kernel_error)?
                .0;
            let registry = Arc::clone(&context.managed_descriptions);
            let mut descriptions = registry.lock().map_err(|_| {
                SidecarError::host("EIO", "managed description registry lock poisoned")
            })?;
            let description = descriptions.get(&description_id).cloned().ok_or_else(|| {
                SidecarError::host("ENOTSOCK", "managed socket description disappeared")
            })?;
            let current_route = description
                .route_for(context.process.kernel_pid)
                .cloned()
                .ok_or_else(|| SidecarError::host("ESTALE", "managed socket route is missing"))?;
            if current_route != ManagedHostNetRoute::Unbound
                && !(description.kind == SocketKind::Datagram
                    && matches!(current_route, ManagedHostNetRoute::UdpSocket(_)))
            {
                return Err(SidecarError::host("EINVAL", "socket is already bound"));
            }
            let response = match (description.domain, description.kind) {
                (HostSocketDomain::Unix, SocketKind::Stream) => service_managed_endpoint_operation(
                    managed_endpoint_context(&mut context),
                    NetworkOperation::ManagedBindUnix {
                        address: managed_unix_address(&address)?,
                    },
                )?,
                (HostSocketDomain::Inet4 | HostSocketDomain::Inet6, SocketKind::Stream) => {
                    let SocketAddress::Inet { host, port } = &address else {
                        return Err(SidecarError::host(
                            "EAFNOSUPPORT",
                            "INET socket requires INET address",
                        ));
                    };
                    service_managed_endpoint_operation(
                        managed_endpoint_context(&mut context),
                        NetworkOperation::ManagedReserveTcpPort {
                            host: Some(host.clone()),
                            port: Some(*port),
                        },
                    )?
                }
                (HostSocketDomain::Inet4 | HostSocketDomain::Inet6, SocketKind::Datagram) => {
                    let ManagedHostNetRoute::UdpSocket(socket_id) = &current_route else {
                        return Err(SidecarError::host("EIO", "UDP socket route is missing"));
                    };
                    let socket_id = socket_id.clone();
                    let SocketAddress::Inet { host, port } = &address else {
                        return Err(SidecarError::host(
                            "EAFNOSUPPORT",
                            "UDP socket requires INET address",
                        ));
                    };
                    service_managed_udp_operation(
                        ManagedUdpServiceRequest {
                            bridge: context.bridge,
                            kernel: &mut *context.kernel,
                            vm_id: context.vm_id,
                            dns: context.dns,
                            socket_paths: context.socket_paths,
                            process: &mut *context.process,
                            kernel_readiness: context.kernel_readiness.clone(),
                            capabilities: context.capabilities.clone(),
                        },
                        NetworkOperation::ManagedUdpBind {
                            socket_id: bounded_managed_id(socket_id.clone())?,
                            host: Some(host.clone()),
                            port: *port,
                        },
                    )?
                }
                _ => return Err(SidecarError::host("EOPNOTSUPP", "unsupported socket bind")),
            };
            let route = match (description.domain, description.kind) {
                (HostSocketDomain::Unix, SocketKind::Stream) => ManagedHostNetRoute::UnixBound {
                    listener_id: response_string_field(&response, "serverId").ok_or_else(|| {
                        SidecarError::host("EIO", "Unix bind omitted listener id")
                    })?,
                },
                (HostSocketDomain::Inet4 | HostSocketDomain::Inet6, SocketKind::Stream) => {
                    ManagedHostNetRoute::TcpBound {
                        reservation_id: response_string_field(&response, "reservationId")
                            .ok_or_else(|| {
                                SidecarError::host("EIO", "TCP bind omitted reservation id")
                            })?,
                    }
                }
                (_, SocketKind::Datagram) => {
                    let ManagedHostNetRoute::UdpSocket(id) = current_route else {
                        unreachable!("UDP bind validated its route")
                    };
                    ManagedHostNetRoute::UdpSocket(id)
                }
                _ => unreachable!(),
            };
            let local_address =
                response_socket_address(&response, false)?.or_else(|| Some(address.clone()));
            let kernel_pid = context.process.kernel_pid;
            let description = descriptions
                .get_mut(&description_id)
                .expect("managed bind description remains locked");
            description.bound_address = Some(address);
            description.local_address = local_address;
            description.routes.insert(kernel_pid, route);
            Ok(response)
        }
        NetworkOperation::Connect { fd, address, .. } => {
            let description_id = managed_fd_description_id(&context, fd)?;
            let registry = Arc::clone(&context.managed_descriptions);
            let mut descriptions = registry.lock().map_err(|_| {
                SidecarError::host("EIO", "managed description registry lock poisoned")
            })?;
            let description = descriptions.get(&description_id).cloned().ok_or_else(|| {
                SidecarError::host("ENOTSOCK", "managed socket description disappeared")
            })?;
            if description.kind != SocketKind::Stream {
                return Err(SidecarError::host(
                    "EOPNOTSUPP",
                    "connect requires a stream socket",
                ));
            }
            let mut endpoint = endpoint_for_address(&address);
            match description
                .route_for(context.process.kernel_pid)
                .cloned()
                .ok_or_else(|| SidecarError::host("ESTALE", "managed socket route is missing"))?
            {
                ManagedHostNetRoute::Unbound => {}
                ManagedHostNetRoute::TcpBound { reservation_id } => {
                    endpoint.local_reservation = Some(bounded_managed_id(reservation_id)?);
                    if let Some(SocketAddress::Inet { host, port }) =
                        description.local_address.or(description.bound_address)
                    {
                        endpoint.local_address = Some(host);
                        endpoint.local_port = Some(port);
                    }
                }
                ManagedHostNetRoute::UnixBound { listener_id } => {
                    endpoint.bound_server_id = Some(bounded_managed_id(listener_id)?);
                }
                ManagedHostNetRoute::TcpListener(_) | ManagedHostNetRoute::UnixListener(_) => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        "listening socket cannot connect",
                    ))
                }
                _ => return Err(SidecarError::host("EISCONN", "socket is already connected")),
            }
            let response = service_managed_endpoint_operation(
                managed_endpoint_context(&mut context),
                NetworkOperation::ManagedConnect { endpoint },
            )?;
            if let Some(socket_id) = response_string_field(&response, "socketId") {
                let route = if description.domain == HostSocketDomain::Unix {
                    ManagedHostNetRoute::UnixSocket(socket_id)
                } else {
                    ManagedHostNetRoute::TcpSocket(socket_id)
                };
                let local_address = response_socket_address(&response, false)?;
                let peer_address =
                    response_socket_address(&response, true)?.or_else(|| Some(address.clone()));
                let description = descriptions
                    .get_mut(&description_id)
                    .expect("managed connect description remains locked");
                description.routes.insert(context.process.kernel_pid, route);
                description.local_address = local_address;
                description.peer_address = peer_address;
            } else if matches!(response, HostServiceResponse::Deferred { .. }) {
                context
                    .process
                    .pending_managed_host_net_connects
                    .insert(context.call_id, description_id);
            }
            Ok(response)
        }
        NetworkOperation::Listen { fd, backlog } => {
            let description_id = managed_fd_description_id(&context, fd)?;
            let registry = Arc::clone(&context.managed_descriptions);
            let mut descriptions = registry.lock().map_err(|_| {
                SidecarError::host("EIO", "managed description registry lock poisoned")
            })?;
            let description = descriptions.get(&description_id).cloned().ok_or_else(|| {
                SidecarError::host("ENOTSOCK", "managed socket description disappeared")
            })?;
            let mut endpoint = match description.bound_address.as_ref() {
                Some(address) => endpoint_for_address(address),
                None if description.domain != HostSocketDomain::Unix => ManagedTcpEndpoint {
                    host: None,
                    port: Some(0),
                    unix: None,
                    bound_server_id: None,
                    local_address: None,
                    local_port: None,
                    local_reservation: None,
                    backlog: None,
                },
                None => return Err(SidecarError::host("EINVAL", "Unix listener must be bound")),
            };
            endpoint.backlog = Some(backlog);
            match description
                .route_for(context.process.kernel_pid)
                .cloned()
                .ok_or_else(|| SidecarError::host("ESTALE", "managed socket route is missing"))?
            {
                ManagedHostNetRoute::TcpBound { reservation_id } => {
                    endpoint.local_reservation = Some(bounded_managed_id(reservation_id)?);
                }
                ManagedHostNetRoute::UnixBound { listener_id } => {
                    endpoint = ManagedTcpEndpoint {
                        host: None,
                        port: None,
                        unix: None,
                        bound_server_id: Some(bounded_managed_id(listener_id)?),
                        local_address: None,
                        local_port: None,
                        local_reservation: None,
                        backlog: Some(backlog),
                    };
                }
                ManagedHostNetRoute::UnixListener(listener_id) => {
                    return relisten_managed_unix_endpoint(
                        managed_endpoint_context(&mut context),
                        &listener_id,
                        backlog,
                    )
                }
                ManagedHostNetRoute::Unbound => {}
                _ => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        "socket cannot enter listen state",
                    ))
                }
            }
            let response = service_managed_endpoint_operation(
                managed_endpoint_context(&mut context),
                NetworkOperation::ManagedListen { endpoint },
            )?;
            let listener_id = response_string_field(&response, "serverId")
                .ok_or_else(|| SidecarError::host("EIO", "listen omitted listener id"))?;
            let route = if description.domain == HostSocketDomain::Unix {
                ManagedHostNetRoute::UnixListener(listener_id)
            } else {
                ManagedHostNetRoute::TcpListener(listener_id)
            };
            let local_address = response_socket_address(&response, false)?
                .or(description.local_address)
                .or(description.bound_address);
            let description = descriptions
                .get_mut(&description_id)
                .expect("managed listen description remains locked");
            description.routes.insert(context.process.kernel_pid, route);
            description.local_address = local_address;
            Ok(response)
        }
        NetworkOperation::Accept {
            fd,
            nonblocking,
            close_on_exec,
            ..
        } => {
            let registry = Arc::clone(&context.managed_descriptions);
            let mut descriptions = registry.lock().map_err(|_| {
                SidecarError::host("EIO", "managed description registry lock poisoned")
            })?;
            let description_id = context
                .kernel
                .fd_description_identity(EXECUTION_DRIVER_NAME, context.process.kernel_pid, fd)
                .map_err(kernel_error)?
                .0;
            let parent = descriptions.get(&description_id).cloned().ok_or_else(|| {
                SidecarError::host("ENOTSOCK", "managed listener description disappeared")
            })?;
            let parent_route = parent
                .route_for(context.process.kernel_pid)
                .cloned()
                .ok_or_else(|| SidecarError::host("ESTALE", "managed listener route is missing"))?;
            let listener_id = match &parent_route {
                ManagedHostNetRoute::TcpListener(id) | ManagedHostNetRoute::UnixListener(id) => {
                    id.clone()
                }
                _ => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        "accept requires a listening socket",
                    ))
                }
            };
            let response = service_managed_network_operation(
                managed_socket_context(&mut context),
                NetworkOperation::ManagedAccept {
                    listener_id: bounded_managed_id(listener_id)?,
                },
            )?;
            let Some(socket_id) = response_string_field(&response, "socketId") else {
                return Ok(response);
            };
            let mut value = match response_value(&response) {
                Some(Value::Object(fields)) => Value::Object(fields),
                _ => {
                    rollback_managed_stream_socket(&mut context, &socket_id);
                    return Err(SidecarError::host(
                        "EIO",
                        "accept returned an invalid response",
                    ));
                }
            };
            let address_info = value.get("info").unwrap_or(&value);
            let local_address = match managed_socket_address_from_info(address_info, false) {
                Ok(address) => address,
                Err(error) => {
                    rollback_managed_stream_socket(&mut context, &socket_id);
                    return Err(error);
                }
            };
            let peer_address = match managed_socket_address_from_info(address_info, true) {
                Ok(address) => address,
                Err(error) => {
                    rollback_managed_stream_socket(&mut context, &socket_id);
                    return Err(error);
                }
            };
            let expected_socket_present = if parent.domain == HostSocketDomain::Unix {
                context.process.unix_sockets.contains_key(&socket_id)
            } else {
                context.process.tcp_sockets.contains_key(&socket_id)
            };
            if !expected_socket_present {
                rollback_managed_stream_socket(&mut context, &socket_id);
                return Err(SidecarError::host(
                    "EIO",
                    "accept returned an unknown socket id",
                ));
            }
            let (accepted_fd, accepted_description_id) =
                match context.kernel.fd_open_external_socket(
                    EXECUTION_DRIVER_NAME,
                    context.process.kernel_pid,
                    false,
                    nonblocking,
                    close_on_exec,
                ) {
                    Ok(opened) => opened,
                    Err(error) => {
                        rollback_managed_stream_socket(&mut context, &socket_id);
                        return Err(kernel_error(error));
                    }
                };
            let accepted_lease = match context.kernel.fd_transfer(
                EXECUTION_DRIVER_NAME,
                context.process.kernel_pid,
                accepted_fd,
            ) {
                Ok(lease) => lease,
                Err(error) => {
                    rollback_managed_stream_socket(&mut context, &socket_id);
                    if let Err(close_error) = context.kernel.fd_close(
                        EXECUTION_DRIVER_NAME,
                        context.process.kernel_pid,
                        accepted_fd,
                    ) {
                        eprintln!(
                            "ERR_AGENTOS_SOCKET_CLEANUP: failed to close accepted fd after transfer failure: {close_error}"
                        );
                    }
                    return Err(kernel_error(error));
                }
            };
            let mut accepted = ManagedHostNetDescription::new(
                parent.domain,
                SocketKind::Stream,
                accepted_lease,
                context.process.kernel_pid,
            );
            accepted.receive_timeout_ms = parent.receive_timeout_ms;
            accepted.reuse_address = parent.reuse_address;
            accepted.linger_enabled = parent.linger_enabled;
            accepted.linger_seconds = parent.linger_seconds;
            accepted.no_delay = parent.no_delay;
            accepted.keep_alive = parent.keep_alive;
            accepted.local_address = local_address;
            accepted.peer_address = peer_address;
            let accepted_route = if parent.domain == HostSocketDomain::Unix {
                ManagedHostNetRoute::UnixSocket(socket_id.clone())
            } else {
                ManagedHostNetRoute::TcpSocket(socket_id.clone())
            };
            accepted
                .routes
                .insert(context.process.kernel_pid, accepted_route);
            if descriptions.contains_key(&accepted_description_id) {
                rollback_managed_stream_socket(&mut context, &socket_id);
                if let Err(error) = context.kernel.fd_close(
                    EXECUTION_DRIVER_NAME,
                    context.process.kernel_pid,
                    accepted_fd,
                ) {
                    eprintln!(
                        "ERR_AGENTOS_SOCKET_CLEANUP: failed to close accepted fd after description collision: {error}"
                    );
                }
                return Err(SidecarError::host(
                    "EEXIST",
                    "kernel reused a live accepted socket description id",
                ));
            }
            descriptions.insert(accepted_description_id, accepted);
            let Value::Object(fields) = &mut value else {
                unreachable!("accept response validated as an object")
            };
            fields.insert("fd".to_owned(), json!(accepted_fd));
            fields.insert(
                "descriptionId".to_owned(),
                json!(accepted_description_id.to_string()),
            );
            Ok(HostServiceResponse::Json(value))
        }
        NetworkOperation::Receive {
            fd,
            max_bytes,
            flags,
            deadline_ms,
        } => {
            let description_id = managed_fd_description_id(&context, fd)?;
            let route = managed_route(&context, description_id)?;
            let peek = flags & 0x0002 != 0;
            let wait_ms = deadline_ms.unwrap_or_default();
            match route {
                ManagedHostNetRoute::TcpSocket(socket_id)
                | ManagedHostNetRoute::UnixSocket(socket_id) => service_managed_network_operation(
                    managed_socket_context(&mut context),
                    NetworkOperation::ManagedRead {
                        socket_id: bounded_managed_id(socket_id)?,
                        max_bytes: max_bytes.get() as u64,
                        peek,
                        wait_ms,
                    },
                ),
                ManagedHostNetRoute::UdpSocket(_) => Err(SidecarError::host(
                    "ERR_AGENTOS_CONTEXT_DISPATCH_REQUIRED",
                    "UDP receive requires the asynchronous fd poll dispatcher",
                )),
                _ => Err(SidecarError::host("ENOTCONN", "socket is not connected")),
            }
        }
        NetworkOperation::Send {
            fd,
            bytes,
            flags: _,
            address,
            ..
        } => {
            let description_id = managed_fd_description_id(&context, fd)?;
            let route = managed_route(&context, description_id)?;
            match route {
                ManagedHostNetRoute::TcpSocket(socket_id)
                | ManagedHostNetRoute::UnixSocket(socket_id) => service_managed_network_operation(
                    managed_socket_context(&mut context),
                    NetworkOperation::ManagedWrite {
                        socket_id: bounded_managed_id(socket_id)?,
                        bytes,
                    },
                ),
                ManagedHostNetRoute::UdpSocket(socket_id) => {
                    let (host, port) = match address {
                        Some(SocketAddress::Inet { host, port }) => (Some(host), Some(port)),
                        None => (None, None),
                        _ => {
                            return Err(SidecarError::host(
                                "EAFNOSUPPORT",
                                "UDP send requires INET address",
                            ))
                        }
                    };
                    let response = service_managed_udp_operation(
                        ManagedUdpServiceRequest {
                            bridge: context.bridge,
                            kernel: &mut *context.kernel,
                            vm_id: context.vm_id,
                            dns: context.dns,
                            socket_paths: context.socket_paths,
                            process: &mut *context.process,
                            kernel_readiness: context.kernel_readiness.clone(),
                            capabilities: context.capabilities.clone(),
                        },
                        NetworkOperation::ManagedUdpSend {
                            socket_id: bounded_managed_id(socket_id.clone())?,
                            bytes,
                            host,
                            port,
                        },
                    )?;
                    let local_address = context
                        .process
                        .udp_sockets
                        .get(&socket_id)
                        .and_then(ActiveUdpSocket::local_addr)
                        .map(|address| SocketAddress::Inet {
                            host: BoundedString::try_new(
                                address.ip().to_string(),
                                &PayloadLimit::new("runtime.network.maxHostBytes", HOST_BYTES)
                                    .expect("static nonzero host limit"),
                            )
                            .expect("IP address remains within host limit"),
                            port: address.port(),
                        });
                    if local_address.is_some() {
                        update_managed_description(&context, description_id, |description| {
                            description.local_address = local_address;
                        })?;
                    }
                    Ok(response)
                }
                _ => Err(SidecarError::host("ENOTCONN", "socket is not connected")),
            }
        }
        NetworkOperation::LocalAddress { fd } | NetworkOperation::PeerAddress { fd } => {
            let peer = matches!(operation, NetworkOperation::PeerAddress { .. });
            let description_id = managed_fd_description_id(&context, fd)?;
            let description = managed_description(&context, description_id)?;
            let address = if peer {
                description.peer_address
            } else {
                description.local_address.or(description.bound_address)
            };
            if let Some(address) = address {
                return Ok(HostServiceResponse::Json(socket_address_value(address)));
            }
            if peer {
                return Err(SidecarError::host("ENOTCONN", "socket has no peer address"));
            }
            if description.domain == HostSocketDomain::Unix {
                // Linux reports an unbound AF_UNIX socket as an unnamed Unix
                // address. An empty value is deliberately encoded by the
                // executor adapter as `unix-unnamed`.
                return Ok(HostServiceResponse::Json(json!({})));
            }
            Err(SidecarError::host("EINVAL", "socket is not bound"))
        }
        NetworkOperation::SetOption { fd, name, value } => {
            let description_id = managed_fd_description_id(&context, fd)?;
            let registry = Arc::clone(&context.managed_descriptions);
            let mut descriptions = registry.lock().map_err(|_| {
                SidecarError::host("EIO", "managed description registry lock poisoned")
            })?;
            match (name, value) {
                (SocketOptionName::ReuseAddress, SocketOptionValue::Bool(value)) => {
                    // The agentOS TCP namespace has no TIME_WAIT state, so
                    // there is no host bind conflict for SO_REUSEADDR to
                    // relax. Retain the open-description option nonetheless:
                    // dup/fork aliases and getsockopt observe Linux-compatible
                    // state, and both executor adapters share this owner.
                    descriptions
                        .get_mut(&description_id)
                        .ok_or_else(|| {
                            SidecarError::host("ENOTSOCK", "managed socket description disappeared")
                        })?
                        .reuse_address = value;
                }
                (SocketOptionName::Linger, SocketOptionValue::Linger { enabled, seconds }) => {
                    let description = descriptions.get_mut(&description_id).ok_or_else(|| {
                        SidecarError::host("ENOTSOCK", "managed socket description disappeared")
                    })?;
                    description.linger_enabled = enabled;
                    description.linger_seconds = seconds;
                }
                (SocketOptionName::ReceiveTimeout, SocketOptionValue::DurationMs(value)) => {
                    descriptions
                        .get_mut(&description_id)
                        .ok_or_else(|| {
                            SidecarError::host("ENOTSOCK", "managed socket description disappeared")
                        })?
                        .receive_timeout_ms = value;
                }
                (SocketOptionName::NoDelay, SocketOptionValue::Bool(value)) => {
                    let route = descriptions
                        .get(&description_id)
                        .and_then(|description| {
                            description.route_for(context.process.kernel_pid).cloned()
                        })
                        .ok_or_else(|| {
                            SidecarError::host("ESTALE", "managed socket route is missing")
                        })?;
                    let ManagedHostNetRoute::TcpSocket(id) = &route else {
                        return Err(SidecarError::host(
                            "ENOPROTOOPT",
                            "TCP_NODELAY requires TCP",
                        ));
                    };
                    context
                        .process
                        .tcp_sockets
                        .get_mut(id)
                        .ok_or_else(|| SidecarError::host("EBADF", "managed TCP route is stale"))?
                        .set_no_delay(value)?;
                    descriptions
                        .get_mut(&description_id)
                        .expect("managed socket description remains locked")
                        .no_delay = value;
                }
                (SocketOptionName::KeepAlive, SocketOptionValue::Bool(value)) => {
                    let route = descriptions
                        .get(&description_id)
                        .and_then(|description| {
                            description.route_for(context.process.kernel_pid).cloned()
                        })
                        .ok_or_else(|| {
                            SidecarError::host("ESTALE", "managed socket route is missing")
                        })?;
                    let ManagedHostNetRoute::TcpSocket(id) = &route else {
                        return Err(SidecarError::host(
                            "ENOPROTOOPT",
                            "SO_KEEPALIVE requires TCP",
                        ));
                    };
                    context
                        .process
                        .tcp_sockets
                        .get_mut(id)
                        .ok_or_else(|| SidecarError::host("EBADF", "managed TCP route is stale"))?
                        .set_keep_alive(value, None)?;
                    descriptions
                        .get_mut(&description_id)
                        .expect("managed socket description remains locked")
                        .keep_alive = value;
                }
                _ => {
                    return Err(SidecarError::host(
                        "ENOPROTOOPT",
                        "socket option is unsupported for this transport",
                    ))
                }
            }
            Ok(Value::Null.into())
        }
        NetworkOperation::GetOption { fd, name } => {
            let description_id = managed_fd_description_id(&context, fd)?;
            let description = managed_description(&context, description_id)?;
            let value = match name {
                SocketOptionName::Error => json!(0),
                SocketOptionName::ReuseAddress => json!(description.reuse_address),
                SocketOptionName::Linger => json!({
                    "enabled": description.linger_enabled,
                    "seconds": description.linger_seconds,
                }),
                SocketOptionName::ReceiveTimeout => {
                    json!({ "durationMs": description.receive_timeout_ms })
                }
                SocketOptionName::NoDelay => json!(description.no_delay),
                SocketOptionName::KeepAlive => json!(description.keep_alive),
                _ => {
                    return Err(SidecarError::host(
                        "ENOPROTOOPT",
                        "socket option getter is unsupported",
                    ))
                }
            };
            Ok(value.into())
        }
        NetworkOperation::TlsConnect {
            fd,
            server_name,
            alpn,
            reject_unauthorized,
            ..
        } => {
            let description_id = managed_fd_description_id(&context, fd)?;
            let route = managed_route(&context, description_id)?;
            let ManagedHostNetRoute::TcpSocket(socket_id) = route else {
                return Err(SidecarError::host(
                    "ENOTCONN",
                    "TLS upgrade requires a connected TCP socket",
                ));
            };
            let protocols = alpn
                .into_vec()
                .into_iter()
                .map(|bytes| String::from_utf8_lossy(bytes.as_slice()).into_owned())
                .collect::<Vec<_>>();
            service_managed_network_operation(
                managed_socket_context(&mut context),
                NetworkOperation::ManagedTlsUpgrade {
                    socket_id: bounded_managed_id(socket_id)?,
                    options_json: bounded_managed_payload(
                        json!({
                            "servername": server_name.as_str(),
                            "ALPNProtocols": protocols,
                            "rejectUnauthorized": reject_unauthorized,
                        })
                        .to_string(),
                    )?,
                },
            )
        }
        NetworkOperation::Poll {
            interests,
            deadline_ms: _,
        } => {
            let mut ready = Vec::new();
            for interest in interests.into_vec() {
                let description_id = managed_fd_description_id(&context, interest.fd)?;
                let readable = if interest.readable {
                    match &managed_route(&context, description_id)? {
                        ManagedHostNetRoute::TcpSocket(id)
                        | ManagedHostNetRoute::UnixSocket(id) => {
                            let response = service_managed_network_operation(
                                managed_socket_context(&mut context),
                                NetworkOperation::ManagedPoll {
                                    socket_id: bounded_managed_id(id.clone())?,
                                    wait_ms: 0,
                                },
                            )?;
                            !matches!(response_value(&response), Some(Value::Null) | None)
                        }
                        ManagedHostNetRoute::TcpListener(_)
                        | ManagedHostNetRoute::UnixListener(_) => false,
                        ManagedHostNetRoute::UdpSocket(id) => context
                            .process
                            .udp_sockets
                            .get(id)
                            .and_then(|socket| socket.pending_datagram.lock().ok())
                            .is_some_and(|pending| pending.is_some()),
                        _ => false,
                    }
                } else {
                    false
                };
                ready.push(json!({
                    "fd": interest.fd,
                    "readable": readable,
                    "writable": interest.writable,
                }));
            }
            Ok(json!({ "ready": ready }).into())
        }
        other => Err(SidecarError::host(
            "EINVAL",
            format!("unsupported managed fd operation: {other:?}"),
        )),
    }
}

fn bounded_managed_id(value: String) -> Result<BoundedString, SidecarError> {
    BoundedString::try_new(
        value,
        &PayloadLimit::new("runtime.network.maxCapabilityIdBytes", MANAGED_ID_BYTES)
            .map_err(SidecarError::from)?,
    )
    .map_err(SidecarError::from)
}

fn bounded_managed_payload(value: String) -> Result<BoundedString, SidecarError> {
    BoundedString::try_new(
        value,
        &PayloadLimit::new("limits.reactor.maxBridgeRequestBytes", 1024 * 1024)
            .map_err(SidecarError::from)?,
    )
    .map_err(SidecarError::from)
}

fn socket_address_value(address: SocketAddress) -> Value {
    match address {
        SocketAddress::Inet { host, port } => json!({ "address": host.as_str(), "port": port }),
        SocketAddress::UnixPath(path) => json!({ "path": path.as_str() }),
        SocketAddress::UnixAbstract(bytes) => {
            json!({ "abstractPathHex": encode_hex_bytes(bytes.as_slice()) })
        }
        SocketAddress::UnixAutobind => json!({ "autobind": true }),
    }
}

fn encode_hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn is_managed_fd_operation(operation: &NetworkOperation) -> bool {
    matches!(
        operation,
        NetworkOperation::Socket { .. }
            | NetworkOperation::Bind { .. }
            | NetworkOperation::Connect { .. }
            | NetworkOperation::Listen { .. }
            | NetworkOperation::Accept { .. }
            | NetworkOperation::Validate { .. }
            | NetworkOperation::Receive { .. }
            | NetworkOperation::Send { .. }
            | NetworkOperation::LocalAddress { .. }
            | NetworkOperation::PeerAddress { .. }
            | NetworkOperation::GetOption { .. }
            | NetworkOperation::SetOption { .. }
            | NetworkOperation::Poll { .. }
            | NetworkOperation::TlsConnect { .. }
    )
}

fn settle_managed_network_response<B>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    runtime: agentos_runtime::RuntimeContext,
    reply: DirectHostReplyHandle,
    operation: &str,
    response: Result<HostServiceResponse, SidecarError>,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let response = match response {
        Ok(HostServiceResponse::Deferred {
            receiver,
            timeout,
            task_class,
        }) => {
            enqueue_deferred_host_service_completion(
                sidecar, vm_id, process_id, runtime, reply, operation, receiver, timeout,
                task_class,
            )?;
            return Ok(());
        }
        other => other,
    };
    settle_execution_host_call(&reply, response)
}

fn is_managed_endpoint_operation(operation: &NetworkOperation) -> bool {
    matches!(
        operation,
        NetworkOperation::ManagedBindUnix { .. }
            | NetworkOperation::ManagedBindConnectedUnix { .. }
            | NetworkOperation::ManagedReserveTcpPort { .. }
            | NetworkOperation::ManagedReleaseTcpPort { .. }
            | NetworkOperation::ManagedConnect { .. }
            | NetworkOperation::ManagedListen { .. }
    )
}

fn is_direct_managed_operation(operation: &NetworkOperation) -> bool {
    matches!(
        operation,
        NetworkOperation::ManagedBindUnix { .. }
            | NetworkOperation::ManagedBindConnectedUnix { .. }
            | NetworkOperation::ManagedReserveTcpPort { .. }
            | NetworkOperation::ManagedReleaseTcpPort { .. }
            | NetworkOperation::ManagedConnect { .. }
            | NetworkOperation::ManagedListen { .. }
            | NetworkOperation::ManagedPoll { .. }
            | NetworkOperation::ManagedWaitConnect { .. }
            | NetworkOperation::ManagedRead { .. }
            | NetworkOperation::ManagedWrite { .. }
            | NetworkOperation::ManagedDestroy { .. }
            | NetworkOperation::ManagedAccept { .. }
            | NetworkOperation::ManagedCloseListener { .. }
            | NetworkOperation::ManagedTlsUpgrade { .. }
            | NetworkOperation::ManagedUdpCreate { .. }
            | NetworkOperation::ManagedUdpBind { .. }
            | NetworkOperation::ManagedUdpSend { .. }
            | NetworkOperation::ManagedUdpPoll { .. }
            | NetworkOperation::ManagedUdpClose { .. }
    )
}

pub(super) fn dispatch_context_udp_poll<B>(
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
    let kernel_pid = reply.identity().pid;
    let NetworkOperation::ManagedUdpPoll {
        socket_id,
        wait_ms,
        peek,
        max_bytes,
    } = operation
    else {
        return Err(SidecarError::host(
            "EINVAL",
            "UDP poll dispatcher received a different network operation",
        ));
    };
    if !reply.claim().map_err(SidecarError::from)? {
        return Ok(());
    }
    let process_path = sidecar
        .vms
        .get(vm_id)
        .and_then(|vm| vm.active_processes.get(process_id))
        .and_then(|root| NativeSidecar::<B>::active_process_path_by_kernel_pid(root, kernel_pid))
        .ok_or_else(|| {
            SidecarError::host(
                "ESTALE",
                format!("active process for kernel pid {kernel_pid} disappeared"),
            )
        })?;
    let path = process_path.iter().map(String::as_str).collect::<Vec<_>>();
    let socket = sidecar
        .vms
        .get(vm_id)
        .and_then(|vm| vm.active_processes.get(process_id))
        .and_then(|root| NativeSidecar::<B>::active_process_by_path(root, &path))
        .and_then(|process| process.udp_sockets.get(socket_id.as_str()))
        .ok_or_else(|| SidecarError::host("EBADF", "unknown UDP socket"));
    let socket = match socket {
        Ok(socket) => socket,
        Err(error) => {
            reply
                .fail(host_service_error(&error))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
    };
    let requested_wait = Duration::from_millis(wait_ms);
    let operation_deadline = socket.poll_handle().operation_deadline();
    let wait = requested_wait.min(operation_deadline);
    dispatch_claimed_context_udp_poll(
        sidecar,
        vm_id,
        process_id,
        ManagedUdpPollRecheck {
            root_process_id: process_id.to_owned(),
            process_path,
            socket_id: socket_id.into_string(),
            peek,
            max_bytes,
            deadline: Instant::now() + wait,
            operation_deadline: (requested_wait >= operation_deadline)
                .then_some(operation_deadline),
            deadline_warning_emitted: false,
            native_probe_completed: false,
            native_event: None,
            reply,
            fair_turn: None,
        },
    )
}

pub(in crate::execution) fn dispatch_descendant_context_udp_poll<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    root_process_id: &str,
    process_path: &[&str],
    operation: NetworkOperation,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let NetworkOperation::ManagedUdpPoll {
        socket_id,
        wait_ms,
        peek,
        max_bytes,
    } = operation
    else {
        return Err(SidecarError::host(
            "EINVAL",
            "descendant UDP poll dispatcher received a different network operation",
        ));
    };
    let mut pending = ManagedUdpPollRecheck {
        root_process_id: root_process_id.to_owned(),
        process_path: process_path
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect(),
        socket_id: socket_id.into_string(),
        peek,
        max_bytes,
        deadline: Instant::now(),
        operation_deadline: None,
        deadline_warning_emitted: false,
        native_probe_completed: false,
        native_event: None,
        reply,
        fair_turn: None,
    };
    if !validate_udp_poll_target(sidecar, vm_id, &pending)? {
        return Ok(());
    }
    if !pending.reply.claim().map_err(SidecarError::from)? {
        return Ok(());
    }
    let socket = sidecar
        .vms
        .get(vm_id)
        .and_then(|vm| udp_poll_target_process::<B>(vm, &pending))
        .and_then(|process| process.udp_sockets.get(&pending.socket_id));
    let Some(socket) = socket else {
        pending
            .reply
            .fail(HostServiceError::new("EBADF", "unknown UDP socket"))
            .map_err(SidecarError::from)?;
        return Ok(());
    };
    let requested_wait = Duration::from_millis(wait_ms);
    let operation_deadline = socket.poll_handle().operation_deadline();
    let wait = requested_wait.min(operation_deadline);
    pending.deadline = Instant::now() + wait;
    pending.operation_deadline =
        (requested_wait >= operation_deadline).then_some(operation_deadline);
    dispatch_claimed_context_udp_poll(sidecar, vm_id, root_process_id, pending)
}

pub(in crate::execution) fn dispatch_claimed_context_udp_poll<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    mut pending: ManagedUdpPollRecheck,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if process_id != pending.root_process_id || !validate_udp_poll_target(sidecar, vm_id, &pending)?
    {
        return Ok(());
    }

    let socket_paths = build_socket_path_context(
        sidecar
            .vms
            .get(vm_id)
            .expect("validated UDP-poll VM remains registered"),
    )?;
    let vm = sidecar
        .vms
        .get(vm_id)
        .expect("validated UDP-poll VM remains registered");
    let wait_handle = vm.kernel.poll_wait_handle();
    let observed_generation = wait_handle.snapshot();
    let process = udp_poll_target_process::<B>(vm, &pending)
        .expect("validated UDP-poll process remains registered");
    let socket = match process.udp_sockets.get(&pending.socket_id) {
        Some(socket) => socket,
        None => {
            if let Some(turn) = pending.fair_turn.take() {
                turn.complete(FairBudget::default(), false)
                    .map_err(|error| SidecarError::Execution(error.to_string()))?;
            }
            pending
                .reply
                .fail(HostServiceError::new("EBADF", "unknown UDP socket"))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
    };
    let poll_handle = socket.poll_handle();
    if let Some(event) = pending.native_event.take() {
        return settle_or_buffer_udp_poll_event(
            &pending.reply,
            &socket_paths,
            &poll_handle.pending_datagram,
            Some(event),
            pending.peek,
            pending.max_bytes,
        );
    }
    if let Some(event) = buffered_udp_poll_event(&poll_handle, pending.peek)? {
        return settle_udp_poll_event(
            &pending.reply,
            &socket_paths,
            Some(event),
            pending.max_bytes,
        );
    }
    let kernel_readable = socket.kernel_readable(&vm.kernel, process.kernel_pid)?;

    if kernel_readable {
        if let Some(turn) = pending.fair_turn.take() {
            let vm = sidecar
                .vms
                .get_mut(vm_id)
                .expect("validated UDP-poll VM remains registered");
            let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
            let process = udp_poll_target_process_mut::<B>(active_processes, &pending)
                .expect("validated UDP-poll process remains registered");
            let socket = process
                .udp_sockets
                .get(&pending.socket_id)
                .expect("validated UDP socket remains registered");
            let event = socket.consume_kernel_datagram(kernel, process.kernel_pid, turn)?;
            return settle_or_buffer_udp_poll_event(
                &pending.reply,
                &socket_paths,
                &socket.pending_datagram,
                event,
                pending.peek,
                pending.max_bytes,
            );
        }
        return spawn_udp_fair_reentry(sidecar, vm_id, process_id, poll_handle, pending);
    }

    if let Some(turn) = pending.fair_turn.take() {
        turn.complete(FairBudget::default(), false)
            .map_err(|error| SidecarError::Execution(error.to_string()))?;
    }
    if poll_handle.native_commands.is_none() {
        if let Some(limit) = pending.operation_deadline {
            let mut deadline = crate::execution::OperationDeadlineTracker::from_deadline(
                pending.deadline,
                limit,
                pending.deadline_warning_emitted,
            );
            deadline.observe("managed UDP poll wait");
            pending.deadline_warning_emitted = deadline.warning_emitted();
        }
    }
    if Instant::now() >= pending.deadline
        && (poll_handle.native_commands.is_none() || pending.native_probe_completed)
    {
        return settle_udp_poll_event(&pending.reply, &socket_paths, None, pending.max_bytes);
    }
    spawn_udp_wait_reentry(
        sidecar,
        vm_id,
        process_id,
        poll_handle,
        wait_handle,
        observed_generation,
        pending,
    )
}

fn udp_poll_target_process<'a, B>(
    vm: &'a VmState,
    pending: &ManagedUdpPollRecheck,
) -> Option<&'a ActiveProcess>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let root = vm.active_processes.get(&pending.root_process_id)?;
    if pending.process_path.is_empty() {
        return Some(root);
    }
    let path = pending
        .process_path
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    NativeSidecar::<B>::active_process_by_path(root, &path)
}

fn udp_poll_target_process_mut<'a, B>(
    active_processes: &'a mut BTreeMap<String, ActiveProcess>,
    pending: &ManagedUdpPollRecheck,
) -> Option<&'a mut ActiveProcess>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let root = active_processes.get_mut(&pending.root_process_id)?;
    if pending.process_path.is_empty() {
        return Some(root);
    }
    let path = pending
        .process_path
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    NativeSidecar::<B>::active_process_by_path_mut(root, &path)
}

fn validate_udp_poll_target<B>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
    pending: &ManagedUdpPollRecheck,
) -> Result<bool, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let Some(vm) = sidecar.vms.get(vm_id) else {
        pending
            .reply
            .fail(HostServiceError::new(
                "ESTALE",
                "UDP poll VM no longer exists",
            ))
            .map_err(SidecarError::from)?;
        return Ok(false);
    };
    let Some(process) = udp_poll_target_process::<B>(vm, pending) else {
        pending
            .reply
            .fail(HostServiceError::new(
                "ESTALE",
                "UDP poll process target no longer exists",
            ))
            .map_err(SidecarError::from)?;
        return Ok(false);
    };
    let identity = pending.reply.identity();
    if identity.generation != vm.generation || identity.pid != process.kernel_pid {
        pending
            .reply
            .fail(
                HostServiceError::new(
                    "ESTALE",
                    "UDP poll identity does not match the generation-bound process target",
                )
                .with_details(json!({
                    "expectedGeneration": vm.generation,
                    "expectedPid": process.kernel_pid,
                    "observedGeneration": identity.generation,
                    "observedPid": identity.pid,
                })),
            )
            .map_err(SidecarError::from)?;
        return Ok(false);
    }
    Ok(true)
}

fn spawn_udp_fair_reentry<B>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    poll_handle: ActiveUdpPollHandle,
    pending: ManagedUdpPollRecheck,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let (runtime, connection_id, session_id) = udp_reentry_context(sidecar, vm_id)?;
    let sender = sidecar.process_event_sender.clone();
    let notify = Arc::clone(&sidecar.process_event_notify);
    let vm_id = vm_id.to_owned();
    let process_id = process_id.to_owned();
    let failure_reply = pending.reply.clone();
    if let Err(error) = runtime.spawn(agentos_runtime::TaskClass::Udp, async move {
        let mut pending = pending;
        match poll_handle.acquire_fair_turn().await {
            Ok(turn) => pending.fair_turn = Some(turn),
            Err(error) => {
                if let Err(reply_error) = pending.reply.fail(host_service_error(&error)) {
                    eprintln!(
                        "ERR_AGENTOS_HOST_REPLY_SETTLEMENT: failed to publish managed UDP fair-turn error: {reply_error}"
                    );
                }
                return;
            }
        }
        send_udp_reentry(
            sender,
            notify,
            connection_id,
            session_id,
            vm_id,
            process_id,
            pending,
        )
        .await;
    }) {
        failure_reply
            .fail(host_service_error(&SidecarError::from(error)))
            .map_err(SidecarError::from)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_udp_wait_reentry<B>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    poll_handle: ActiveUdpPollHandle,
    wait_handle: agentos_kernel::poll::PollWaitHandle,
    observed_generation: u64,
    pending: ManagedUdpPollRecheck,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let (runtime, connection_id, session_id) = udp_reentry_context(sidecar, vm_id)?;
    let sender = sidecar.process_event_sender.clone();
    let notify = Arc::clone(&sidecar.process_event_notify);
    let vm_id = vm_id.to_owned();
    let process_id = process_id.to_owned();
    let failure_reply = pending.reply.clone();
    if let Err(error) = runtime.spawn(agentos_runtime::TaskClass::Udp, async move {
        let mut pending = pending;
        // Register readiness before the native owner probe so an event arriving
        // in the gap remains observable after an empty completion.
        let native_ready = poll_handle.read_event_notify.notified();
        match poll_handle.poll_native_once().await {
            Ok(Some(event)) => {
                pending.native_probe_completed = true;
                pending.native_event = Some(event);
                send_udp_reentry(
                    sender,
                    notify,
                    connection_id,
                    session_id,
                    vm_id,
                    process_id,
                    pending,
                )
                .await;
                return;
            }
            Ok(None) => pending.native_probe_completed = true,
            Err(error) => {
                pending.native_probe_completed = true;
                pending.native_event = Some(DatagramEvent::Error {
                    code: Some(host_service_error_code(&error)),
                    message: host_service_error_message(&error),
                });
                send_udp_reentry(
                    sender,
                    notify,
                    connection_id,
                    session_id,
                    vm_id,
                    process_id,
                    pending,
                )
                .await;
                return;
            }
        }
        let remaining = if let Some(limit) = pending.operation_deadline {
            let mut deadline = crate::execution::OperationDeadlineTracker::from_deadline(
                pending.deadline,
                limit,
                pending.deadline_warning_emitted,
            );
            deadline.observe("managed UDP poll wait");
            pending.deadline_warning_emitted = deadline.warning_emitted();
            if deadline.expired() {
                send_udp_reentry(
                    sender,
                    notify,
                    connection_id,
                    session_id,
                    vm_id,
                    process_id,
                    pending,
                )
                .await;
                return;
            }
            deadline.remaining_until_next_edge()
        } else {
            pending.deadline.saturating_duration_since(Instant::now())
        };
        if !remaining.is_zero() {
            tokio::select! {
                _ = native_ready => {}
                _ = wait_handle.wait_for_change_async(observed_generation) => {}
                _ = tokio::time::sleep(remaining) => {}
            }
        }
        send_udp_reentry(
            sender,
            notify,
            connection_id,
            session_id,
            vm_id,
            process_id,
            pending,
        )
        .await;
    }) {
        failure_reply
            .fail(host_service_error(&SidecarError::from(error)))
            .map_err(SidecarError::from)?;
    }
    Ok(())
}

fn udp_reentry_context<B>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
) -> Result<(agentos_runtime::RuntimeContext, String, String), SidecarError> {
    let vm = sidecar
        .vms
        .get(vm_id)
        .ok_or_else(|| SidecarError::host("ESTALE", "UDP-poll VM no longer exists"))?;
    Ok((
        vm.runtime_context.clone(),
        vm.connection_id.clone(),
        vm.session_id.clone(),
    ))
}

#[allow(clippy::too_many_arguments)]
async fn send_udp_reentry(
    sender: tokio::sync::mpsc::Sender<ProcessEventEnvelope>,
    notify: Arc<tokio::sync::Notify>,
    connection_id: String,
    session_id: String,
    vm_id: String,
    process_id: String,
    pending: ManagedUdpPollRecheck,
) {
    debug_assert_eq!(process_id, pending.root_process_id);
    let process_id = pending.root_process_id.clone();
    let envelope = ProcessEventEnvelope {
        connection_id,
        session_id,
        vm_id,
        process_id,
        event: ActiveExecutionEvent::ManagedUdpPollRecheck(Box::new(pending)),
    };
    if let Err(error) = sender.send(envelope).await {
        if let ActiveExecutionEvent::ManagedUdpPollRecheck(pending) = error.0.event {
            if let Err(reply_error) = pending.reply.fail(HostServiceError::new(
                "ECANCELED",
                "UDP poll re-entry lane closed",
            )) {
                eprintln!(
                    "ERR_AGENTOS_HOST_REPLY_SETTLEMENT: failed to cancel UDP poll after re-entry lane closure: {reply_error}"
                );
            }
        }
        eprintln!("ERR_AGENTOS_PROCESS_EVENT_CHANNEL_CLOSED: UDP poll could not re-enter");
    } else {
        notify.notify_one();
    }
}

fn settle_udp_poll_event(
    reply: &DirectHostReplyHandle,
    socket_paths: &SocketPathContext,
    event: Option<DatagramEvent>,
    max_bytes: Option<BoundedUsize>,
) -> Result<(), SidecarError> {
    match event {
        Some(DatagramEvent::Message {
            data,
            remote_addr,
            _byte_reservation,
            _datagram_reservation,
            _udp_byte_reservation,
            _udp_datagram_reservation,
        }) => {
            let family = SocketFamily::from_ip(remote_addr.ip());
            let guest_port = if is_loopback_ip(remote_addr.ip()) {
                socket_paths
                    .guest_udp_port_for_host_port(family, remote_addr.port())
                    .unwrap_or(remote_addr.port())
            } else {
                remote_addr.port()
            };
            let mut value = remote_endpoint_value(&remote_addr, guest_port);
            if let Value::Object(fields) = &mut value {
                fields.insert("type".to_owned(), Value::String("message".to_owned()));
                let data = max_bytes
                    .map(|limit| &data[..data.len().min(limit.get())])
                    .unwrap_or(data.as_slice());
                fields.insert("data".to_owned(), host_bytes_value(data));
            }
            reply
                .succeed_retained(
                    HostCallReply::Json(value),
                    (
                        _byte_reservation,
                        _datagram_reservation,
                        _udp_byte_reservation,
                        _udp_datagram_reservation,
                    ),
                )
                .map_err(SidecarError::from)
        }
        Some(DatagramEvent::Error { code, message }) => reply
            .fail(HostServiceError::new(
                code.as_deref().unwrap_or("EIO"),
                message,
            ))
            .map_err(SidecarError::from),
        None => reply
            .succeed(HostCallReply::Json(if max_bytes.is_some() {
                json!({ "kind": "wouldBlock" })
            } else {
                Value::Null
            }))
            .map_err(SidecarError::from),
    }
}

fn buffered_udp_poll_event(
    poll_handle: &ActiveUdpPollHandle,
    peek: bool,
) -> Result<Option<DatagramEvent>, SidecarError> {
    let mut pending = poll_handle.pending_datagram.lock().map_err(|_| {
        SidecarError::host(
            "ERR_AGENTOS_UDP_READ_STATE_POISONED",
            "managed UDP pending-datagram lock poisoned",
        )
    })?;
    Ok(if peek {
        pending.clone()
    } else {
        pending.take()
    })
}

fn settle_or_buffer_udp_poll_event(
    reply: &DirectHostReplyHandle,
    socket_paths: &SocketPathContext,
    pending_datagram: &Arc<Mutex<Option<DatagramEvent>>>,
    event: Option<DatagramEvent>,
    peek: bool,
    max_bytes: Option<BoundedUsize>,
) -> Result<(), SidecarError> {
    if peek {
        if let Some(event) = event.as_ref() {
            let mut pending = pending_datagram.lock().map_err(|_| {
                SidecarError::host(
                    "ERR_AGENTOS_UDP_READ_STATE_POISONED",
                    "managed UDP pending-datagram lock poisoned",
                )
            })?;
            if pending.is_none() {
                *pending = Some(event.clone());
            }
        }
    }
    settle_udp_poll_event(reply, socket_paths, event, max_bytes)
}

impl SidecarHostCapability<NetworkOperation> for NetworkCapability {
    fn requires_claim(operation: &NetworkOperation) -> bool {
        matches!(
            operation,
            NetworkOperation::SocketPair { .. }
                | NetworkOperation::Shutdown { .. }
                | NetworkOperation::ManagedBindUnix { .. }
                | NetworkOperation::ManagedBindConnectedUnix { .. }
                | NetworkOperation::ManagedReserveTcpPort { .. }
                | NetworkOperation::ManagedReleaseTcpPort { .. }
                | NetworkOperation::ManagedConnect { .. }
                | NetworkOperation::ManagedListen { .. }
                | NetworkOperation::ManagedPoll { .. }
                | NetworkOperation::ManagedRead { .. }
                | NetworkOperation::ManagedWrite { .. }
                | NetworkOperation::ManagedDestroy { .. }
                | NetworkOperation::ManagedAccept { .. }
                | NetworkOperation::ManagedCloseListener { .. }
                | NetworkOperation::ManagedTlsUpgrade { .. }
                | NetworkOperation::ManagedUdpCreate { .. }
                | NetworkOperation::ManagedUdpBind { .. }
                | NetworkOperation::ManagedUdpSend { .. }
                | NetworkOperation::ManagedUdpPoll { .. }
                | NetworkOperation::ManagedUdpClose { .. }
                | NetworkOperation::SendDescriptorRights { .. }
                | NetworkOperation::ReceiveDescriptorRights { .. }
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

#[cfg(test)]
mod managed_tests {
    use super::*;
    use crate::execution::host_dispatch::inventory::WASM_RUNNER_RPC_INVENTORY;
    use agentos_execution::backend::{DirectHostReplyTarget, HostCallIdentity};
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::{KernelVmConfig, SpawnOptions};
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::MemoryFileSystem;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct ReplyTarget {
        result: Mutex<Option<(bool, Result<HostCallReply, HostServiceError>)>>,
    }

    impl DirectHostReplyTarget for ReplyTarget {
        fn claim(&self, _: u64) -> Result<bool, HostServiceError> {
            Ok(true)
        }

        fn respond(
            &self,
            _: u64,
            claimed: bool,
            result: Result<HostCallReply, HostServiceError>,
        ) -> Result<(), HostServiceError> {
            *self.result.lock().expect("reply result") = Some((claimed, result));
            Ok(())
        }
    }

    fn request(method: &str) -> HostRpcRequest {
        let args = match method {
            "net.bind_unix" => vec![json!({"path":"/tmp/s"})],
            "net.bind_connected_unix" => vec![json!({"socketId":"s1","path":"/tmp/s"})],
            "net.connect" => vec![json!({"host":"127.0.0.1","port":80})],
            "net.listen" => vec![json!({"host":"127.0.0.1","port":0,"backlog":16})],
            "net.reserve_tcp_port" => vec![json!({"host":"127.0.0.1","port":0})],
            "net.release_tcp_port" => vec![json!("r1")],
            "net.poll" => vec![json!("s1"), json!(0)],
            "net.socket_wait_connect" | "net.socket_read" | "net.destroy" => vec![json!("s1")],
            "net.write" => vec![json!("s1"), json!("x")],
            "net.server_accept" | "net.server_close" => vec![json!("l1")],
            "net.socket_upgrade_tls" => vec![json!("s1"), json!("{}")],
            "dgram.createSocket" => vec![json!({"type":"udp4"})],
            "dgram.bind" => vec![json!("u1"), json!({"address":"127.0.0.1","port":0})],
            "dgram.send" => vec![
                json!("u1"),
                json!("x"),
                json!({"address":"127.0.0.1","port":7}),
            ],
            "dgram.poll" => vec![json!("u1"), json!(0)],
            "dgram.close" => vec![json!("u1")],
            other => panic!("missing managed network fixture for {other}"),
        };
        HostRpcRequest {
            id: 1,
            method: method.to_owned(),
            args,
            raw_bytes_args: HashMap::new(),
        }
    }

    fn managed_test_kernel(name: &str) -> SidecarKernel {
        let mut config = KernelVmConfig::new(name);
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register managed-network test driver");
        kernel
    }

    #[test]
    fn common_managed_network_and_completion_state_are_executor_neutral() {
        let managed_source = include_str!("network_compat.rs");
        let state_source = include_str!("../../state.rs");
        for forbidden in [
            ["Java", "script", "Net"].concat(),
            ["Java", "script", "Sync", "Rpc"].concat(),
            ["Py", "thon"].concat(),
        ] {
            assert!(
                !managed_source.contains(&forbidden),
                "common managed network source contains executor-specific type {forbidden}"
            );
        }
        assert!(state_source.contains("struct HostCallCompletion"));
        assert!(!state_source.contains(&["Java", "script", "Sync", "RpcCompletion"].concat()));
    }

    #[test]
    fn parent_route_retires_while_child_and_queued_scm_right_retain_description() {
        let mut kernel = managed_test_kernel("managed-parent-child-scm-lifecycle");
        let parent = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(EXECUTION_DRIVER_NAME.to_owned()),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn parent");
        let (fd, description_id) = kernel
            .fd_open_external_socket(EXECUTION_DRIVER_NAME, parent.pid(), false, false, false)
            .expect("open managed external socket");
        let lease = kernel
            .fd_transfer(EXECUTION_DRIVER_NAME, parent.pid(), fd)
            .expect("capture canonical description lease");
        let child = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(EXECUTION_DRIVER_NAME.to_owned()),
                    parent_pid: Some(parent.pid()),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn inheriting child");
        assert_eq!(
            kernel
                .fd_description_identity(EXECUTION_DRIVER_NAME, child.pid(), fd)
                .expect("child inherits external socket")
                .0,
            description_id
        );

        let mut description = ManagedHostNetDescription::new(
            HostSocketDomain::Inet4,
            SocketKind::Stream,
            lease,
            parent.pid(),
        );
        description
            .routes
            .insert(child.pid(), ManagedHostNetRoute::Unbound);
        let mut descriptions = BTreeMap::from([(description_id, description)]);
        let queued_scm_right = kernel
            .fd_transfer(EXECUTION_DRIVER_NAME, parent.pid(), fd)
            .expect("queue SCM_RIGHTS description lease");

        kernel
            .fd_close(EXECUTION_DRIVER_NAME, parent.pid(), fd)
            .expect("parent final close");
        let (retired_parent_routes, retired_descriptions) =
            prune_managed_registry_routes(&mut descriptions, parent.pid(), [(description_id, 0)]);
        assert_eq!(retired_parent_routes.len(), 1);
        assert!(retired_descriptions.is_empty());
        assert_eq!(
            descriptions
                .get(&description_id)
                .expect("global description retained")
                .routes
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [child.pid()]
        );
        drop(retired_parent_routes);

        drop(queued_scm_right);
        kernel
            .fd_close(EXECUTION_DRIVER_NAME, child.pid(), fd)
            .expect("child final close");
        let (retired_child_routes, retired_descriptions) =
            prune_managed_registry_routes(&mut descriptions, child.pid(), [(description_id, 0)]);
        assert_eq!(retired_child_routes.len(), 1);
        assert_eq!(retired_descriptions.len(), 1);
        assert!(descriptions.is_empty(), "final close retires registry row");
    }

    #[test]
    fn exec_cloexec_retires_managed_route_and_description() {
        let mut kernel = managed_test_kernel("managed-exec-cloexec-lifecycle");
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(EXECUTION_DRIVER_NAME.to_owned()),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn process");
        let (fd, description_id) = kernel
            .fd_open_external_socket(EXECUTION_DRIVER_NAME, process.pid(), false, false, true)
            .expect("open CLOEXEC managed socket");
        let lease = kernel
            .fd_transfer(EXECUTION_DRIVER_NAME, process.pid(), fd)
            .expect("capture description lease");
        let mut descriptions = BTreeMap::from([(
            description_id,
            ManagedHostNetDescription::new(
                HostSocketDomain::Inet4,
                SocketKind::Stream,
                lease,
                process.pid(),
            ),
        )]);
        kernel
            .exec_process_retaining_internal_fds(
                EXECUTION_DRIVER_NAME,
                process.pid(),
                WASM_COMMAND,
                Vec::new(),
                BTreeMap::new(),
                String::new(),
                &[],
                &[],
                None,
                None,
            )
            .expect("exec closes CLOEXEC fd");
        let aliases = kernel
            .fd_description_alias_count(EXECUTION_DRIVER_NAME, process.pid(), description_id)
            .expect("query post-exec aliases");
        assert_eq!(aliases, 0);
        let (_, retired) = prune_managed_registry_routes(
            &mut descriptions,
            process.pid(),
            [(description_id, aliases)],
        );
        assert_eq!(retired.len(), 1);
        assert!(descriptions.is_empty());
    }

    #[test]
    fn process_exit_final_fd_cleanup_retires_managed_description() {
        let mut kernel = managed_test_kernel("managed-process-exit-lifecycle");
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(EXECUTION_DRIVER_NAME.to_owned()),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn process");
        let (fd, description_id) = kernel
            .fd_open_external_socket(EXECUTION_DRIVER_NAME, process.pid(), false, false, false)
            .expect("open managed socket");
        let lease = kernel
            .fd_transfer(EXECUTION_DRIVER_NAME, process.pid(), fd)
            .expect("capture description lease");
        let mut descriptions = BTreeMap::from([(
            description_id,
            ManagedHostNetDescription::new(
                HostSocketDomain::Inet4,
                SocketKind::Stream,
                lease,
                process.pid(),
            ),
        )]);
        process.finish(0);
        kernel.waitpid(process.pid()).expect("reap process");
        assert_eq!(
            descriptions
                .get(&description_id)
                .expect("description exists before sidecar retirement")
                .lease
                .ref_count(),
            1,
            "kernel process cleanup released its final fd reference"
        );
        let (_, retired) =
            prune_managed_registry_routes(&mut descriptions, process.pid(), [(description_id, 0)]);
        assert_eq!(retired.len(), 1);
        assert!(descriptions.is_empty());
    }

    fn frozen_network_request(method: &str) -> HostRpcRequest {
        match method {
            "__kernel_poll" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!([{ "fd": 0, "events": 1 }]), json!(0)],
                raw_bytes_args: HashMap::new(),
            },
            "process.posix_poll" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!([{ "fd": 0, "events": 1 }]), json!(0), Value::Null],
                raw_bytes_args: HashMap::new(),
            },
            "dns.lookup" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!({ "hostname": "localhost", "family": 4 })],
                raw_bytes_args: HashMap::new(),
            },
            "dns.resolveRawRr" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!({ "hostname": "localhost", "rrtype": "A" })],
                raw_bytes_args: HashMap::new(),
            },
            "process.fd_socketpair" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(1), json!(true), json!(true)],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_fd_open" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(2), json!(6), json!(true), json!(true)],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_bind" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![
                    json!(3),
                    json!({ "type": "inet", "host": "127.0.0.1", "port": 0 }),
                ],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_connect" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![
                    json!(3),
                    json!({ "type": "inet", "host": "127.0.0.1", "port": 80 }),
                    json!(0),
                ],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_listen" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3), json!(16)],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_accept" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3), json!(true), json!(true), json!(0)],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_recv" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3), json!(1), json!(0), json!(0)],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_send" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3), json!("x"), json!(0), Value::Null, json!(0)],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_local_address" | "process.hostnet_peer_address" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3)],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_get_option" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3), json!("error")],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_set_option" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3), json!("reuse-address"), json!(true)],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_poll" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!([{ "fd": 3, "readable": true }]), json!(0)],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_tls_connect" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3), json!("localhost"), json!([]), json!(0)],
                raw_bytes_args: HashMap::new(),
            },
            "process.hostnet_validate" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3), json!(true)],
                raw_bytes_args: HashMap::new(),
            },
            "process.fd_socket_shutdown" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3), json!(2)],
                raw_bytes_args: HashMap::new(),
            },
            "process.fd_sendmsg_rights" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![json!(3), json!("x"), json!([4]), json!(0)],
                raw_bytes_args: HashMap::new(),
            },
            "process.fd_recvmsg_rights" => HostRpcRequest {
                id: 1,
                method: method.to_owned(),
                args: vec![
                    json!(3),
                    json!(64),
                    json!(4),
                    json!(true),
                    json!(false),
                    json!(true),
                    json!(false),
                ],
                raw_bytes_args: HashMap::new(),
            },
            _ => request(method),
        }
    }

    #[test]
    fn guest_wait_deadline_rejects_unbounded_duration_without_panicking() {
        let error = checked_deferred_guest_wait_deadline(u64::MAX)
            .expect_err("an unbounded guest wait must be rejected");

        assert_eq!(error.code, "EINVAL");
        assert!(error.message.contains("guestWaitDurationMs"));
    }

    #[test]
    fn every_frozen_net_and_dgram_rpc_has_a_bounded_typed_route() {
        for method in WASM_RUNNER_RPC_INVENTORY
            .iter()
            .copied()
            .filter(|method| method.starts_with("net.") || method.starts_with("dgram."))
        {
            let operation = decode_managed(&request(method), 1024 * 1024)
                .unwrap_or_else(|error| panic!("decode {method}: {error}"));
            assert!(
                operation.is_some(),
                "{method} must not use legacy fallthrough"
            );
            let operation = operation.expect("checked typed operation");
            assert!(
                is_direct_managed_operation(&operation),
                "{method} must execute directly without reconstructing an RPC request"
            );
            assert!(
                descriptor_rights_compat_request(1, operation).is_err(),
                "{method} must be rejected by the descriptor-rights-only compatibility adapter"
            );
        }
    }

    #[test]
    fn managed_read_preserves_the_complete_runner_request() {
        let request = HostRpcRequest {
            id: 1,
            method: "net.socket_read".to_owned(),
            args: vec![json!("s1"), json!(4096), json!(true), json!(17)],
            raw_bytes_args: HashMap::new(),
        };
        assert!(matches!(
            decode_managed(&request, 1024).expect("decode managed read"),
            Some(NetworkOperation::ManagedRead {
                max_bytes: 4096,
                peek: true,
                wait_ms: 17,
                ..
            })
        ));
    }

    #[test]
    fn every_frozen_network_family_rpc_has_a_typed_operation_route() {
        use crate::execution::host_dispatch::inventory::{capability_family, HostCapabilityFamily};
        for method in WASM_RUNNER_RPC_INVENTORY
            .iter()
            .copied()
            .filter(|method| capability_family(method) == Some(HostCapabilityFamily::Network))
        {
            let decoded = super::super::decode_host_operation(
                &frozen_network_request(method),
                true,
                1024 * 1024,
            )
            .unwrap_or_else(|error| panic!("decode frozen Network RPC {method}: {error}"));
            assert!(
                matches!(decoded, Some(HostOperation::Network(_))),
                "{method} must route to a typed Network operation"
            );
        }
    }

    #[test]
    fn host_network_scm_rights_metadata_stays_an_explicit_compatibility_projection() {
        let mut request = frozen_network_request("process.fd_sendmsg_rights");
        request.args[2] = json!([{
            "kind": "hostNet",
            "domain": 1,
            "socketType": 1,
            "protocol": 0,
            "nonblocking": false,
            "listening": false
        }]);
        assert!(
            super::super::decode_host_operation(&request, true, 1024)
                .expect("decode compatibility host-network transfer")
                .is_none(),
            "V8-owned host-network description metadata is adapter state, not a neutral kernel right"
        );
    }

    #[test]
    fn managed_decoder_rejects_oversized_id_path_and_bytes() {
        let mut id = request("net.destroy");
        id.args[0] = json!("x".repeat(MANAGED_ID_BYTES + 1));
        assert_eq!(
            decode_managed(&id, 1024).unwrap_err().code(),
            Some("ENAMETOOLONG")
        );
        let mut path = request("net.bind_unix");
        path.args[0] = json!({"path":"x".repeat(UNIX_PATH_BYTES + 1)});
        assert_eq!(
            decode_managed(&path, 1024 * 1024).unwrap_err().code(),
            Some("ENAMETOOLONG")
        );
        let mut bytes = request("net.write");
        bytes.args[1] = json!("12345");
        assert_eq!(decode_managed(&bytes, 4).unwrap_err().code(), Some("E2BIG"));
    }

    #[test]
    fn kernel_poll_is_a_bounded_typed_network_operation() {
        let request = HostRpcRequest {
            id: 1,
            method: "__kernel_poll".to_owned(),
            args: vec![
                json!([
                    { "fd": 0, "events": 1 },
                    { "fd": 1, "events": 4 }
                ]),
                Value::Null,
            ],
            raw_bytes_args: HashMap::new(),
        };
        assert!(matches!(
            super::super::decode_host_operation(&request, true, 1024).expect("decode poll"),
            Some(HostOperation::Network(NetworkOperation::KernelPoll {
                timeout_ms: None,
                ..
            }))
        ));

        let mut oversized = request;
        oversized.args[0] = Value::Array(
            (0..=1024)
                .map(|fd| json!({ "fd": fd, "events": 1 }))
                .collect(),
        );
        let error = super::super::decode_host_operation(&oversized, true, 1024)
            .expect_err("oversized poll set must fail");
        assert_eq!(error.code(), Some("E2BIG"));

        let ppoll = HostRpcRequest {
            id: 2,
            method: "process.posix_poll".to_owned(),
            args: vec![
                json!([{ "fd": 7, "events": POSIX_POLLIN }]),
                Value::Null,
                json!([2, 15]),
            ],
            raw_bytes_args: HashMap::new(),
        };
        assert!(matches!(
            super::super::decode_host_operation(&ppoll, true, 1024).expect("decode ppoll"),
            Some(HostOperation::Network(NetworkOperation::PosixPoll {
                timeout_ms: None,
                signal_mask: Some(SignalSetValue(bits)),
                ..
            })) if bits == (1_u64 << 1) | (1_u64 << 14)
        ));
    }

    #[test]
    fn closed_udp_reentry_lane_cancels_the_claimed_reply() {
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create UDP re-entry test runtime");
        let target = Arc::new(ReplyTarget::default());
        let reply = DirectHostReplyHandle::new(
            HostCallIdentity {
                generation: 3,
                pid: 9,
                call_id: 11,
            },
            target.clone(),
            4096,
        )
        .expect("create direct reply");
        assert!(reply.claim().expect("claim reply"));
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        drop(receiver);
        runtime.context().handle().block_on(send_udp_reentry(
            sender,
            Arc::new(tokio::sync::Notify::new()),
            "connection".to_owned(),
            "session".to_owned(),
            "vm".to_owned(),
            "process".to_owned(),
            ManagedUdpPollRecheck {
                root_process_id: "process".to_owned(),
                process_path: Vec::new(),
                socket_id: "udp-1".to_owned(),
                peek: false,
                max_bytes: None,
                deadline: Instant::now(),
                operation_deadline: None,
                deadline_warning_emitted: false,
                native_probe_completed: false,
                native_event: None,
                reply,
                fair_turn: None,
            },
        ));
        let result = target
            .result
            .lock()
            .expect("reply result")
            .take()
            .expect("closed lane settles reply");
        assert!(result.0, "reply must stay claimed through cancellation");
        assert_eq!(result.1.expect_err("cancellation error").code, "ECANCELED");
    }

    #[test]
    fn deferred_posix_poll_wake_is_durable_with_a_competing_broker_waiter() {
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create POSIX-poll wake test runtime");
        let notify = Arc::new(tokio::sync::Notify::new());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let lane = DeferredPosixPollWakeLane {
            sender,
            notify: Arc::clone(&notify),
            connection_id: "connection".to_owned(),
            session_id: "session".to_owned(),
            vm_id: "vm".to_owned(),
            process_id: "process".to_owned(),
        };

        let envelope = runtime.context().handle().block_on(async move {
            // Two waiters model the owner pump and a competing deferred poll.
            // Both registered waiters must wake, and one coalesced permit must
            // remain for an owner that registers immediately after publication.
            let mut first_waiter = Box::pin(notify.notified());
            let mut second_waiter = Box::pin(notify.notified());
            first_waiter.as_mut().enable();
            second_waiter.as_mut().enable();
            lane.publish().await;
            tokio::time::timeout(Duration::from_secs(1), async {
                first_waiter.as_mut().await;
                second_waiter.as_mut().await;
            })
            .await
            .expect("both registered broker waiters complete");
            tokio::time::timeout(Duration::from_secs(1), notify.notified())
                .await
                .expect("one coalesced broker permit remains");
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .expect("durable wake arrives before deadline")
                .expect("wake lane remains open")
        });

        assert_eq!(envelope.process_id, "process");
        assert!(matches!(
            envelope.event,
            ActiveExecutionEvent::DeferredPosixPollWake
        ));

        // A full event lane must not put the publisher to sleep before the
        // owner receives a wake that lets it drain the lane.
        let notify = Arc::new(tokio::sync::Notify::new());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let filler = ProcessEventEnvelope {
            connection_id: "connection".to_owned(),
            session_id: "session".to_owned(),
            vm_id: "vm".to_owned(),
            process_id: "filler".to_owned(),
            event: ActiveExecutionEvent::DeferredPosixPollWake,
        };
        sender.try_send(filler).expect("fill bounded owner lane");
        let lane = DeferredPosixPollWakeLane {
            sender,
            notify: Arc::clone(&notify),
            connection_id: "connection".to_owned(),
            session_id: "session".to_owned(),
            vm_id: "vm".to_owned(),
            process_id: "deadline".to_owned(),
        };

        runtime.context().handle().block_on(async move {
            let mut owner_wake = Box::pin(notify.notified());
            owner_wake.as_mut().enable();
            let publisher = tokio::spawn(lane.publish());
            tokio::time::timeout(Duration::from_secs(1), owner_wake.as_mut())
                .await
                .expect("saturated lane wakes owner before publisher admission");
            receiver.recv().await.expect("owner drains filler");
            publisher.await.expect("deadline publisher completes");
            let envelope = receiver.recv().await.expect("owner receives deadline wake");
            assert_eq!(envelope.process_id, "deadline");
        });
    }

    #[test]
    fn managed_posix_poll_waits_on_socket_readiness_after_broker_wake_is_consumed() {
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create managed POSIX-poll readiness test runtime");
        runtime.context().handle().block_on(async {
            let broker_notify = Arc::new(tokio::sync::Notify::new());
            let socket_notify = Arc::new(tokio::sync::Notify::new());

            let competing_broker = {
                let notify = Arc::clone(&broker_notify);
                tokio::spawn(async move { notify.notified().await })
            };
            let socket_waiter =
                tokio::spawn(wait_for_managed_posix_poll_readiness(vec![Arc::clone(
                    &socket_notify,
                )]));
            tokio::task::yield_now().await;

            // Model the owner pump consuming the generic broker notification.
            // Managed transport readiness must still wake the poll directly.
            broker_notify.notify_one();
            competing_broker
                .await
                .expect("competing broker waiter completes");
            socket_notify.notify_one();
            tokio::time::timeout(Duration::from_secs(1), socket_waiter)
                .await
                .expect("socket readiness wakes POSIX poll")
                .expect("socket readiness waiter completes");
        });
    }

    #[test]
    fn indexed_posix_poll_preserves_duplicate_fd_event_masks() {
        let interests = [
            KernelPollInterest {
                fd: 8,
                events: POSIX_POLLIN,
            },
            KernelPollInterest { fd: 8, events: 0 },
        ];
        let response = indexed_posix_poll_response(&interests, &[Some(POSIX_POLLIN), Some(0)]);

        assert_eq!(response["readyCount"], 1);
        let fds = response["fds"].as_array().expect("poll fd response array");
        assert_eq!(fds.len(), 2);
        assert_eq!(fds[0]["fd"], 8);
        assert_eq!(fds[0]["events"], POSIX_POLLIN);
        assert_eq!(fds[0]["revents"], POSIX_POLLIN);
        assert_eq!(fds[1]["fd"], 8);
        assert_eq!(fds[1]["events"], 0);
        assert_eq!(fds[1]["revents"], 0);
    }

    #[test]
    fn descendant_udp_reentry_preserves_root_lane_and_process_path() {
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create UDP re-entry test runtime");
        let target = Arc::new(ReplyTarget::default());
        let reply = DirectHostReplyHandle::new(
            HostCallIdentity {
                generation: 3,
                pid: 9,
                call_id: 12,
            },
            target,
            4096,
        )
        .expect("create direct reply");
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        runtime.context().handle().block_on(send_udp_reentry(
            sender,
            Arc::new(tokio::sync::Notify::new()),
            "connection".to_owned(),
            "session".to_owned(),
            "vm".to_owned(),
            "root".to_owned(),
            ManagedUdpPollRecheck {
                root_process_id: "root".to_owned(),
                process_path: vec!["child-1".to_owned(), "child-2".to_owned()],
                socket_id: "udp-1".to_owned(),
                peek: false,
                max_bytes: None,
                deadline: Instant::now(),
                operation_deadline: None,
                deadline_warning_emitted: false,
                native_probe_completed: false,
                native_event: None,
                reply,
                fair_turn: None,
            },
        ));

        let envelope = receiver.try_recv().expect("UDP re-entry envelope");
        assert_eq!(envelope.process_id, "root");
        let ActiveExecutionEvent::ManagedUdpPollRecheck(pending) = envelope.event else {
            panic!("expected UDP poll re-entry event");
        };
        assert_eq!(pending.root_process_id, "root");
        assert_eq!(pending.process_path, ["child-1", "child-2"]);
        assert_eq!(pending.reply.identity().generation, 3);
        assert_eq!(pending.reply.identity().pid, 9);
    }
}
