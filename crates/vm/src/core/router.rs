use crate::core::frames::{reject, DispatchResult};
use agentos_sidecar_protocol::protocol::{
    AuthenticateRequest, BootstrapRootFilesystemRequest, CloseStdinRequest, ConfigureVmRequest,
    CreateLayerRequest, CreateOverlayRequest, CreateVmRequest, DisposeVmRequest, ExecuteRequest,
    ExportSnapshotRequest, ExtEnvelope, FindBoundUdpRequest, FindListenerRequest,
    GetProcessSnapshotRequest, GetResourceSnapshotRequest, GetSignalStateRequest,
    GetZombieTimerCountRequest, GuestFilesystemCallRequest, GuestKernelCallRequest,
    ImportSnapshotRequest, KillProcessRequest, LinkPackageRequest, ListMountsRequest,
    OpenSessionRequest, OwnershipScope, ProvidedCommandsRequest, RegisterHostCallbacksRequest,
    RequestFrame, RequestPayload, ResizePtyRequest, SealLayerRequest,
    SnapshotRootFilesystemRequest, VmFetchRequest, WriteStdinRequest,
};
use agentos_sidecar_protocol::wire as generated_wire;

pub const UNSUPPORTED_HOST_CALLBACK_DIRECTION_CODE: &str = "unsupported_direction";
pub const UNSUPPORTED_HOST_CALLBACK_DIRECTION_MESSAGE: &str =
    "host callback request categories are sidecar-to-host only in this scaffold";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDispatchMode {
    Immediate,
    Async,
}

// Request payload variants intentionally vary widely in size (small acks next
// to bulky create/exec payloads); boxing is a wire-adjacent refactor.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum RequestRoute {
    Authenticate(AuthenticateRequest),
    OpenSession(OpenSessionRequest),
    CreateVm(CreateVmRequest),
    DisposeVm(DisposeVmRequest),
    BootstrapRootFilesystem(BootstrapRootFilesystemRequest),
    ConfigureVm(ConfigureVmRequest),
    RegisterHostCallbacks(RegisterHostCallbacksRequest),
    CreateLayer(CreateLayerRequest),
    SealLayer(SealLayerRequest),
    ImportSnapshot(ImportSnapshotRequest),
    ExportSnapshot(ExportSnapshotRequest),
    CreateOverlay(CreateOverlayRequest),
    GuestFilesystemCall(GuestFilesystemCallRequest),
    GuestKernelCall(GuestKernelCallRequest),
    SnapshotRootFilesystem(SnapshotRootFilesystemRequest),
    ListMounts(ListMountsRequest),
    Execute(ExecuteRequest),
    WriteStdin(WriteStdinRequest),
    ResizePty(ResizePtyRequest),
    CloseStdin(CloseStdinRequest),
    KillProcess(KillProcessRequest),
    GetProcessSnapshot(GetProcessSnapshotRequest),
    GetResourceSnapshot(GetResourceSnapshotRequest),
    FindListener(FindListenerRequest),
    FindBoundUdp(FindBoundUdpRequest),
    VmFetch(VmFetchRequest),
    GetSignalState(GetSignalStateRequest),
    GetZombieTimerCount(GetZombieTimerCountRequest),
    LinkPackage(LinkPackageRequest),
    ProvidedCommands(ProvidedCommandsRequest),
    ExecutionOperation(RequestPayload),
    ExecutionLifecycle(RequestPayload),
    Ext(ExtEnvelope),
    UnsupportedHostCallbackDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockingExtensionInterrupt<'a> {
    ExtensionPayload(&'a [u8]),
    KillProcess,
}

