use agentos_sidecar_protocol::protocol::{
    AgentosProjectedAgent, AuthenticateRequest, AuthenticatedResponse, BoundUdpSnapshotResponse,
    EventFrame, EventPayload, LayerCreatedResponse, LayerSealedResponse, ListMountsResponse,
    ListenerSnapshotResponse, MountInfo, OverlayCreatedResponse, OwnershipScope, PackageCommands,
    PackageLinkedResponse, ProcessExitedEvent, ProcessKilledResponse, ProcessOutputEvent,
    ProcessSnapshotEntry, ProcessSnapshotResponse, ProcessStartedResponse, ProjectedCommand,
    ProtocolSchema, ProvidedCommandsResponse, RejectedResponse, RequestFrame, RequestId,
    ResponseFrame, ResponsePayload, RootFilesystemBootstrappedResponse, RootFilesystemEntry,
    RootFilesystemSnapshotResponse, SessionOpenedResponse, SignalHandlerRegistration,
    SignalStateResponse, SnapshotExportedResponse, SnapshotImportedResponse, SocketStateEntry,
    StdinClosedResponse, StdinWrittenResponse, StreamChannel, StructuredEvent,
    VmConfiguredResponse, VmCreatedResponse, VmDisposedResponse, VmLifecycleEvent,
    VmLifecycleState, ZombieTimerCountResponse, PROTOCOL_VERSION,
};
use std::collections::HashMap;

pub const UNSUPPORTED_GUEST_KERNEL_CALL_EVENT: &str = "guest.kernel_call.unsupported";

#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub response: ResponseFrame,
    pub events: Vec<EventFrame>,
}

pub fn response_with_ownership(
    request_id: RequestId,
    ownership: OwnershipScope,
    payload: ResponsePayload,
) -> ResponseFrame {
    ResponseFrame {
        schema: ProtocolSchema::current(),
        request_id,
        ownership,
        payload,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticateVersionError {
    ProtocolVersionMismatch(String),
    BridgeVersionMismatch(String),
}

impl AuthenticateVersionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProtocolVersionMismatch(_) => "protocol_version_mismatch",
            Self::BridgeVersionMismatch(_) => "bridge_version_mismatch",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::ProtocolVersionMismatch(message) | Self::BridgeVersionMismatch(message) => {
                message
            }
        }
    }
}

pub fn validate_authenticate_versions(
    payload: &AuthenticateRequest,
) -> Result<(), AuthenticateVersionError> {
    if payload.protocol_version != PROTOCOL_VERSION {
        return Err(AuthenticateVersionError::ProtocolVersionMismatch(format!(
            "sidecar protocol version mismatch: expected {}, got {}",
            PROTOCOL_VERSION, payload.protocol_version
        )));
    }

    let expected_bridge_version = agentos_bridge::bridge_contract().version;
    if payload.bridge_version != expected_bridge_version {
        return Err(AuthenticateVersionError::BridgeVersionMismatch(format!(
            "bridge contract version mismatch: expected {expected_bridge_version}, got {}",
            payload.bridge_version
        )));
    }

    Ok(())
}

pub fn authenticated_response(
    request_id: RequestId,
    sidecar_id: impl Into<String>,
    connection_id: String,
    max_frame_bytes: u32,
) -> ResponseFrame {
    response_with_ownership(
        request_id,
        OwnershipScope::connection(&connection_id),
        ResponsePayload::Authenticated(AuthenticatedResponse {
            sidecar_id: sidecar_id.into(),
            connection_id,
            max_frame_bytes,
        }),
    )
}

pub fn session_opened_response(
    request_id: RequestId,
    owner_connection_id: String,
    session_id: String,
) -> ResponseFrame {
    response_with_ownership(
        request_id,
        OwnershipScope::session(&owner_connection_id, &session_id),
        ResponsePayload::SessionOpened(SessionOpenedResponse {
            session_id,
            owner_connection_id,
        }),
    )
}

pub fn respond(request: &RequestFrame, payload: ResponsePayload) -> ResponseFrame {
    response_with_ownership(request.request_id, request.ownership.clone(), payload)
}

