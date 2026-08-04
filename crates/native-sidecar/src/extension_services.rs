use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore};

use crate::execution::{
    prepare_owned_python_process_event_service, service_owned_child_bridge_event,
    service_owned_python_socket_connect_completion, OwnedChildBridgeEventService,
    OwnedJavascriptEventService, OwnedPythonEventService, OwnedPythonSocketCompletionService,
};
use crate::extension::{
    ExtensionBufferedProcessOutput, ExtensionFuture, ExtensionServices, ProjectedAgentLaunchEntry,
};
use crate::filesystem::guest_filesystem_call_vm;
use crate::ownership_coordinator::{
    CoordinatorOperationPermit, InternalVmEventAdmission, OwnershipCoordinator,
    OwnershipCoordinatorError, VmDisposal,
};
use crate::process_event_broker::{ProcessEventBroker, ProcessEventLease, ProcessEventTarget};
use crate::protocol::{
    CloseStdinRequest, DisposeReason, EventFrame, ExecuteRequest, GuestFilesystemCallRequest,
    GuestFilesystemResultResponse, KillProcessRequest, OwnershipScope, ProcessKilledResponse,
    ProcessStartedResponse, RequestFrame, RequestPayload, ResponsePayload, StdinClosedResponse,
    StdinWrittenResponse, WriteStdinRequest,
};
use crate::request_operations::{
    OperationCancellation, OperationCancellationReason, RequestOperationMetadata,
    RequestOrderingKey,
};
use crate::service::NativeSidecar;
use crate::state::SidecarError;
use crate::stdio::LocalBridge;
use crate::vm_sqlite::SharedVmSqliteDatabase;

type Reply<T> = oneshot::Sender<Result<T, SidecarError>>;

type ExtensionServiceCompletionMutation = Box<
    dyn FnOnce(&mut NativeSidecar<LocalBridge>) -> Option<PreparedExtensionServiceCommand>
        + 'static,
>;
type ExtensionServiceCommandFuture =
    Pin<Box<dyn Future<Output = ExtensionServiceCompletionMutation> + 'static>>;

/// Command work that no longer borrows the protocol router's `NativeSidecar`.
///
/// The owned future runs under the bounded stdio supervisor. Its terminal
/// mutation is deliberately synchronous: it may publish a result or perform a
/// short coordinator update, but it may never wait on external work.
pub(crate) struct PreparedExtensionServiceCommand {
    operation: &'static str,
    future: ExtensionServiceCommandFuture,
    panic_reply: Box<dyn FnOnce(SidecarError) + 'static>,
    admission: Option<ExtensionServiceAdmission>,
}

pub(crate) struct CompletedExtensionServiceCommand {
    operation: &'static str,
    mutation: ExtensionServiceCompletionMutation,
    failure_reply: Box<dyn FnOnce(SidecarError) + 'static>,
    _permit: Option<CoordinatorOperationPermit>,
}

struct ExtensionServiceAdmission {
    coordinator: OwnershipCoordinator,
    metadata: RequestOperationMetadata,
    cancellation: OperationCancellation,
    pre_admitted: Option<CoordinatorOperationPermit>,
    deferred_tracked: Option<CoordinatorOperationPermit>,
    class: ExtensionServiceAdmissionClass,
}

#[derive(Clone, Copy)]
enum ExtensionServiceAdmissionClass {
    Ordinary,
    InternalVmEvent,
}

pub(crate) enum VmEventAdmissionResult {
    Admitted(PreparedExtensionServiceCommand),
    Deferred(PreparedExtensionServiceCommand),
}