pub fn route_request_payload(request: &RequestFrame) -> RequestRoute {
    match request.payload.clone() {
        RequestPayload::Authenticate(payload) => RequestRoute::Authenticate(payload),
        RequestPayload::OpenSession(payload) => RequestRoute::OpenSession(payload),
        RequestPayload::CreateVm(payload) => RequestRoute::CreateVm(payload),
        RequestPayload::DisposeVm(payload) => RequestRoute::DisposeVm(payload),
        RequestPayload::BootstrapRootFilesystem(payload) => {
            RequestRoute::BootstrapRootFilesystem(payload)
        }
        RequestPayload::ConfigureVm(payload) => RequestRoute::ConfigureVm(payload),
        RequestPayload::RegisterHostCallbacks(payload) => {
            RequestRoute::RegisterHostCallbacks(payload)
        }
        RequestPayload::CreateLayer(payload) => RequestRoute::CreateLayer(payload),
        RequestPayload::SealLayer(payload) => RequestRoute::SealLayer(payload),
        RequestPayload::ImportSnapshot(payload) => RequestRoute::ImportSnapshot(payload),
        RequestPayload::ExportSnapshot(payload) => RequestRoute::ExportSnapshot(payload),
        RequestPayload::CreateOverlay(payload) => RequestRoute::CreateOverlay(payload),
        RequestPayload::GuestFilesystemCall(payload) => RequestRoute::GuestFilesystemCall(payload),
        RequestPayload::GuestKernelCall(payload) => RequestRoute::GuestKernelCall(payload),
        RequestPayload::SnapshotRootFilesystem(payload) => {
            RequestRoute::SnapshotRootFilesystem(payload)
        }
        RequestPayload::ListMounts(payload) => RequestRoute::ListMounts(payload),
        RequestPayload::Execute(payload) => RequestRoute::Execute(payload),
        RequestPayload::WriteStdin(payload) => RequestRoute::WriteStdin(payload),
        RequestPayload::ResizePty(payload) => RequestRoute::ResizePty(payload),
        RequestPayload::CloseStdin(payload) => RequestRoute::CloseStdin(payload),
        RequestPayload::KillProcess(payload) => RequestRoute::KillProcess(payload),
        RequestPayload::GetProcessSnapshot(payload) => RequestRoute::GetProcessSnapshot(payload),
        RequestPayload::GetResourceSnapshot(payload) => RequestRoute::GetResourceSnapshot(payload),
        RequestPayload::FindListener(payload) => RequestRoute::FindListener(payload),
        RequestPayload::FindBoundUdp(payload) => RequestRoute::FindBoundUdp(payload),
        RequestPayload::VmFetch(payload) => RequestRoute::VmFetch(payload),
        RequestPayload::GetSignalState(payload) => RequestRoute::GetSignalState(payload),
        RequestPayload::GetZombieTimerCount(payload) => RequestRoute::GetZombieTimerCount(payload),
        RequestPayload::LinkPackage(payload) => RequestRoute::LinkPackage(payload),
        RequestPayload::ProvidedCommands(payload) => RequestRoute::ProvidedCommands(payload),
        payload @ (RequestPayload::ShellExecution(_)
        | RequestPayload::ArgvExecution(_)
        | RequestPayload::JavaScriptExecution(_)
        | RequestPayload::JavaScriptEvaluation(_)
        | RequestPayload::JavaScriptFileExecution(_)
        | RequestPayload::TypeScriptExecution(_)
        | RequestPayload::TypeScriptEvaluation(_)
        | RequestPayload::TypeScriptFileExecution(_)
        | RequestPayload::TypeScriptCheck(_)
        | RequestPayload::TypeScriptProjectCheck(_)
        | RequestPayload::NpmProjectInstall(_)
        | RequestPayload::NpmPackageInstall(_)
        | RequestPayload::NpmScriptExecution(_)
        | RequestPayload::NpmPackageExecution(_)
        | RequestPayload::PythonExecution(_)
        | RequestPayload::PythonEvaluation(_)
        | RequestPayload::PythonFileExecution(_)
        | RequestPayload::PythonModuleExecution(_)
        | RequestPayload::PythonInstall(_)) => RequestRoute::ExecutionOperation(payload),
        payload @ (RequestPayload::CreateContext(_)
        | RequestPayload::GetExecution(_)
        | RequestPayload::ListExecutions(_)
        | RequestPayload::WaitExecution(_)
        | RequestPayload::CancelExecution(_)
        | RequestPayload::SignalExecution(_)
        | RequestPayload::ResetExecution(_)
        | RequestPayload::DeleteExecution(_)
        | RequestPayload::WriteExecutionStdin(_)
        | RequestPayload::CloseExecutionStdin(_)
        | RequestPayload::ResizeExecutionPty(_)
        | RequestPayload::ReadExecutionOutput(_)) => RequestRoute::ExecutionLifecycle(payload),
        RequestPayload::HostFilesystemCall(_)
        | RequestPayload::PersistenceLoad(_)
        | RequestPayload::PersistenceFlush(_) => RequestRoute::UnsupportedHostCallbackDirection,
        RequestPayload::Ext(payload) => RequestRoute::Ext(payload),
    }
}