pub fn reject(request: &RequestFrame, code: &str, message: &str) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::Rejected(RejectedResponse {
            code: code.to_owned(),
            message: message.to_owned(),
            limit_name: None,
            configured_limit: None,
            current_usage: None,
            requested: None,
            unit: None,
            scope: None,
            vm_id: None,
            session_generation: None,
            capability_id: None,
            operation: None,
            configuration_path: None,
            retryable: None,
            errno: None,
        }),
    )
}

pub fn vm_created_response(request: &RequestFrame, vm_id: String) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::VmCreated(VmCreatedResponse { vm_id }),
    )
}

pub fn vm_disposed_response(request: &RequestFrame, vm_id: String) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::VmDisposed(VmDisposedResponse { vm_id }),
    )
}

pub fn root_filesystem_bootstrapped_response(
    request: &RequestFrame,
    entry_count: u32,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::RootFilesystemBootstrapped(RootFilesystemBootstrappedResponse {
            entry_count,
        }),
    )
}

pub fn vm_configured_response(
    request: &RequestFrame,
    applied_mounts: u32,
    applied_software: u32,
    projected_commands: Vec<ProjectedCommand>,
    agents: Vec<AgentosProjectedAgent>,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::VmConfigured(VmConfiguredResponse {
            applied_mounts,
            applied_software,
            projected_commands,
            agents,
        }),
    )
}

pub fn package_linked_response(
    request: &RequestFrame,
    projected_commands: Vec<ProjectedCommand>,
    agents: Vec<AgentosProjectedAgent>,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::PackageLinked(PackageLinkedResponse {
            projected_commands,
            agents,
        }),
    )
}

pub fn provided_commands_response(
    request: &RequestFrame,
    packages: Vec<PackageCommands>,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::ProvidedCommands(ProvidedCommandsResponse { packages }),
    )
}

pub fn layer_created_response(request: &RequestFrame, layer_id: String) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::LayerCreated(LayerCreatedResponse { layer_id }),
    )
}

pub fn layer_sealed_response(request: &RequestFrame, layer_id: String) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::LayerSealed(LayerSealedResponse { layer_id }),
    )
}

pub fn snapshot_imported_response(request: &RequestFrame, layer_id: String) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::SnapshotImported(SnapshotImportedResponse { layer_id }),
    )
}

pub fn snapshot_exported_response(
    request: &RequestFrame,
    layer_id: String,
    entries: Vec<RootFilesystemEntry>,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::SnapshotExported(SnapshotExportedResponse { layer_id, entries }),
    )
}

pub fn overlay_created_response(request: &RequestFrame, layer_id: String) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::OverlayCreated(OverlayCreatedResponse { layer_id }),
    )
}

pub fn root_filesystem_snapshot_response(
    request: &RequestFrame,
    entries: Vec<RootFilesystemEntry>,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::RootFilesystemSnapshot(RootFilesystemSnapshotResponse { entries }),
    )
}

pub fn mounts_listed_response(request: &RequestFrame, mounts: Vec<MountInfo>) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::MountsListed(ListMountsResponse { mounts }),
    )
}

pub fn process_started_response(
    request: &RequestFrame,
    process_id: String,
    pid: Option<u32>,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::ProcessStarted(ProcessStartedResponse { process_id, pid }),
    )
}

pub fn stdin_written_response(
    request: &RequestFrame,
    process_id: String,
    accepted_bytes: u64,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::StdinWritten(StdinWrittenResponse {
            process_id,
            accepted_bytes,
        }),
    )
}

pub fn stdin_closed_response(request: &RequestFrame, process_id: String) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::StdinClosed(StdinClosedResponse { process_id }),
    )
}

pub fn process_killed_response(request: &RequestFrame, process_id: String) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::ProcessKilled(ProcessKilledResponse { process_id }),
    )
}

pub fn process_snapshot_response(
    request: &RequestFrame,
    processes: Vec<ProcessSnapshotEntry>,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::ProcessSnapshot(ProcessSnapshotResponse { processes }),
    )
}

pub fn listener_snapshot_response(
    request: &RequestFrame,
    listener: Option<SocketStateEntry>,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::ListenerSnapshot(ListenerSnapshotResponse { listener }),
    )
}