impl PreparedExtensionServiceCommand {
    pub(crate) fn operation(&self) -> &'static str {
        self.operation
    }

    /// Register a VM-bound claimed event before it enters the local pending
    /// queue. VM-operation admission is intentionally non-suspending; holding
    /// its permit makes disposal cancel and drain work that has been claimed
    /// but has not yet reached a task slot.
    pub(crate) fn admit_vm_event_nowait(mut self) -> Result<VmEventAdmissionResult, SidecarError> {
        let Some(admission) = self.admission.as_mut() else {
            return Ok(VmEventAdmissionResult::Admitted(self));
        };
        if admission.pre_admitted.is_some() {
            return Ok(VmEventAdmissionResult::Admitted(self));
        }
        if !matches!(
            admission.class,
            ExtensionServiceAdmissionClass::InternalVmEvent
        ) {
            let message = String::from(
                "ERR_AGENTOS_PROCESS_EVENT_ADMISSION_CLASS: claimed VM event did not use internal-event admission",
            );
            (self.panic_reply)(SidecarError::InvalidState(message.clone()));
            return Err(SidecarError::InvalidState(message));
        }
        if let Some(mut permit) = admission.deferred_tracked.take() {
            match permit.try_activate_deferred_internal_event() {
                Ok(true) => {
                    admission.pre_admitted = Some(permit);
                    return Ok(VmEventAdmissionResult::Admitted(self));
                }
                Ok(false) => {
                    admission.deferred_tracked = Some(permit);
                    return Ok(VmEventAdmissionResult::Deferred(self));
                }
                Err(error) => {
                    let message = error.to_string();
                    (self.panic_reply)(SidecarError::InvalidState(message.clone()));
                    return Err(SidecarError::InvalidState(message));
                }
            }
        }
        match admission
            .coordinator
            .admit_internal_vm_event(&admission.metadata, admission.cancellation.clone())
        {
            Ok(InternalVmEventAdmission::Admitted(permit)) => {
                admission.pre_admitted = Some(permit);
                Ok(VmEventAdmissionResult::Admitted(self))
            }
            Ok(InternalVmEventAdmission::Deferred(permit)) => {
                tracing::debug!(
                    operation = self.operation,
                    "claimed internal process event remains durably pending at its admission bound"
                );
                admission.deferred_tracked = Some(permit);
                Ok(VmEventAdmissionResult::Deferred(self))
            }
            Err(error) => {
                let message = error.to_string();
                (self.panic_reply)(SidecarError::InvalidState(message.clone()));
                Err(SidecarError::InvalidState(message))
            }
        }
    }

    pub(crate) fn cancel_before_schedule(self, reason: OperationCancellationReason) {
        let PreparedExtensionServiceCommand {
            operation,
            panic_reply,
            admission,
            ..
        } = self;
        panic_reply(SidecarError::InvalidState(format!(
            "ERR_AGENTOS_EXTENSION_SERVICE_CANCELLED: extension service {operation} cancelled before task admission: {reason:?}"
        )));
        drop(admission);
    }

    pub(crate) async fn execute_supervised(self) -> Option<CompletedExtensionServiceCommand> {
        let PreparedExtensionServiceCommand {
            operation,
            future,
            panic_reply,
            admission,
        } = self;
        let (permit, cancellation) = match admission {
            Some(mut admission) => {
                let cancellation = admission.cancellation;
                match admission.pre_admitted.take() {
                    Some(permit) => (Some(permit), Some(cancellation)),
                    None => {
                        let admitted = match admission.class {
                            ExtensionServiceAdmissionClass::Ordinary => {
                                admission
                                    .coordinator
                                    .admit(&admission.metadata, cancellation.clone())
                                    .await
                            }
                            ExtensionServiceAdmissionClass::InternalVmEvent => {
                                match admission.coordinator.admit_internal_vm_event(
                                    &admission.metadata,
                                    cancellation.clone(),
                                ) {
                                    Ok(InternalVmEventAdmission::Admitted(permit)) => Ok(permit),
                                    Ok(InternalVmEventAdmission::Deferred(permit)) => {
                                        drop(permit);
                                        Err(OwnershipCoordinatorError::OwnershipMismatch {
                                            expected: String::from(
                                                "claimed internal event pre-admitted before scheduling",
                                            ),
                                            actual: String::from(
                                                "untracked direct internal-event scheduling",
                                            ),
                                        })
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                        };
                        match admitted {
                            Ok(permit) => (Some(permit), Some(cancellation)),
                            Err(error) => {
                                panic_reply(SidecarError::InvalidState(error.to_string()));
                                return None;
                            }
                        }
                    }
                }
            }
            None => (None, None),
        };
        let mut task = tokio::task::spawn_local(future);
        let joined = match cancellation {
            Some(cancellation) => tokio::select! {
                result = &mut task => result,
                reason = cancellation.cancelled() => {
                    task.abort();
                    if let Err(error) = task.await {
                        if !error.is_cancelled() {
                            tracing::error!(
                                operation,
                                %error,
                                "ERR_AGENTOS_EXTENSION_SERVICE_CANCEL_JOIN: cancelled extension service task failed while being observed"
                            );
                        }
                    }
                    panic_reply(SidecarError::InvalidState(format!(
                        "ERR_AGENTOS_EXTENSION_SERVICE_CANCELLED: extension service {operation} cancelled: {reason:?}"
                    )));
                    return None;
                }
            },
            None => task.await,
        };
        match joined {
            Ok(mutation) => Some(CompletedExtensionServiceCommand {
                operation,
                mutation,
                failure_reply: panic_reply,
                _permit: permit,
            }),
            Err(error) => {
                let failure = SidecarError::Execution(format!(
                    "ERR_AGENTOS_EXTENSION_SERVICE_TASK_PANIC: extension service {operation} failed: {error}"
                ));
                tracing::error!(
                    operation,
                    %error,
                    "ERR_AGENTOS_EXTENSION_SERVICE_TASK_PANIC: extension service task failed"
                );
                panic_reply(failure);
                None
            }
        }
    }
}

impl CompletedExtensionServiceCommand {
    pub(crate) fn complete(
        self,
        sidecar: &mut NativeSidecar<LocalBridge>,
    ) -> Option<PreparedExtensionServiceCommand> {
        let CompletedExtensionServiceCommand {
            operation,
            mutation,
            failure_reply,
            _permit,
        } = self;
        let result = catch_unwind(AssertUnwindSafe(|| mutation(sidecar)));
        drop(_permit);
        match result {
            Ok(next) => next,
            Err(_) => {
                let error = SidecarError::Execution(format!(
                    "ERR_AGENTOS_EXTENSION_SERVICE_COMPLETION_PANIC: extension service {operation} completion mutation panicked"
                ));
                tracing::error!(
                    operation,
                    "ERR_AGENTOS_EXTENSION_SERVICE_COMPLETION_PANIC: extension service completion mutation panicked"
                );
                failure_reply(error);
                None
            }
        }
    }
}

fn with_vm_admission(
    prepared: PreparedExtensionServiceCommand,
    coordinator: &OwnershipCoordinator,
    ownership: &OwnershipScope,
) -> PreparedExtensionServiceCommand {
    with_vm_admission_class(
        prepared,
        coordinator,
        ownership,
        ExtensionServiceAdmissionClass::Ordinary,
    )
}

fn with_internal_vm_event_admission(
    prepared: PreparedExtensionServiceCommand,
    coordinator: &OwnershipCoordinator,
    ownership: &OwnershipScope,
) -> PreparedExtensionServiceCommand {
    with_vm_admission_class(
        prepared,
        coordinator,
        ownership,
        ExtensionServiceAdmissionClass::InternalVmEvent,
    )
}

fn with_vm_admission_class(
    mut prepared: PreparedExtensionServiceCommand,
    coordinator: &OwnershipCoordinator,
    ownership: &OwnershipScope,
    class: ExtensionServiceAdmissionClass,
) -> PreparedExtensionServiceCommand {
    let OwnershipScope::VmOwnership(scope) = ownership else {
        return prepared;
    };
    prepared.admission = Some(ExtensionServiceAdmission {
        coordinator: coordinator.clone(),
        metadata: RequestOperationMetadata::new(
            ownership.clone(),
            prepared.operation,
            RequestOrderingKey::VmOperation {
                connection_id: scope.connection_id.clone(),
                session_id: scope.session_id.clone(),
                vm_id: scope.vm_id.clone(),
            },
        ),
        cancellation: OperationCancellation::new(),
        pre_admitted: None,
        deferred_tracked: None,
        class,
    });
    prepared
}

struct SharedReply<T> {
    inner: Arc<StdMutex<Option<Reply<T>>>>,
}

impl<T> Clone for SharedReply<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: 'static> SharedReply<T> {
    fn new(reply: Reply<T>) -> Self {
        Self {
            inner: Arc::new(StdMutex::new(Some(reply))),
        }
    }

    fn send(&self, result: Result<T, SidecarError>, operation: &str) {
        if let Err(result) = self.try_send(result, operation) {
            tracing::debug!(
                operation,
                result = ?result.as_ref().err(),
                "extension service caller ended before its reply"
            );
        }
    }

    fn try_send(
        &self,
        result: Result<T, SidecarError>,
        operation: &str,
    ) -> Result<(), Result<T, SidecarError>> {
        let reply = match self.inner.lock() {
            Ok(mut reply) => reply.take(),
            Err(_) => {
                eprintln!("ERR_AGENTOS_EXTENSION_SERVICE_REPLY_POISONED: operation={operation}");
                return Err(result);
            }
        };
        let Some(reply) = reply else {
            tracing::error!(
                operation,
                "ERR_AGENTOS_EXTENSION_SERVICE_DOUBLE_REPLY: extension service command completed more than once"
            );
            return Err(result);
        };
        reply.send(result)
    }
}

fn prepared_command<T, F, Fut>(
    operation: &'static str,
    reply: Reply<T>,
    work: F,
) -> PreparedExtensionServiceCommand
where
    T: 'static,
    F: FnOnce() -> Fut + 'static,
    Fut: Future<Output = Result<T, SidecarError>> + 'static,
{
    let reply = SharedReply::new(reply);
    let completion_reply = reply.clone();
    let panic_reply = reply.clone();
    PreparedExtensionServiceCommand {
        operation,
        future: Box::pin(async move {
            let result = work().await;
            Box::new(move |_sidecar: &mut NativeSidecar<LocalBridge>| {
                completion_reply.send(result, operation);
                None
            }) as ExtensionServiceCompletionMutation
        }),
        panic_reply: Box::new(move |error| panic_reply.send(Err(error), operation)),
        admission: None,
    }
}

fn prepared_mutation<T, F>(
    operation: &'static str,
    reply: Reply<T>,
    mutation: F,
) -> PreparedExtensionServiceCommand
where
    T: 'static,
    F: FnOnce(&mut NativeSidecar<LocalBridge>) -> Result<T, SidecarError> + 'static,
{
    let reply = SharedReply::new(reply);
    let completion_reply = reply.clone();
    let panic_reply = reply.clone();
    PreparedExtensionServiceCommand {
        operation,
        future: Box::pin(async move {
            Box::new(move |sidecar: &mut NativeSidecar<LocalBridge>| {
                completion_reply.send(mutation(sidecar), operation);
                None
            }) as ExtensionServiceCompletionMutation
        }),
        panic_reply: Box::new(move |error| panic_reply.send(Err(error), operation)),
        admission: None,
    }
}

#[cfg(test)]
pub(crate) fn prepared_test_service_command<Fut>(
    operation: &'static str,
    future: Fut,
) -> (
    PreparedExtensionServiceCommand,
    oneshot::Receiver<Result<(), SidecarError>>,
)
where
    Fut: Future<Output = ()> + 'static,
{
    let (reply, response) = oneshot::channel();
    (
        prepared_command(operation, reply, move || async move {
            future.await;
            Ok(())
        }),
        response,
    )
}

#[cfg(test)]
pub(crate) fn prepared_test_vm_service_command<Fut>(
    operation: &'static str,
    coordinator: &OwnershipCoordinator,
    ownership: &OwnershipScope,
    future: Fut,
) -> (
    PreparedExtensionServiceCommand,
    oneshot::Receiver<Result<(), SidecarError>>,
)
where
    Fut: Future<Output = ()> + 'static,
{
    let (prepared, response) = prepared_test_service_command(operation, future);
    (
        with_vm_admission(prepared, coordinator, ownership),
        response,
    )
}

pub(crate) fn prepare_owned_javascript_event_service(
    sidecar: &mut NativeSidecar<LocalBridge>,
    coordinator: &OwnershipCoordinator,
    target: OwnedJavascriptEventService,
) -> PreparedExtensionServiceCommand {
    let ownership = target.ownership.clone();
    let panic_vm = target.vm.clone();
    let panic_process_id = target.process_id.clone();
    let panic_child_path = target.child_path.clone();
    let panic_request_id = target.request.id;
    let service = sidecar.prepare_owned_javascript_process_event_service(target);
    let operation = "service_javascript_process_event";
    let prepared = PreparedExtensionServiceCommand {
        operation,
        future: Box::pin(async move {
            let result = service.await;
            Box::new(move |_sidecar: &mut NativeSidecar<LocalBridge>| {
                if let Err(error) = result {
                    tracing::error!(
                        operation,
                        %error,
                        "ERR_AGENTOS_PROCESS_EVENT_SERVICE: owned JavaScript event service failed"
                    );
                }
                None
            }) as ExtensionServiceCompletionMutation
        }),
        panic_reply: Box::new(move |error| {
            tracing::error!(
                operation,
                %error,
                "ERR_AGENTOS_PROCESS_EVENT_SERVICE_PANIC: owned JavaScript event service failed"
            );
            let message = error.to_string();
            if let Err(reply_error) =
                panic_vm.try_command("reject owned JavaScript process event", |vm| {
                    let Some(mut process) = vm.active_processes.get_mut(&panic_process_id) else {
                        return Ok(());
                    };
                    for child_id in &panic_child_path {
                        let Some(child) = process.child_processes.get_mut(child_id) else {
                            return Ok(());
                        };
                        process = child;
                    }
                    process.execution.respond_javascript_sync_rpc_error(
                        panic_request_id,
                        "ERR_AGENTOS_PROCESS_EVENT_SERVICE",
                        message,
                    )
                })
            {
                tracing::debug!(
                    operation,
                    request_id = panic_request_id,
                    %reply_error,
                    "owned JavaScript event service failure reply was no longer pending"
                );
            }
        }),
        admission: None,
    };
    with_internal_vm_event_admission(prepared, coordinator, &ownership)
}

pub(crate) fn prepare_owned_python_event_service(
    sidecar: &NativeSidecar<LocalBridge>,
    coordinator: &OwnershipCoordinator,
    target: OwnedPythonEventService,
) -> PreparedExtensionServiceCommand {
    let ownership = target.ownership.clone();
    let request_id = target.request.id;
    let completion_responder = target.responder.clone();
    let panic_responder = target.responder.clone();
    let service = prepare_owned_python_process_event_service(sidecar, target);
    let operation = "service_python_process_event";
    let prepared = PreparedExtensionServiceCommand {
        operation,
        future: Box::pin(async move {
            let result = service.await;
            Box::new(move |_sidecar: &mut NativeSidecar<LocalBridge>| {
                if let Err(error) = result {
                    tracing::error!(
                        operation,
                        request_id,
                        %error,
                        "ERR_AGENTOS_PROCESS_EVENT_SERVICE: owned Python event service failed"
                    );
                    if let Err(reply_error) = completion_responder.respond_error(
                        request_id,
                        "ERR_AGENTOS_PYTHON_VFS_RPC",
                        error.to_string(),
                    ) {
                        tracing::debug!(
                            operation,
                            request_id,
                            %reply_error,
                            "owned Python event service failure reply was no longer pending"
                        );
                    }
                }
                None
            }) as ExtensionServiceCompletionMutation
        }),
        panic_reply: Box::new(move |error| {
            tracing::error!(
                operation,
                request_id,
                %error,
                "ERR_AGENTOS_PROCESS_EVENT_SERVICE_PANIC: owned Python event service failed"
            );
            if let Err(reply_error) = panic_responder.respond_error(
                request_id,
                "ERR_AGENTOS_PYTHON_VFS_RPC",
                error.to_string(),
            ) {
                tracing::debug!(
                    operation,
                    request_id,
                    %reply_error,
                    "owned Python event service panic/cancellation reply was no longer pending"
                );
            }
        }),
        admission: None,
    };
    with_internal_vm_event_admission(prepared, coordinator, &ownership)
}

pub(crate) fn prepare_owned_python_socket_completion_service(
    coordinator: &OwnershipCoordinator,
    target: OwnedPythonSocketCompletionService,
) -> PreparedExtensionServiceCommand {
    let (ownership, vm_id, process_id, child_path, vm, responder, completion, reservation) =
        target.into_parts();
    let request_id = completion.request_id;
    let completion_responder = responder.clone();
    let panic_responder = responder.clone();
    let operation = "complete_python_socket_connect";
    let prepared = PreparedExtensionServiceCommand {
        operation,
        future: Box::pin(async move {
            let _reservation = reservation;
            let result = service_owned_python_socket_connect_completion(
                vm, vm_id, process_id, child_path, responder, completion,
            )
            .await;
            Box::new(move |_sidecar: &mut NativeSidecar<LocalBridge>| {
                if let Err(error) = result {
                    tracing::error!(
                        operation,
                        request_id,
                        %error,
                        "ERR_AGENTOS_PROCESS_EVENT_SERVICE: owned Python socket completion failed"
                    );
                    if let Err(reply_error) = completion_responder.respond_error(
                        request_id,
                        "ERR_AGENTOS_PYTHON_VFS_RPC",
                        error.to_string(),
                    ) {
                        tracing::debug!(
                            operation,
                            request_id,
                            %reply_error,
                            "owned Python socket completion failure reply was no longer pending"
                        );
                    }
                }
                None
            }) as ExtensionServiceCompletionMutation
        }),
        panic_reply: Box::new(move |error| {
            tracing::error!(
                operation,
                request_id,
                %error,
                "ERR_AGENTOS_PROCESS_EVENT_SERVICE_PANIC: owned Python socket completion failed"
            );
            if let Err(reply_error) = panic_responder.respond_error(
                request_id,
                "ERR_AGENTOS_PYTHON_VFS_RPC",
                error.to_string(),
            ) {
                tracing::debug!(
                    operation,
                    request_id,
                    %reply_error,
                    "owned Python socket completion panic/cancellation reply was no longer pending"
                );
            }
        }),
        admission: None,
    };
    with_internal_vm_event_admission(prepared, coordinator, &ownership)
}

pub(crate) fn prepare_owned_child_bridge_event_service(
    coordinator: &OwnershipCoordinator,
    target: OwnedChildBridgeEventService,
) -> PreparedExtensionServiceCommand {
    let ownership = target.ownership().clone();
    let operation = "service_child_bridge_process_event";
    let prepared = PreparedExtensionServiceCommand {
        operation,
        future: Box::pin(async move {
            let result = service_owned_child_bridge_event(target).await;
            Box::new(move |_sidecar: &mut NativeSidecar<LocalBridge>| {
                if let Err(error) = result {
                    tracing::error!(
                        operation,
                        %error,
                        "ERR_AGENTOS_PROCESS_EVENT_SERVICE: owned child bridge event service failed"
                    );
                }
                None
            }) as ExtensionServiceCompletionMutation
        }),
        panic_reply: Box::new(move |error| {
            tracing::error!(
                operation,
                %error,
                "ERR_AGENTOS_PROCESS_EVENT_SERVICE_PANIC: owned child bridge event service failed"
            );
        }),
        admission: None,
    };
    with_internal_vm_event_admission(prepared, coordinator, &ownership)
}

pub(crate) enum OwnedProcessEventService {
    Javascript(OwnedJavascriptEventService),
    Python(OwnedPythonEventService),
    PythonSocketCompletion(OwnedPythonSocketCompletionService),
    ChildBridge(OwnedChildBridgeEventService),
}

pub(crate) fn prepare_owned_process_event_service(
    sidecar: &mut NativeSidecar<LocalBridge>,
    coordinator: &OwnershipCoordinator,
    target: OwnedProcessEventService,
) -> PreparedExtensionServiceCommand {
    match target {
        OwnedProcessEventService::Javascript(target) => {
            prepare_owned_javascript_event_service(sidecar, coordinator, target)
        }
        OwnedProcessEventService::Python(target) => {
            prepare_owned_python_event_service(sidecar, coordinator, target)
        }
        OwnedProcessEventService::PythonSocketCompletion(target) => {
            prepare_owned_python_socket_completion_service(coordinator, target)
        }
        OwnedProcessEventService::ChildBridge(target) => {
            prepare_owned_child_bridge_event_service(coordinator, target)
        }
    }
}

/// Short coordinator operations requested by an independently running
/// extension. This channel is bounded by the request supervisor's configured
/// in-flight limit; long extension waits remain in the caller, never here.
pub(crate) enum ExtensionServiceCommand {
    AcpTerminationGrace {
        reply: Reply<Duration>,
    },
    VmAcpLimits {
        ownership: OwnershipScope,
        reply: Reply<agentos_native_sidecar_core::limits::AcpLimits>,
    },
    VmDatabase {
        ownership: OwnershipScope,
        reply: Reply<Option<SharedVmSqliteDatabase>>,
    },
    ProjectedAgents {
        ownership: OwnershipScope,
        reply: Reply<Vec<ProjectedAgentLaunchEntry>>,
    },
    SpawnProcess {
        ownership: OwnershipScope,
        request: ExecuteRequest,
        reply: Reply<ProcessStartedResponse>,
    },
    WriteStdin {
        ownership: OwnershipScope,
        request: WriteStdinRequest,
        reply: Reply<StdinWrittenResponse>,
    },
    CloseStdin {
        ownership: OwnershipScope,
        request: CloseStdinRequest,
        reply: Reply<StdinClosedResponse>,
    },
    KillProcess {
        ownership: OwnershipScope,
        request: KillProcessRequest,
        reply: Reply<ProcessKilledResponse>,
    },
    PollEvent {
        ownership: OwnershipScope,
        reply: Reply<Option<EventFrame>>,
    },
    RouteProcessEvent {
        ownership: OwnershipScope,
        target: ProcessEventTarget,
        reply: Reply<bool>,
    },
    HandleProcessEvent {
        target: ProcessEventTarget,
        lease: ProcessEventLease,
        completion_store: CompletedProcessEventStore,
        completion_reservation: OwnedSemaphorePermit,
        reply: Reply<Option<EventFrame>>,
    },
    GuestFilesystemCall {
        ownership: OwnershipScope,
        request: GuestFilesystemCallRequest,
        reply: Reply<GuestFilesystemResultResponse>,
    },
    BindProcessToSession {
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
        reply: Reply<()>,
    },
    BindVmToSession {
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        reply: Reply<()>,
    },
    DisposeSessionResources {
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        reply: Reply<Vec<EventFrame>>,
    },
    StartBufferingProcessOutput {
        ownership: OwnershipScope,
        process_id: String,
        reply: Reply<()>,
    },
    ProbeBufferedProcessOutputHandoff {
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
        finalize_if_empty: bool,
        reply: Reply<Option<ExtensionBufferedProcessOutput>>,
    },
}

#[derive(Clone)]
pub(crate) struct RoutedExtensionServices {
    commands: mpsc::Sender<ExtensionServiceCommand>,
    /// Wakes extension-side probes only after the protocol coordinator has
    /// drained the runtime producer queues. Runtime producers use a separate
    /// notification owned exclusively by the central process-event pump, so
    /// an ACP/public-event waiter cannot consume the pump's only wake.
    routed_process_event_notify: Arc<Notify>,
    process_event_broker: Option<ProcessEventBroker>,
    completed_process_events: CompletedProcessEventStore,
}

#[derive(Clone)]
pub(crate) struct CompletedProcessEventStore {
    inner: Arc<StdMutex<BTreeMap<ProcessEventTarget, VecDeque<CompletedProcessEvent>>>>,
    budget: Arc<Semaphore>,
    notify: Arc<Notify>,
}

struct CompletedProcessEvent {
    event: EventFrame,
    _reservation: OwnedSemaphorePermit,
}

impl CompletedProcessEventStore {
    fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(StdMutex::new(BTreeMap::new())),
            budget: Arc::new(Semaphore::new(capacity.max(1))),
            notify: Arc::new(Notify::new()),
        }
    }

    fn take(&self, target: &ProcessEventTarget) -> Result<Option<EventFrame>, SidecarError> {
        let mut state = self.inner.lock().map_err(|_| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_COMPLETED_PROCESS_EVENT_POISONED: completed process event store lock poisoned",
            ))
        })?;
        let event = state
            .get_mut(target)
            .and_then(VecDeque::pop_front)
            .map(|completed| completed.event);
        if state.get(target).is_some_and(VecDeque::is_empty) {
            state.remove(target);
        }
        Ok(event)
    }

    fn retain(
        &self,
        target: ProcessEventTarget,
        event: EventFrame,
        reservation: OwnedSemaphorePermit,
    ) -> Result<(), SidecarError> {
        self.inner
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "ERR_AGENTOS_COMPLETED_PROCESS_EVENT_POISONED: completed process event store lock poisoned",
                ))
            })?
            .entry(target)
            .or_default()
            .push_back(CompletedProcessEvent {
                event,
                _reservation: reservation,
            });
        self.notify.notify_waiters();
        Ok(())
    }

    async fn reserve(&self) -> Result<OwnedSemaphorePermit, SidecarError> {
        Arc::clone(&self.budget).acquire_owned().await.map_err(|_| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_COMPLETED_PROCESS_EVENT_CLOSED: completed process event store closed",
            ))
        })
    }
}