pub fn generated_wire_blocking_extension_interrupt<'a>(
    active_request: &generated_wire::RequestFrame,
    blocking_namespace: &str,
    interrupting_request: &'a generated_wire::RequestFrame,
) -> Option<BlockingExtensionInterrupt<'a>> {
    if interrupting_request.ownership != active_request.ownership {
        return None;
    }

    match &interrupting_request.payload {
        generated_wire::RequestPayload::ExtEnvelope(envelope)
            if envelope.namespace == blocking_namespace =>
        {
            Some(BlockingExtensionInterrupt::ExtensionPayload(
                &envelope.payload,
            ))
        }
        generated_wire::RequestPayload::ExtEnvelope(_) => None,
        generated_wire::RequestPayload::KillProcessRequest(_) => {
            Some(BlockingExtensionInterrupt::KillProcess)
        }
        _ => None,
    }
}

pub fn request_dispatch_mode(request: &RequestFrame) -> RequestDispatchMode {
    match request.payload {
        RequestPayload::DisposeVm(_) | RequestPayload::Ext(_) => RequestDispatchMode::Async,
        RequestPayload::Authenticate(_)
        | RequestPayload::OpenSession(_)
        | RequestPayload::CreateVm(_)
        | RequestPayload::BootstrapRootFilesystem(_)
        | RequestPayload::ConfigureVm(_)
        | RequestPayload::RegisterHostCallbacks(_)
        | RequestPayload::CreateLayer(_)
        | RequestPayload::SealLayer(_)
        | RequestPayload::ImportSnapshot(_)
        | RequestPayload::ExportSnapshot(_)
        | RequestPayload::CreateOverlay(_)
        | RequestPayload::GuestFilesystemCall(_)
        | RequestPayload::GuestKernelCall(_)
        | RequestPayload::SnapshotRootFilesystem(_)
        | RequestPayload::ListMounts(_)
        | RequestPayload::Execute(_)
        | RequestPayload::WriteStdin(_)
        | RequestPayload::ResizePty(_)
        | RequestPayload::CloseStdin(_)
        | RequestPayload::KillProcess(_)
        | RequestPayload::GetProcessSnapshot(_)
        | RequestPayload::GetResourceSnapshot(_)
        | RequestPayload::FindListener(_)
        | RequestPayload::FindBoundUdp(_)
        | RequestPayload::VmFetch(_)
        | RequestPayload::GetSignalState(_)
        | RequestPayload::GetZombieTimerCount(_)
        | RequestPayload::LinkPackage(_)
        | RequestPayload::ProvidedCommands(_)
        | RequestPayload::ShellExecution(_)
        | RequestPayload::ArgvExecution(_)
        | RequestPayload::JavaScriptExecution(_)
        | RequestPayload::JavaScriptEvaluation(_)
        | RequestPayload::JavaScriptFileExecution(_)
        | RequestPayload::TypeScriptExecution(_)
        | RequestPayload::TypeScriptEvaluation(_)
        | RequestPayload::TypeScriptFileExecution(_)
        | RequestPayload::TypeScriptCheck(_)
        | RequestPayload::TypeScriptProjectCheck(_)
        | RequestPayload::NpmProjectInstall(_)
        | RequestPayload::NpmPackageInstall(_)
        | RequestPayload::NpmScriptExecution(_)
        | RequestPayload::NpmPackageExecution(_)
        | RequestPayload::PythonExecution(_)
        | RequestPayload::PythonEvaluation(_)
        | RequestPayload::PythonFileExecution(_)
        | RequestPayload::PythonModuleExecution(_)
        | RequestPayload::PythonInstall(_)
        | RequestPayload::CreateContext(_)
        | RequestPayload::GetExecution(_)
        | RequestPayload::ListExecutions(_)
        | RequestPayload::WaitExecution(_)
        | RequestPayload::CancelExecution(_)
        | RequestPayload::SignalExecution(_)
        | RequestPayload::ResetExecution(_)
        | RequestPayload::DeleteExecution(_)
        | RequestPayload::WriteExecutionStdin(_)
        | RequestPayload::CloseExecutionStdin(_)
        | RequestPayload::ResizeExecutionPty(_)
        | RequestPayload::ReadExecutionOutput(_)
        | RequestPayload::HostFilesystemCall(_)
        | RequestPayload::PersistenceLoad(_)
        | RequestPayload::PersistenceFlush(_) => RequestDispatchMode::Immediate,
    }
}

pub fn request_is_unsupported_host_callback_direction(request: &RequestFrame) -> bool {
    matches!(
        request.payload,
        RequestPayload::HostFilesystemCall(_)
            | RequestPayload::PersistenceLoad(_)
            | RequestPayload::PersistenceFlush(_)
    )
}