pub fn bound_udp_snapshot_response(
    request: &RequestFrame,
    socket: Option<SocketStateEntry>,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::BoundUdpSnapshot(BoundUdpSnapshotResponse { socket }),
    )
}

pub fn signal_state_response(
    request: &RequestFrame,
    process_id: String,
    handlers: impl IntoIterator<Item = (u32, SignalHandlerRegistration)>,
) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::SignalState(SignalStateResponse {
            process_id,
            handlers: handlers.into_iter().collect(),
        }),
    )
}

pub fn zombie_timer_count_response(request: &RequestFrame, count: u64) -> ResponseFrame {
    respond(
        request,
        ResponsePayload::ZombieTimerCount(ZombieTimerCountResponse { count }),
    )
}

pub fn event(ownership: OwnershipScope, payload: EventPayload) -> EventFrame {
    EventFrame::new(ownership, payload)
}

pub fn vm_lifecycle_event(
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    state: VmLifecycleState,
) -> EventFrame {
    event(
        OwnershipScope::vm(connection_id, session_id, vm_id),
        EventPayload::VmLifecycle(VmLifecycleEvent { state }),
    )
}

pub fn process_output_event(
    ownership: OwnershipScope,
    process_id: &str,
    channel: StreamChannel,
    chunk: Vec<u8>,
) -> EventFrame {
    event(
        ownership,
        EventPayload::ProcessOutput(ProcessOutputEvent {
            process_id: process_id.to_owned(),
            channel,
            chunk,
        }),
    )
}

pub fn process_exited_event(
    ownership: OwnershipScope,
    process_id: &str,
    exit_code: i32,
) -> EventFrame {
    event(
        ownership,
        EventPayload::ProcessExited(ProcessExitedEvent {
            process_id: process_id.to_owned(),
            exit_code,
        }),
    )
}

pub fn unsupported_guest_kernel_call_event(
    ownership: OwnershipScope,
    process_id: &str,
    execution_id: &str,
    operation: &str,
    payload_size_bytes: usize,
) -> EventFrame {
    event(
        ownership,
        EventPayload::Structured(StructuredEvent {
            name: String::from(UNSUPPORTED_GUEST_KERNEL_CALL_EVENT),
            detail: unsupported_guest_kernel_call_detail(
                Some(process_id),
                execution_id,
                operation,
                payload_size_bytes,
            ),
        }),
    )
}