impl RoutedExtensionServices {
    pub(crate) fn new(
        commands: mpsc::Sender<ExtensionServiceCommand>,
        routed_process_event_notify: Arc<Notify>,
    ) -> Self {
        Self {
            commands,
            routed_process_event_notify,
            process_event_broker: None,
            completed_process_events: CompletedProcessEventStore::new(1),
        }
    }

    pub(crate) fn new_with_process_event_broker(
        commands: mpsc::Sender<ExtensionServiceCommand>,
        routed_process_event_notify: Arc<Notify>,
        process_event_broker: ProcessEventBroker,
    ) -> Self {
        let completed_process_events =
            CompletedProcessEventStore::new(process_event_broker.max_events());
        Self {
            commands,
            routed_process_event_notify,
            process_event_broker: Some(process_event_broker),
            completed_process_events,
        }
    }

    fn call<T, F>(&self, build: F) -> ExtensionFuture<'static, T>
    where
        T: Send + 'static,
        F: FnOnce(Reply<T>) -> ExtensionServiceCommand + Send + 'static,
    {
        let commands = self.commands.clone();
        Box::pin(async move {
            let (reply, response) = oneshot::channel();
            commands.send(build(reply)).await.map_err(|_| {
                SidecarError::Io(String::from(
                    "ERR_AGENTOS_EXTENSION_SERVICE_CLOSED: sidecar coordinator stopped; active extension operation was cancelled",
                ))
            })?;
            response.await.map_err(|_| {
                SidecarError::Io(String::from(
                    "ERR_AGENTOS_EXTENSION_SERVICE_REPLY_CLOSED: sidecar coordinator dropped an extension operation without a result",
                ))
            })?
        })
    }
}