pub fn unsupported_host_callback_direction_dispatch(request: &RequestFrame) -> DispatchResult {
    debug_assert!(request_is_unsupported_host_callback_direction(request));
    DispatchResult {
        response: reject(
            request,
            UNSUPPORTED_HOST_CALLBACK_DIRECTION_CODE,
            UNSUPPORTED_HOST_CALLBACK_DIRECTION_MESSAGE,
        ),
        events: Vec::new(),
    }
}

pub fn connection_id_of(ownership: &OwnershipScope) -> Option<String> {
    match ownership {
        OwnershipScope::ConnectionOwnership(ownership) => Some(ownership.connection_id.clone()),
        OwnershipScope::SessionOwnership(ownership) => Some(ownership.connection_id.clone()),
        OwnershipScope::VmOwnership(ownership) => Some(ownership.connection_id.clone()),
    }
}

pub fn session_scope_of(ownership: &OwnershipScope) -> Option<(String, String)> {
    match ownership {
        OwnershipScope::SessionOwnership(ownership) => Some((
            ownership.connection_id.clone(),
            ownership.session_id.clone(),
        )),
        OwnershipScope::VmOwnership(ownership) => Some((
            ownership.connection_id.clone(),
            ownership.session_id.clone(),
        )),
        OwnershipScope::ConnectionOwnership(_) => None,
    }
}