pub fn unsupported_guest_kernel_call_detail(
    process_id: Option<&str>,
    execution_id: &str,
    operation: &str,
    payload_size_bytes: usize,
) -> HashMap<String, String> {
    let mut detail = HashMap::from([
        (String::from("execution_id"), execution_id.to_owned()),
        (String::from("operation"), operation.to_owned()),
        (
            String::from("payload_size_bytes"),
            payload_size_bytes.to_string(),
        ),
    ]);
    if let Some(process_id) = process_id {
        detail.insert(String::from("process_id"), process_id.to_owned());
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_sidecar_protocol::protocol::RequestPayload;

    fn authenticate_request() -> AuthenticateRequest {
        AuthenticateRequest {
            client_name: String::from("test"),
            auth_token: String::from("token"),
            protocol_version: agentos_sidecar_protocol::protocol::PROTOCOL_VERSION,
            bridge_version: agentos_bridge::bridge_contract().version,
        }
    }

    #[test]
    fn reject_preserves_request_identity_and_ownership() {
        let request = RequestFrame::new(
            42,
            OwnershipScope::connection("conn-1"),
            RequestPayload::Authenticate(authenticate_request()),
        );

        let response = reject(&request, "bad_request", "nope");

        assert_eq!(response.request_id, request.request_id);
        assert_eq!(response.ownership, request.ownership);
        match response.payload {
            ResponsePayload::Rejected(rejected) => {
                assert_eq!(rejected.code, "bad_request");
                assert_eq!(rejected.message, "nope");
            }
            other => panic!("unexpected response payload: {other:?}"),
        }
    }

    #[test]
    fn validates_authenticate_versions() {
        validate_authenticate_versions(&authenticate_request()).expect("current versions");

        let mut stale_protocol = authenticate_request();
        stale_protocol.protocol_version = stale_protocol.protocol_version.saturating_sub(1);
        let error = validate_authenticate_versions(&stale_protocol).expect_err("protocol mismatch");
        assert_eq!(error.code(), "protocol_version_mismatch");
        assert!(error
            .message()
            .contains("sidecar protocol version mismatch"));

        let mut stale_bridge = authenticate_request();
        stale_bridge.bridge_version = stale_bridge.bridge_version.saturating_sub(1);
        let error = validate_authenticate_versions(&stale_bridge).expect_err("bridge mismatch");
        assert_eq!(error.code(), "bridge_version_mismatch");
        assert!(error.message().contains("bridge contract version mismatch"));
    }

    #[test]
    fn authenticated_response_sets_connection_ownership() {
        let response = authenticated_response(7, "agentos-test", String::from("conn-test"), 1024);

        assert_eq!(response.request_id, 7);
        assert_eq!(response.ownership, OwnershipScope::connection("conn-test"));
        match response.payload {
            ResponsePayload::Authenticated(authenticated) => {
                assert_eq!(authenticated.sidecar_id, "agentos-test");
                assert_eq!(authenticated.connection_id, "conn-test");
                assert_eq!(authenticated.max_frame_bytes, 1024);
            }
            other => panic!("unexpected response payload: {other:?}"),
        }
    }

    #[test]
    fn session_opened_response_sets_session_ownership() {
        let response =
            session_opened_response(8, String::from("conn-1"), String::from("session-1"));

        assert_eq!(response.request_id, 8);
        assert_eq!(
            response.ownership,
            OwnershipScope::session("conn-1", "session-1")
        );
        match response.payload {
            ResponsePayload::SessionOpened(opened) => {
                assert_eq!(opened.owner_connection_id, "conn-1");
                assert_eq!(opened.session_id, "session-1");
            }
            other => panic!("unexpected response payload: {other:?}"),
        }
    }

    #[test]
    fn lifecycle_response_helpers_preserve_request_ownership() {
        let request = RequestFrame::new(
            43,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            RequestPayload::Authenticate(authenticate_request()),
        );

        let created = vm_created_response(&request, String::from("vm-1"));
        assert_eq!(created.request_id, request.request_id);
        assert_eq!(created.ownership, request.ownership);
        match created.payload {
            ResponsePayload::VmCreated(created) => assert_eq!(created.vm_id, "vm-1"),
            other => panic!("unexpected response payload: {other:?}"),
        }

        let bootstrapped = root_filesystem_bootstrapped_response(&request, 3);
        assert_eq!(bootstrapped.request_id, request.request_id);
        assert_eq!(bootstrapped.ownership, request.ownership);
        match bootstrapped.payload {
            ResponsePayload::RootFilesystemBootstrapped(bootstrapped) => {
                assert_eq!(bootstrapped.entry_count, 3);
            }
            other => panic!("unexpected response payload: {other:?}"),
        }

        let disposed = vm_disposed_response(&request, String::from("vm-1"));
        assert_eq!(disposed.request_id, request.request_id);
        assert_eq!(disposed.ownership, request.ownership);
        match disposed.payload {
            ResponsePayload::VmDisposed(disposed) => assert_eq!(disposed.vm_id, "vm-1"),
            other => panic!("unexpected response payload: {other:?}"),
        }
    }

    #[test]
    fn process_started_response_preserves_pid() {
        let request = RequestFrame::new(
            44,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            RequestPayload::Authenticate(authenticate_request()),
        );

        let started = process_started_response(&request, String::from("proc-1"), Some(123));
        assert_eq!(started.request_id, request.request_id);
        assert_eq!(started.ownership, request.ownership);
        match started.payload {
            ResponsePayload::ProcessStarted(started) => {
                assert_eq!(started.process_id, "proc-1");
                assert_eq!(started.pid, Some(123));
            }
            other => panic!("unexpected response payload: {other:?}"),
        }

        let started_without_pid = process_started_response(&request, String::from("proc-2"), None);
        match started_without_pid.payload {
            ResponsePayload::ProcessStarted(started) => {
                assert_eq!(started.process_id, "proc-2");
                assert_eq!(started.pid, None);
            }
            other => panic!("unexpected response payload: {other:?}"),
        }
    }

    #[test]
    fn shared_response_helpers_preserve_payloads() {
        let request = RequestFrame::new(
            45,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            RequestPayload::Authenticate(authenticate_request()),
        );

        match vm_configured_response(&request, 2, 3, Vec::new(), Vec::new()).payload {
            ResponsePayload::VmConfigured(configured) => {
                assert_eq!(configured.applied_mounts, 2);
                assert_eq!(configured.applied_software, 3);
                assert!(configured.projected_commands.is_empty());
            }
            other => panic!("unexpected response payload: {other:?}"),
        }

        match snapshot_exported_response(&request, String::from("layer-1"), Vec::new()).payload {
            ResponsePayload::SnapshotExported(exported) => {
                assert_eq!(exported.layer_id, "layer-1");
                assert!(exported.entries.is_empty());
            }
            other => panic!("unexpected response payload: {other:?}"),
        }

        match stdin_written_response(&request, String::from("proc-1"), 9).payload {
            ResponsePayload::StdinWritten(written) => {
                assert_eq!(written.process_id, "proc-1");
                assert_eq!(written.accepted_bytes, 9);
            }
            other => panic!("unexpected response payload: {other:?}"),
        }

        match process_killed_response(&request, String::from("proc-1")).payload {
            ResponsePayload::ProcessKilled(killed) => {
                assert_eq!(killed.process_id, "proc-1");
            }
            other => panic!("unexpected response payload: {other:?}"),
        }

        match signal_state_response(&request, String::from("proc-1"), []).payload {
            ResponsePayload::SignalState(state) => {
                assert_eq!(state.process_id, "proc-1");
                assert!(state.handlers.is_empty());
            }
            other => panic!("unexpected response payload: {other:?}"),
        }

        match zombie_timer_count_response(&request, 4).payload {
            ResponsePayload::ZombieTimerCount(count) => assert_eq!(count.count, 4),
            other => panic!("unexpected response payload: {other:?}"),
        }
    }

    #[test]
    fn process_event_helpers_build_vm_owned_events() {
        let ownership = OwnershipScope::vm("conn-1", "session-1", "vm-1");

        let output = process_output_event(
            ownership.clone(),
            "proc-1",
            StreamChannel::Stdout,
            b"hello".to_vec(),
        );
        assert_eq!(output.ownership, ownership);
        match output.payload {
            EventPayload::ProcessOutput(event) => {
                assert_eq!(event.process_id, "proc-1");
                assert_eq!(event.channel, StreamChannel::Stdout);
                assert_eq!(event.chunk, b"hello");
            }
            other => panic!("unexpected event payload: {other:?}"),
        }

        let exited = process_exited_event(output.ownership, "proc-1", 7);
        match exited.payload {
            EventPayload::ProcessExited(event) => {
                assert_eq!(event.process_id, "proc-1");
                assert_eq!(event.exit_code, 7);
            }
            other => panic!("unexpected event payload: {other:?}"),
        }
    }

    #[test]
    fn unsupported_guest_kernel_call_event_preserves_execution_identity() {
        let ownership = OwnershipScope::vm("conn-1", "session-1", "vm-1");
        let event = unsupported_guest_kernel_call_event(
            ownership.clone(),
            "proc-1",
            "exec-1",
            "fs.read",
            17,
        );

        assert_eq!(event.ownership, ownership);
        match event.payload {
            EventPayload::Structured(event) => {
                assert_eq!(event.name, "guest.kernel_call.unsupported");
                assert_eq!(event.detail["process_id"], "proc-1");
                assert_eq!(event.detail["execution_id"], "exec-1");
                assert_eq!(event.detail["operation"], "fs.read");
                assert_eq!(event.detail["payload_size_bytes"], "17");
            }
            other => panic!("unexpected event payload: {other:?}"),
        }
    }

    #[test]
    fn unsupported_guest_kernel_call_detail_can_omit_process_identity() {
        let detail = unsupported_guest_kernel_call_detail(None, "exec-1", "fs.read", 17);

        assert_eq!(detail["execution_id"], "exec-1");
        assert_eq!(detail["operation"], "fs.read");
        assert_eq!(detail["payload_size_bytes"], "17");
        assert!(!detail.contains_key("process_id"));
    }
}