impl ExtensionServices for RoutedExtensionServices {
    fn acp_termination_grace(&self) -> ExtensionFuture<'static, Duration> {
        self.call(move |reply| ExtensionServiceCommand::AcpTerminationGrace { reply })
    }

    fn vm_acp_limits(
        &self,
        ownership: OwnershipScope,
    ) -> ExtensionFuture<'static, agentos_native_sidecar_core::limits::AcpLimits> {
        self.call(move |reply| ExtensionServiceCommand::VmAcpLimits { ownership, reply })
    }

    fn vm_database(
        &self,
        ownership: OwnershipScope,
    ) -> ExtensionFuture<'static, Option<SharedVmSqliteDatabase>> {
        self.call(move |reply| ExtensionServiceCommand::VmDatabase { ownership, reply })
    }

    fn projected_agents(
        &self,
        ownership: OwnershipScope,
    ) -> ExtensionFuture<'static, Vec<ProjectedAgentLaunchEntry>> {
        self.call(move |reply| ExtensionServiceCommand::ProjectedAgents { ownership, reply })
    }

    fn spawn_process(
        &self,
        ownership: OwnershipScope,
        request: ExecuteRequest,
    ) -> ExtensionFuture<'static, ProcessStartedResponse> {
        self.call(move |reply| ExtensionServiceCommand::SpawnProcess {
            ownership,
            request,
            reply,
        })
    }

    fn write_stdin(
        &self,
        ownership: OwnershipScope,
        request: WriteStdinRequest,
    ) -> ExtensionFuture<'static, StdinWrittenResponse> {
        self.call(move |reply| ExtensionServiceCommand::WriteStdin {
            ownership,
            request,
            reply,
        })
    }

    fn close_stdin(
        &self,
        ownership: OwnershipScope,
        request: CloseStdinRequest,
    ) -> ExtensionFuture<'static, StdinClosedResponse> {
        self.call(move |reply| ExtensionServiceCommand::CloseStdin {
            ownership,
            request,
            reply,
        })
    }

    fn kill_process(
        &self,
        ownership: OwnershipScope,
        request: KillProcessRequest,
    ) -> ExtensionFuture<'static, ProcessKilledResponse> {
        self.call(move |reply| ExtensionServiceCommand::KillProcess {
            ownership,
            request,
            reply,
        })
    }

    fn poll_event(
        &self,
        ownership: OwnershipScope,
        timeout: Duration,
    ) -> ExtensionFuture<'static, Option<EventFrame>> {
        let services = self.clone();
        Box::pin(async move {
            let deadline = Instant::now() + timeout;
            loop {
                // Register before the durable zero-time probe so an event edge
                // racing the probe cannot be lost between probe and wait.
                let notified = services.routed_process_event_notify.notified();
                let polled = services
                    .call({
                        let ownership = ownership.clone();
                        move |reply| ExtensionServiceCommand::PollEvent { ownership, reply }
                    })
                    .await?;
                if polled.is_some() || timeout.is_zero() {
                    return Ok(polled);
                }
                let now = Instant::now();
                if now >= deadline {
                    return Ok(None);
                }
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(deadline.saturating_duration_since(now)) => {
                        return Ok(None);
                    }
                }
            }
        })
    }

    fn poll_process_event(
        &self,
        ownership: OwnershipScope,
        process_id: String,
        timeout: Duration,
    ) -> ExtensionFuture<'static, Option<EventFrame>> {
        let services = self.clone();
        Box::pin(async move {
            let broker = services.process_event_broker.clone().ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "ERR_AGENTOS_PROCESS_EVENT_BROKER_UNAVAILABLE: owned extension services were not wired to the process event broker",
                ))
            })?;
            let target = ProcessEventTarget::for_owned_process(&ownership, process_id)
                .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
            // Claim synchronously before the first coordinator command. The
            // stdio event pump consults this broker-owned claim, so it cannot
            // win a select race and consume adapter output already queued
            // between spawn and the first targeted RouteProcessEvent command.
            broker
                .claim_target(&ownership, target.clone())
                .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
            let waiter = broker
                .register_waiter(&ownership, target.clone(), OperationCancellation::new())
                .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
            let deadline = Instant::now() + timeout;
            loop {
                let completed = services.completed_process_events.notify.notified();
                if let Some(event) = services.completed_process_events.take(&target)? {
                    return Ok(Some(event));
                }
                let routed = services
                    .call({
                        let ownership = ownership.clone();
                        let target = target.clone();
                        move |reply| ExtensionServiceCommand::RouteProcessEvent {
                            ownership,
                            target,
                            reply,
                        }
                    })
                    .await?;

                let lease =
                    if routed {
                        if timeout.is_zero() {
                            match tokio::time::timeout(Duration::ZERO, waiter.next_lease()).await {
                                Ok(result) => Some(result.map_err(|error| {
                                    SidecarError::InvalidState(error.to_string())
                                })?),
                                Err(_) => None,
                            }
                        } else {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            match tokio::time::timeout(remaining, waiter.next_lease()).await {
                                Ok(result) => Some(result.map_err(|error| {
                                    SidecarError::InvalidState(error.to_string())
                                })?),
                                Err(_) => None,
                            }
                        }
                    } else if timeout.is_zero() || Instant::now() >= deadline {
                        None
                    } else {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        tokio::select! {
                            result = waiter.next_lease() => Some(result.map_err(|error| {
                                SidecarError::InvalidState(error.to_string())
                            })?),
                            _ = completed => continue,
                            _ = tokio::time::sleep(remaining) => None,
                        }
                    };

                let Some(lease) = lease else {
                    return Ok(None);
                };
                let remaining = deadline.saturating_duration_since(Instant::now());
                let completion_reservation = if timeout.is_zero() {
                    match Arc::clone(&services.completed_process_events.budget).try_acquire_owned()
                    {
                        Ok(reservation) => reservation,
                        Err(_) => return Ok(None),
                    }
                } else {
                    match tokio::time::timeout(
                        remaining,
                        services.completed_process_events.reserve(),
                    )
                    .await
                    {
                        Ok(result) => result?,
                        Err(_) => return Ok(None),
                    }
                };
                let command_target = target.clone();
                let completion_store = services.completed_process_events.clone();
                let event = services
                    .call(move |reply| ExtensionServiceCommand::HandleProcessEvent {
                        target: command_target,
                        lease,
                        completion_store,
                        completion_reservation,
                        reply,
                    })
                    .await?;
                if event.is_some() {
                    return Ok(event);
                }
                if timeout.is_zero() || Instant::now() >= deadline {
                    return Ok(None);
                }
            }
        })
    }

    fn guest_filesystem_call(
        &self,
        ownership: OwnershipScope,
        request: GuestFilesystemCallRequest,
    ) -> ExtensionFuture<'static, GuestFilesystemResultResponse> {
        self.call(move |reply| ExtensionServiceCommand::GuestFilesystemCall {
            ownership,
            request,
            reply,
        })
    }

    fn bind_process_to_session(
        &self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
    ) -> ExtensionFuture<'static, ()> {
        self.call(move |reply| ExtensionServiceCommand::BindProcessToSession {
            ownership,
            namespace,
            ext_session_id,
            process_id,
            reply,
        })
    }

    fn bind_vm_to_session(
        &self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
    ) -> ExtensionFuture<'static, ()> {
        self.call(move |reply| ExtensionServiceCommand::BindVmToSession {
            ownership,
            namespace,
            ext_session_id,
            reply,
        })
    }

    fn dispose_session_resources(
        &self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
    ) -> ExtensionFuture<'static, Vec<EventFrame>> {
        self.call(
            move |reply| ExtensionServiceCommand::DisposeSessionResources {
                ownership,
                namespace,
                ext_session_id,
                reply,
            },
        )
    }

    fn start_buffering_process_output(
        &self,
        ownership: OwnershipScope,
        process_id: String,
    ) -> ExtensionFuture<'static, ()> {
        self.call(
            move |reply| ExtensionServiceCommand::StartBufferingProcessOutput {
                ownership,
                process_id,
                reply,
            },
        )
    }

    fn handoff_buffered_process_output(
        &self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
        timeout: Duration,
    ) -> ExtensionFuture<'static, ExtensionBufferedProcessOutput> {
        let services = self.clone();
        Box::pin(async move {
            let deadline = Instant::now() + timeout;
            loop {
                let notified = services.routed_process_event_notify.notified();
                let finalize_if_empty = timeout.is_zero() || Instant::now() >= deadline;
                let output = services
                    .call({
                        let ownership = ownership.clone();
                        let namespace = namespace.clone();
                        let ext_session_id = ext_session_id.clone();
                        let process_id = process_id.clone();
                        move |reply| ExtensionServiceCommand::ProbeBufferedProcessOutputHandoff {
                            ownership,
                            namespace,
                            ext_session_id,
                            process_id,
                            finalize_if_empty,
                            reply,
                        }
                    })
                    .await?;
                if let Some(output) = output {
                    return Ok(output);
                }
                let now = Instant::now();
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(deadline.saturating_duration_since(now)) => {}
                }
            }
        })
    }
}