pub fn vm_id_of(ownership: &OwnershipScope) -> Option<String> {
    match ownership {
        OwnershipScope::VmOwnership(ownership) => Some(ownership.vm_id.clone()),
        OwnershipScope::ConnectionOwnership(_) | OwnershipScope::SessionOwnership(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_sidecar_protocol::protocol::{
        AuthenticateRequest, ExtEnvelope, FilesystemOperation, HostFilesystemCallRequest,
        OwnershipScope, PersistenceFlushRequest, PersistenceLoadRequest, ResponsePayload,
        PROTOCOL_VERSION,
    };
    use agentos_sidecar_protocol::wire as generated_wire;

    fn request(payload: RequestPayload) -> RequestFrame {
        RequestFrame::new(7, OwnershipScope::connection("conn"), payload)
    }

    fn generated_request(
        request_id: i64,
        ownership: generated_wire::OwnershipScope,
        payload: generated_wire::RequestPayload,
    ) -> generated_wire::RequestFrame {
        generated_wire::RequestFrame {
            schema: generated_wire::protocol_schema(),
            request_id,
            ownership,
            payload,
        }
    }

    fn reverse_host_callback_payloads() -> Vec<RequestPayload> {
        vec![
            RequestPayload::HostFilesystemCall(HostFilesystemCallRequest {
                operation: FilesystemOperation::Read,
                path: String::from("/state"),
                payload_size_bytes: 0,
            }),
            RequestPayload::PersistenceLoad(PersistenceLoadRequest {
                key: String::from("state"),
            }),
            RequestPayload::PersistenceFlush(PersistenceFlushRequest {
                key: String::from("state"),
                payload_size_bytes: 0,
            }),
        ]
    }

    #[test]
    fn dispose_and_ext_requests_are_async() {
        let ext = request(RequestPayload::Ext(ExtEnvelope {
            namespace: String::from("test"),
            payload: Vec::new(),
        }));
        assert_eq!(request_dispatch_mode(&ext), RequestDispatchMode::Async);
    }

    #[test]
    fn normal_requests_are_immediate() {
        let authenticate = request(RequestPayload::Authenticate(AuthenticateRequest {
            client_name: String::from("test"),
            auth_token: String::from("token"),
            protocol_version: PROTOCOL_VERSION,
            bridge_version: 1,
        }));
        assert_eq!(
            request_dispatch_mode(&authenticate),
            RequestDispatchMode::Immediate
        );
    }

    #[test]
    fn host_callback_requests_are_identified_as_reverse_direction_only() {
        for payload in reverse_host_callback_payloads() {
            let host_call = request(payload);

            assert!(request_is_unsupported_host_callback_direction(&host_call));
            assert_eq!(
                request_dispatch_mode(&host_call),
                RequestDispatchMode::Immediate
            );
        }
    }

    #[test]
    fn routes_protocol_payloads_through_shared_enum() {
        let authenticate = request(RequestPayload::Authenticate(AuthenticateRequest {
            client_name: String::from("test"),
            auth_token: String::from("token"),
            protocol_version: PROTOCOL_VERSION,
            bridge_version: 1,
        }));
        assert!(matches!(
            route_request_payload(&authenticate),
            RequestRoute::Authenticate(_)
        ));

        let extension = request(RequestPayload::Ext(ExtEnvelope {
            namespace: String::from("test"),
            payload: vec![1, 2, 3],
        }));
        assert!(matches!(
            route_request_payload(&extension),
            RequestRoute::Ext(_)
        ));

        for payload in reverse_host_callback_payloads() {
            let host_call = request(payload);
            assert!(matches!(
                route_request_payload(&host_call),
                RequestRoute::UnsupportedHostCallbackDirection
            ));
        }
    }

    #[test]
    fn unsupported_host_callback_dispatch_rejects_with_shared_code() {
        for payload in reverse_host_callback_payloads() {
            let host_call = request(payload);

            let dispatch = unsupported_host_callback_direction_dispatch(&host_call);

            assert!(dispatch.events.is_empty());
            assert_eq!(dispatch.response.request_id, host_call.request_id);
            assert_eq!(dispatch.response.ownership, host_call.ownership);
            match dispatch.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, UNSUPPORTED_HOST_CALLBACK_DIRECTION_CODE);
                    assert_eq!(
                        rejected.message,
                        UNSUPPORTED_HOST_CALLBACK_DIRECTION_MESSAGE
                    );
                }
                other => panic!("unexpected response payload: {other:?}"),
            }
        }
    }

    #[test]
    fn generated_wire_prompt_interrupt_classifier_matches_only_same_scope_interrupts() {
        let ownership = generated_wire::OwnershipScope::VmOwnership(generated_wire::VmOwnership {
            connection_id: String::from("conn"),
            session_id: String::from("session"),
            vm_id: String::from("vm"),
        });
        let active = generated_request(
            1,
            ownership.clone(),
            generated_wire::RequestPayload::ExtEnvelope(generated_wire::ExtEnvelope {
                namespace: String::from("prompt"),
                payload: b"active".to_vec(),
            }),
        );

        let same_namespace = generated_request(
            2,
            ownership.clone(),
            generated_wire::RequestPayload::ExtEnvelope(generated_wire::ExtEnvelope {
                namespace: String::from("prompt"),
                payload: b"cancel".to_vec(),
            }),
        );
        assert_eq!(
            generated_wire_blocking_extension_interrupt(&active, "prompt", &same_namespace),
            Some(BlockingExtensionInterrupt::ExtensionPayload(b"cancel"))
        );

        let kill = generated_request(
            3,
            ownership.clone(),
            generated_wire::RequestPayload::KillProcessRequest(
                generated_wire::KillProcessRequest {
                    process_id: String::from("proc"),
                    signal: String::from("SIGTERM"),
                },
            ),
        );
        assert_eq!(
            generated_wire_blocking_extension_interrupt(&active, "prompt", &kill),
            Some(BlockingExtensionInterrupt::KillProcess)
        );

        let other_namespace = generated_request(
            4,
            ownership.clone(),
            generated_wire::RequestPayload::ExtEnvelope(generated_wire::ExtEnvelope {
                namespace: String::from("other"),
                payload: b"cancel".to_vec(),
            }),
        );
        assert_eq!(
            generated_wire_blocking_extension_interrupt(&active, "prompt", &other_namespace),
            None
        );

        let other_scope = generated_request(
            5,
            generated_wire::OwnershipScope::VmOwnership(generated_wire::VmOwnership {
                connection_id: String::from("conn"),
                session_id: String::from("session"),
                vm_id: String::from("other-vm"),
            }),
            generated_wire::RequestPayload::KillProcessRequest(
                generated_wire::KillProcessRequest {
                    process_id: String::from("proc"),
                    signal: String::from("SIGTERM"),
                },
            ),
        );
        assert_eq!(
            generated_wire_blocking_extension_interrupt(&active, "prompt", &other_scope),
            None
        );
    }

    #[test]
    fn ownership_scope_helpers_extract_shared_ids() {
        let connection = OwnershipScope::connection("conn-1");
        let session = OwnershipScope::session("conn-1", "session-1");
        let vm = OwnershipScope::vm("conn-1", "session-1", "vm-1");

        assert_eq!(connection_id_of(&connection).as_deref(), Some("conn-1"));
        assert_eq!(connection_id_of(&session).as_deref(), Some("conn-1"));
        assert_eq!(connection_id_of(&vm).as_deref(), Some("conn-1"));
        assert_eq!(
            session_scope_of(&session),
            Some((String::from("conn-1"), String::from("session-1")))
        );
        assert_eq!(
            session_scope_of(&vm),
            Some((String::from("conn-1"), String::from("session-1")))
        );
        assert_eq!(session_scope_of(&connection), None);
        assert_eq!(vm_id_of(&vm).as_deref(), Some("vm-1"));
        assert_eq!(vm_id_of(&session), None);
    }
}