#[cfg(test)]
mod internal_event_lifecycle_tests {
    use super::*;
    use crate::ownership_coordinator::OwnershipCoordinatorLimits;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn prepared_internal_event(
        coordinator: &OwnershipCoordinator,
        ownership: &OwnershipScope,
        panic_count: Arc<AtomicUsize>,
        reservation_drop_count: Arc<AtomicUsize>,
    ) -> PreparedExtensionServiceCommand {
        let reservation = DropCounter(reservation_drop_count);
        let prepared = PreparedExtensionServiceCommand {
            operation: "test_claimed_internal_event",
            future: Box::pin(async move {
                drop(reservation);
                Box::new(|_sidecar: &mut NativeSidecar<LocalBridge>| None)
                    as ExtensionServiceCompletionMutation
            }),
            panic_reply: Box::new(move |_error| {
                panic_count.fetch_add(1, Ordering::AcqRel);
            }),
            admission: None,
        };
        with_internal_vm_event_admission(prepared, coordinator, ownership)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deferred_claimed_event_stays_owned_until_disposal_cancels_it_once() {
        let coordinator = OwnershipCoordinator::new(OwnershipCoordinatorLimits {
            max_connections: 1,
            max_sessions_per_connection: 1,
            max_vms_per_session: 1,
            max_operations_per_entity: 1,
            max_internal_event_operations_per_entity: 1,
        });
        let connection = coordinator
            .register_connection("connection-a")
            .expect("register connection");
        let session = connection.open_session("session-a").expect("open session");
        session.open_vm("vm-a").expect("open VM");
        let ownership = OwnershipScope::vm("connection-a", "session-a", "vm-a");

        let active = prepared_internal_event(
            &coordinator,
            &ownership,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        )
        .admit_vm_event_nowait()
        .expect("admit first claimed event");
        let VmEventAdmissionResult::Admitted(active) = active else {
            panic!("first claimed event must fill active capacity");
        };

        let panic_count = Arc::new(AtomicUsize::new(0));
        let reservation_drop_count = Arc::new(AtomicUsize::new(0));
        let deferred = prepared_internal_event(
            &coordinator,
            &ownership,
            Arc::clone(&panic_count),
            Arc::clone(&reservation_drop_count),
        )
        .admit_vm_event_nowait()
        .expect("track second claimed event");
        let VmEventAdmissionResult::Deferred(deferred) = deferred else {
            panic!("second claimed event must defer at the independent bound");
        };
        let deferred_cancellation = deferred
            .admission
            .as_ref()
            .expect("deferred admission metadata")
            .cancellation
            .clone();
        assert_eq!(reservation_drop_count.load(Ordering::Acquire), 0);

        let disposal = coordinator
            .begin_vm_disposal(&ownership, OperationCancellationReason::Explicit)
            .expect("begin VM disposal");
        assert_eq!(
            deferred_cancellation.reason(),
            Some(OperationCancellationReason::Explicit)
        );
        let mut drained = Box::pin(disposal.wait_drained());
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(matches!(drained.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(reservation_drop_count.load(Ordering::Acquire), 0);

        assert!(
            deferred.admit_vm_event_nowait().is_err(),
            "disposal cancellation must reject the exact deferred target"
        );
        assert_eq!(panic_count.load(Ordering::Acquire), 1);
        assert_eq!(
            reservation_drop_count.load(Ordering::Acquire),
            1,
            "the retained source reservation must release exactly once"
        );
        assert!(
            matches!(drained.as_mut().poll(&mut context), Poll::Pending),
            "the first active event still owns disposal"
        );

        drop(active);
        drained.as_mut().await;
        drop(drained);
        disposal.complete().expect("complete drained disposal");
        assert_eq!(panic_count.load(Ordering::Acquire), 1);
        assert_eq!(reservation_drop_count.load(Ordering::Acquire), 1);
    }
}

fn unexpected_service_response(operation: &str, payload: ResponsePayload) -> SidecarError {
    match payload {
        ResponsePayload::Rejected(response) => SidecarError::InvalidState(format!(
            "extension {operation} rejected with {}: {}",
            response.code, response.message
        )),
        other => SidecarError::InvalidState(format!(
            "extension {operation} returned unexpected response: {other:?}"
        )),
    }
}

struct ExtensionVmDisposalState {
    connection_id: String,
    session_id: String,
    remaining_vm_ids: VecDeque<String>,
    events: Vec<EventFrame>,
    reply: SharedReply<Vec<EventFrame>>,
    coordinator: OwnershipCoordinator,
}

fn prepare_next_extension_vm_disposal(
    sidecar: &mut NativeSidecar<LocalBridge>,
    mut state: ExtensionVmDisposalState,
) -> Option<PreparedExtensionServiceCommand> {
    let Some(vm_id) = state.remaining_vm_ids.pop_front() else {
        state
            .reply
            .send(Ok(state.events), "dispose_session_resources");
        return None;
    };
    let ownership = OwnershipScope::vm(
        state.connection_id.clone(),
        state.session_id.clone(),
        vm_id.clone(),
    );
    let plan = match sidecar.prepare_internal_vm_disposal(
        state.connection_id.clone(),
        state.session_id.clone(),
        vm_id,
        DisposeReason::Requested,
    ) {
        Ok(plan) => plan,
        Err(error) => {
            state.reply.send(Err(error), "dispose_session_resources");
            return None;
        }
    };
    let disposal = match state
        .coordinator
        .begin_vm_disposal(&ownership, OperationCancellationReason::Explicit)
    {
        Ok(disposal) => disposal,
        Err(error) => {
            state.reply.send(
                Err(SidecarError::InvalidState(error.to_string())),
                "dispose_session_resources",
            );
            return None;
        }
    };
    let panic_reply = state.reply.clone();
    Some(PreparedExtensionServiceCommand {
        operation: "dispose_session_resources.wait_vm_drain",
        future: Box::pin(async move {
            disposal.wait_drained().await;
            Box::new(move |sidecar: &mut NativeSidecar<LocalBridge>| {
                let prepared = match sidecar.detach_vm_for_disposal(plan) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        state.reply.send(Err(error), "dispose_session_resources");
                        return None;
                    }
                };
                Some(prepare_extension_vm_teardown(prepared, disposal, state))
            }) as ExtensionServiceCompletionMutation
        }),
        panic_reply: Box::new(move |error| {
            panic_reply.send(Err(error), "dispose_session_resources")
        }),
        admission: None,
    })
}

fn prepare_extension_vm_teardown(
    prepared: crate::vm::PreparedDisposeVm<LocalBridge>,
    disposal: VmDisposal,
    mut state: ExtensionVmDisposalState,
) -> PreparedExtensionServiceCommand {
    let panic_reply = state.reply.clone();
    PreparedExtensionServiceCommand {
        operation: "dispose_session_resources.teardown_vm",
        future: Box::pin(async move {
            let completed = prepared.execute().await;
            Box::new(move |sidecar: &mut NativeSidecar<LocalBridge>| {
                let teardown_result = sidecar.complete_owned_vm_disposal(completed);
                let coordinator_result = disposal.complete().map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "ERR_AGENTOS_EXTENSION_VM_COORDINATOR_DISPOSAL: {error}"
                    ))
                });
                match (teardown_result, coordinator_result) {
                    (Ok(events), Ok(())) => state.events.extend(events),
                    (Err(error), Ok(())) | (Ok(_), Err(error)) => {
                        state.reply.send(Err(error), "dispose_session_resources");
                        return None;
                    }
                    (Err(teardown), Err(coordinator)) => {
                        state.reply.send(
                            Err(SidecarError::Execution(format!(
                                "{teardown}; coordinator completion also failed: {coordinator}"
                            ))),
                            "dispose_session_resources",
                        );
                        return None;
                    }
                }
                prepare_next_extension_vm_disposal(sidecar, state)
            }) as ExtensionServiceCompletionMutation
        }),
        panic_reply: Box::new(move |error| {
            panic_reply.send(Err(error), "dispose_session_resources")
        }),
        admission: None,
    }
}

fn prepare_extension_session_disposal(
    coordinator: OwnershipCoordinator,
    ownership: OwnershipScope,
    namespace: String,
    ext_session_id: String,
    reply: Reply<Vec<EventFrame>>,
) -> PreparedExtensionServiceCommand {
    let reply = SharedReply::new(reply);
    let completion_reply = reply.clone();
    let panic_reply = reply.clone();
    PreparedExtensionServiceCommand {
        operation: "dispose_session_resources",
        future: Box::pin(async move {
            Box::new(move |sidecar: &mut NativeSidecar<LocalBridge>| {
                let (connection_id, session_id, _) = match sidecar.vm_scope_for(&ownership) {
                    Ok(scope) => scope,
                    Err(error) => {
                        completion_reply.send(Err(error), "dispose_session_resources");
                        return None;
                    }
                };
                let remaining_vm_ids = match sidecar
                    .detach_extension_session_resources_for_owned_disposal(
                        ownership,
                        namespace,
                        ext_session_id,
                    ) {
                    Ok(vm_ids) => vm_ids.into(),
                    Err(error) => {
                        completion_reply.send(Err(error), "dispose_session_resources");
                        return None;
                    }
                };
                prepare_next_extension_vm_disposal(
                    sidecar,
                    ExtensionVmDisposalState {
                        connection_id,
                        session_id,
                        remaining_vm_ids,
                        events: Vec::new(),
                        reply: completion_reply,
                        coordinator,
                    },
                )
            }) as ExtensionServiceCompletionMutation
        }),
        panic_reply: Box::new(move |error| {
            panic_reply.send(Err(error), "dispose_session_resources")
        }),
        admission: None,
    }
}

/// Validate and detach one extension-service command from the protocol router.
/// No future returned by this function borrows `sidecar`.
pub(crate) fn prepare_extension_service_command(
    sidecar: &mut NativeSidecar<LocalBridge>,
    coordinator: &OwnershipCoordinator,
    command: ExtensionServiceCommand,
) -> PreparedExtensionServiceCommand {
    let admission_ownership = match &command {
        ExtensionServiceCommand::VmAcpLimits { ownership, .. }
        | ExtensionServiceCommand::VmDatabase { ownership, .. }
        | ExtensionServiceCommand::ProjectedAgents { ownership, .. }
        | ExtensionServiceCommand::SpawnProcess { ownership, .. }
        | ExtensionServiceCommand::WriteStdin { ownership, .. }
        | ExtensionServiceCommand::CloseStdin { ownership, .. }
        | ExtensionServiceCommand::KillProcess { ownership, .. }
        | ExtensionServiceCommand::PollEvent { ownership, .. }
        | ExtensionServiceCommand::RouteProcessEvent { ownership, .. }
        | ExtensionServiceCommand::GuestFilesystemCall { ownership, .. }
        | ExtensionServiceCommand::BindProcessToSession { ownership, .. }
        | ExtensionServiceCommand::BindVmToSession { ownership, .. }
        | ExtensionServiceCommand::StartBufferingProcessOutput { ownership, .. }
        | ExtensionServiceCommand::ProbeBufferedProcessOutputHandoff { ownership, .. } => {
            Some(ownership.clone())
        }
        ExtensionServiceCommand::HandleProcessEvent { target, .. } => Some(OwnershipScope::vm(
            target.connection_id.clone(),
            target.session_id.clone(),
            target.vm_id.clone(),
        )),
        ExtensionServiceCommand::AcpTerminationGrace { .. }
        | ExtensionServiceCommand::DisposeSessionResources { .. } => None,
    };
    let prepared = match command {
        ExtensionServiceCommand::AcpTerminationGrace { reply } => {
            let grace = sidecar.config.acp_termination_grace;
            prepared_command(
                "acp_termination_grace",
                reply,
                move || async move { Ok(grace) },
            )
        }
        ExtensionServiceCommand::VmAcpLimits { ownership, reply } => {
            let result = (|| {
                let (connection_id, session_id, vm_id) = sidecar.vm_scope_for(&ownership)?;
                sidecar.require_owned_vm(&connection_id, &session_id, &vm_id)?;
                sidecar
                    .vms
                    .get(&vm_id)
                    .map(|vm| vm.limits.acp.clone())
                    .ok_or_else(|| SidecarError::InvalidState(format!("VM not found: {vm_id}")))
            })();
            prepared_command("vm_acp_limits", reply, move || async move { result })
        }
        ExtensionServiceCommand::VmDatabase { ownership, reply } => {
            let result = (|| {
                let (connection_id, session_id, vm_id) = sidecar.vm_scope_for(&ownership)?;
                sidecar.require_owned_vm(&connection_id, &session_id, &vm_id)?;
                Ok(sidecar.vms.get(&vm_id).and_then(|vm| vm.database.clone()))
            })();
            prepared_command("vm_database", reply, move || async move { result })
        }
        ExtensionServiceCommand::ProjectedAgents { ownership, reply } => {
            let result = (|| {
                let (connection_id, session_id, vm_id) = sidecar.vm_scope_for(&ownership)?;
                sidecar.require_owned_vm(&connection_id, &session_id, &vm_id)?;
                let vm = sidecar
                    .vms
                    .get(&vm_id)
                    .ok_or_else(|| SidecarError::InvalidState(format!("unknown VM {vm_id}")))?;
                Ok(vm
                    .projected_agent_launch
                    .iter()
                    .map(|(id, launch)| ProjectedAgentLaunchEntry {
                        id: id.clone(),
                        acp_entrypoint: launch.acp_entrypoint.clone(),
                        env: launch.env.clone(),
                        launch_args: launch.launch_args.clone(),
                    })
                    .collect())
            })();
            prepared_command("projected_agents", reply, move || async move { result })
        }
        ExtensionServiceCommand::SpawnProcess {
            ownership,
            request: payload,
            reply,
        } => {
            let request = RequestFrame::new(0, ownership, RequestPayload::Execute(payload.clone()));
            let future = sidecar.execute(&request, payload);
            prepared_command("spawn_process", reply, move || async move {
                let dispatch = future.await?;
                match dispatch.response.payload {
                    ResponsePayload::ProcessStarted(response) => Ok(response),
                    other => Err(unexpected_service_response("execute", other)),
                }
            })
        }
        ExtensionServiceCommand::WriteStdin {
            ownership,
            request: payload,
            reply,
        } => {
            let request =
                RequestFrame::new(0, ownership, RequestPayload::WriteStdin(payload.clone()));
            let future = sidecar.write_stdin(&request, payload);
            prepared_command("write_stdin", reply, move || async move {
                let dispatch = future.await?;
                match dispatch.response.payload {
                    ResponsePayload::StdinWritten(response) => Ok(response),
                    other => Err(unexpected_service_response("write_stdin", other)),
                }
            })
        }
        ExtensionServiceCommand::CloseStdin {
            ownership,
            request: payload,
            reply,
        } => {
            let request =
                RequestFrame::new(0, ownership, RequestPayload::CloseStdin(payload.clone()));
            let future = sidecar.close_stdin(&request, payload);
            prepared_command("close_stdin", reply, move || async move {
                let dispatch = future.await?;
                match dispatch.response.payload {
                    ResponsePayload::StdinClosed(response) => Ok(response),
                    other => Err(unexpected_service_response("close_stdin", other)),
                }
            })
        }
        ExtensionServiceCommand::KillProcess {
            ownership,
            request: payload,
            reply,
        } => {
            let request =
                RequestFrame::new(0, ownership, RequestPayload::KillProcess(payload.clone()));
            let future = sidecar.kill_process(&request, payload);
            prepared_command("kill_process", reply, move || async move {
                let dispatch = future.await?;
                match dispatch.response.payload {
                    ResponsePayload::ProcessKilled(response) => Ok(response),
                    other => Err(unexpected_service_response("kill_process", other)),
                }
            })
        }
        ExtensionServiceCommand::PollEvent { ownership, reply } => {
            prepared_mutation("poll_event", reply, move |sidecar| {
                sidecar.poll_event_nowait(&ownership)
            })
        }
        ExtensionServiceCommand::RouteProcessEvent {
            ownership,
            target,
            reply,
        } => prepared_mutation("route_process_event", reply, move |sidecar| {
            sidecar.route_owned_process_event_to_broker_nowait(&ownership, &target)
        }),
        ExtensionServiceCommand::HandleProcessEvent {
            target,
            lease,
            completion_store,
            completion_reservation,
            reply,
        } => {
            let reply = SharedReply::new(reply);
            let completion_reply = reply.clone();
            let panic_reply = reply.clone();
            PreparedExtensionServiceCommand {
                operation: "handle_process_event",
                future: Box::pin(async move {
                    let envelope = lease
                        .commit()
                        .map_err(|error| SidecarError::InvalidState(error.to_string()));
                    Box::new(move |sidecar: &mut NativeSidecar<LocalBridge>| {
                        let result = envelope.and_then(|envelope| {
                            sidecar.handle_public_process_event_envelope_nowait(envelope)
                        });
                        match completion_reply.try_send(result, "handle_process_event") {
                            Ok(()) => {}
                            Err(Ok(Some(event))) => {
                                if let Err(error) =
                                    completion_store.retain(target, event, completion_reservation)
                                {
                                    eprintln!(
                                        "ERR_AGENTOS_COMPLETED_PROCESS_EVENT_RETAIN: {error}"
                                    );
                                }
                            }
                            Err(other) => {
                                drop(completion_reservation);
                                tracing::debug!(
                                    operation = "handle_process_event",
                                    result = ?other,
                                    "extension service caller ended before its process event reply"
                                );
                            }
                        }
                        None
                    }) as ExtensionServiceCompletionMutation
                }),
                panic_reply: Box::new(move |error| {
                    panic_reply.send(Err(error), "handle_process_event")
                }),
                admission: None,
            }
        }
        ExtensionServiceCommand::GuestFilesystemCall {
            ownership,
            request: payload,
            reply,
        } => {
            let prepared: Result<_, SidecarError> = (|| {
                let (connection_id, session_id, vm_id) = sidecar.vm_scope_for(&ownership)?;
                sidecar.require_owned_vm(&connection_id, &session_id, &vm_id)?;
                let handle = sidecar.vms.handle(&vm_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "VM {vm_id} no longer exists for guest filesystem call"
                    ))
                })?;
                Ok((handle, payload))
            })();
            prepared_command("guest_filesystem_call", reply, move || async move {
                let (handle, payload) = prepared?;
                handle.try_command("extension guest filesystem call", |vm| {
                    guest_filesystem_call_vm(vm, &payload)
                })
            })
        }
        ExtensionServiceCommand::BindProcessToSession {
            ownership,
            namespace,
            ext_session_id,
            process_id,
            reply,
        } => prepared_mutation("bind_process_to_session", reply, move |sidecar| {
            sidecar.bind_extension_process_resource(
                ownership,
                namespace,
                ext_session_id,
                process_id,
            )
        }),
        ExtensionServiceCommand::BindVmToSession {
            ownership,
            namespace,
            ext_session_id,
            reply,
        } => prepared_mutation("bind_vm_to_session", reply, move |sidecar| {
            sidecar.bind_extension_vm_resource(ownership, namespace, ext_session_id)
        }),
        ExtensionServiceCommand::DisposeSessionResources {
            ownership,
            namespace,
            ext_session_id,
            reply,
        } => prepare_extension_session_disposal(
            coordinator.clone(),
            ownership,
            namespace,
            ext_session_id,
            reply,
        ),
        ExtensionServiceCommand::StartBufferingProcessOutput {
            ownership,
            process_id,
            reply,
        } => prepared_mutation("start_buffering_process_output", reply, move |sidecar| {
            sidecar.start_buffering_process_output_nowait(ownership, process_id)
        }),
        ExtensionServiceCommand::ProbeBufferedProcessOutputHandoff {
            ownership,
            namespace,
            ext_session_id,
            process_id,
            finalize_if_empty,
            reply,
        } => prepared_mutation(
            "probe_buffered_process_output_handoff",
            reply,
            move |sidecar| {
                sidecar.probe_extension_process_output_handoff_nowait(
                    ownership,
                    namespace,
                    ext_session_id,
                    process_id,
                    finalize_if_empty,
                )
            },
        ),
    };
    match admission_ownership {
        Some(ownership) => with_vm_admission(prepared, coordinator, &ownership),
        None => prepared,
    }
}
