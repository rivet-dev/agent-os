use agent_os_sidecar::protocol::{
    validate_frame, AuthenticateRequest, AuthenticatedResponse, CreateVmRequest, EventFrame,
    GetZombieTimerCountRequest, GuestRuntimeKind, NativeFrameCodec, OpenSessionRequest,
    OwnershipScope, PatternPermissionScope, PermissionMode, PermissionsPolicy,
    ProcessStartedResponse, ProjectedModuleDescriptor, ProtocolCodecError, ProtocolFrame,
    RequestFrame, RequestPayload, ResponseFrame, ResponsePayload, ResponseTracker,
    ResponseTrackerError, SidecarPlacement, SidecarRequestFrame, SidecarRequestPayload,
    SidecarResponseFrame, SidecarResponsePayload, SidecarResponseTracker,
    SidecarResponseTrackerError, SoftwareDescriptor, StructuredEvent, ToolInvocationRequest,
    ToolInvocationResultResponse, VmLifecycleEvent, VmLifecycleState, WriteStdinRequest,
};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn guest_runtime_kind_serializes_python_in_snake_case() {
    let encoded = serde_json::to_value(GuestRuntimeKind::Python).expect("serialize runtime");
    assert_eq!(encoded, json!("python"));

    let decoded: GuestRuntimeKind =
        serde_json::from_value(json!("python")).expect("decode runtime");
    assert_eq!(decoded, GuestRuntimeKind::Python);
}

#[test]
fn codec_round_trips_authenticated_setup_and_session_messages() {
    let codec = NativeFrameCodec::default();
    let frame = ProtocolFrame::Request(RequestFrame::new(
        1,
        OwnershipScope::connection("conn-1"),
        RequestPayload::Authenticate(AuthenticateRequest {
            client_name: "packages/core".to_string(),
            auth_token: "signed-token".to_string(),
        }),
    ));

    let encoded = codec.encode(&frame).expect("encode");
    let decoded = codec.decode(&encoded).expect("decode");

    assert_eq!(decoded, frame);

    let session_frame = ProtocolFrame::Request(RequestFrame::new(
        2,
        OwnershipScope::connection("conn-1"),
        RequestPayload::OpenSession(OpenSessionRequest {
            placement: SidecarPlacement::Shared {
                pool: Some("default".to_string()),
            },
            metadata: BTreeMap::from([(String::from("owner"), String::from("packages/core"))]),
        }),
    ));

    let encoded = codec.encode(&session_frame).expect("encode session");
    let decoded = codec.decode(&encoded).expect("decode session");

    assert_eq!(decoded, session_frame);
}

#[test]
fn codec_round_trips_vm_scoped_events_and_responses() {
    let codec = NativeFrameCodec::default();
    let response = ProtocolFrame::Response(ResponseFrame::new(
        44,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        ResponsePayload::ProcessStarted(ProcessStartedResponse {
            process_id: "proc-1".to_string(),
            pid: None,
        }),
    ));

    let event = ProtocolFrame::Event(EventFrame::new(
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        agent_os_sidecar::protocol::EventPayload::VmLifecycle(VmLifecycleEvent {
            state: VmLifecycleState::Ready,
        }),
    ));

    assert_eq!(
        codec.decode(&codec.encode(&response).unwrap()).unwrap(),
        response
    );
    assert_eq!(codec.decode(&codec.encode(&event).unwrap()).unwrap(), event);
}

#[test]
fn codec_round_trips_sidecar_request_and_response_frames() {
    let codec = NativeFrameCodec::default();
    let request = ProtocolFrame::SidecarRequest(SidecarRequestFrame::new(
        -7,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::ToolInvocation(ToolInvocationRequest {
            invocation_id: "invoke-1".to_string(),
            tool_key: "toolkit:tool".to_string(),
            input: json!({ "prompt": "ping" }),
            timeout_ms: 5_000,
        }),
    ));
    let response = ProtocolFrame::SidecarResponse(SidecarResponseFrame::new(
        -7,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarResponsePayload::ToolInvocationResult(ToolInvocationResultResponse {
            invocation_id: "invoke-1".to_string(),
            result: Some(json!({ "ok": true })),
            error: None,
        }),
    ));

    assert_eq!(
        codec.decode(&codec.encode(&request).unwrap()).unwrap(),
        request
    );
    assert_eq!(
        codec.decode(&codec.encode(&response).unwrap()).unwrap(),
        response
    );
}

#[test]
fn codec_rejects_invalid_ownership_binding() {
    let frame = ProtocolFrame::Request(RequestFrame::new(
        9,
        OwnershipScope::connection("conn-1"),
        RequestPayload::CreateVm(CreateVmRequest {
            runtime: GuestRuntimeKind::JavaScript,
            metadata: BTreeMap::new(),
            root_filesystem: Default::default(),
            permissions: None,
        }),
    ));

    assert_eq!(
        validate_frame(&frame),
        Err(ProtocolCodecError::InvalidOwnershipScope {
            required: agent_os_sidecar::protocol::OwnershipRequirement::Session,
            actual: agent_os_sidecar::protocol::OwnershipRequirement::Connection,
        }),
    );
}

#[test]
fn codec_rejects_frames_over_the_configured_limit() {
    let codec = NativeFrameCodec::new(64);
    let frame = ProtocolFrame::Request(RequestFrame::new(
        11,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        RequestPayload::WriteStdin(WriteStdinRequest {
            process_id: "proc-1".to_string(),
            chunk: "x".repeat(256),
        }),
    ));

    assert!(matches!(
        codec.encode(&frame),
        Err(ProtocolCodecError::FrameTooLarge { .. })
    ));
}

#[test]
fn response_tracker_enforces_request_response_correlation_and_duplicate_hardening() {
    let mut tracker = ResponseTracker::default();
    let request = RequestFrame::new(
        77,
        OwnershipScope::session("conn-1", "session-1"),
        RequestPayload::CreateVm(CreateVmRequest {
            runtime: GuestRuntimeKind::JavaScript,
            metadata: BTreeMap::new(),
            root_filesystem: Default::default(),
            permissions: None,
        }),
    );
    tracker
        .register_request(&request)
        .expect("register request");

    let response = ResponseFrame::new(
        77,
        OwnershipScope::session("conn-1", "session-1"),
        ResponsePayload::VmCreated(agent_os_sidecar::protocol::VmCreatedResponse {
            vm_id: "vm-1".to_string(),
        }),
    );
    tracker.accept_response(&response).expect("accept response");

    assert_eq!(
        tracker.accept_response(&response),
        Err(ResponseTrackerError::DuplicateResponse { request_id: 77 }),
    );
    assert_eq!(
        tracker.accept_response(&ResponseFrame::new(
            88,
            OwnershipScope::session("conn-1", "session-1"),
            ResponsePayload::VmCreated(agent_os_sidecar::protocol::VmCreatedResponse {
                vm_id: "vm-2".to_string(),
            }),
        )),
        Err(ResponseTrackerError::UnmatchedResponse { request_id: 88 }),
    );
}

#[test]
fn response_tracker_rejects_kind_and_ownership_mismatches() {
    let mut tracker = ResponseTracker::default();
    let request = RequestFrame::new(
        90,
        OwnershipScope::session("conn-1", "session-1"),
        RequestPayload::CreateVm(CreateVmRequest {
            runtime: GuestRuntimeKind::WebAssembly,
            metadata: BTreeMap::from([(String::from("runtime"), String::from("wasm"))]),
            root_filesystem: Default::default(),
            permissions: None,
        }),
    );
    tracker
        .register_request(&request)
        .expect("register request");

    assert_eq!(
        tracker.accept_response(&ResponseFrame::new(
            90,
            OwnershipScope::session("conn-1", "session-2"),
            ResponsePayload::VmCreated(agent_os_sidecar::protocol::VmCreatedResponse {
                vm_id: "vm-1".to_string(),
            }),
        )),
        Err(ResponseTrackerError::OwnershipMismatch {
            request_id: 90,
            expected: OwnershipScope::session("conn-1", "session-1"),
            actual: OwnershipScope::session("conn-1", "session-2"),
        }),
    );

    let mut tracker = ResponseTracker::default();
    tracker
        .register_request(&request)
        .expect("register request again");

    assert_eq!(
        tracker.accept_response(&ResponseFrame::new(
            90,
            OwnershipScope::session("conn-1", "session-1"),
            ResponsePayload::Authenticated(AuthenticatedResponse {
                sidecar_id: "sidecar-1".to_string(),
                connection_id: "conn-1".to_string(),
                max_frame_bytes: 1024,
            }),
        )),
        Err(ResponseTrackerError::ResponseKindMismatch {
            request_id: 90,
            expected: "vm_created".to_string(),
            actual: "authenticated".to_string(),
        }),
    );
}

#[test]
fn response_tracker_accepts_zombie_timer_count_responses() {
    let mut tracker = ResponseTracker::default();
    let request = RequestFrame::new(
        91,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        RequestPayload::GetZombieTimerCount(GetZombieTimerCountRequest::default()),
    );
    tracker
        .register_request(&request)
        .expect("register request");

    tracker
        .accept_response(&ResponseFrame::new(
            91,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            ResponsePayload::ZombieTimerCount(
                agent_os_sidecar::protocol::ZombieTimerCountResponse { count: 2 },
            ),
        ))
        .expect("accept response");
}

#[test]
fn response_tracker_caps_completed_entries() {
    let mut tracker = ResponseTracker::with_completed_cap(3);

    for request_id in 1..=10 {
        let request = RequestFrame::new(
            request_id,
            OwnershipScope::connection("conn-1"),
            RequestPayload::Authenticate(AuthenticateRequest {
                client_name: "packages/core".to_string(),
                auth_token: format!("token-{request_id}"),
            }),
        );
        tracker
            .register_request(&request)
            .expect("register request");
        tracker
            .accept_response(&ResponseFrame::new(
                request_id,
                OwnershipScope::connection("conn-1"),
                ResponsePayload::Authenticated(AuthenticatedResponse {
                    sidecar_id: "sidecar-1".to_string(),
                    connection_id: "conn-1".to_string(),
                    max_frame_bytes: 1024,
                }),
            ))
            .expect("accept response");

        assert!(
            tracker.completed_count() <= 3,
            "completed set should stay bounded"
        );
    }

    assert_eq!(tracker.completed_count(), 3);
}

#[test]
fn sidecar_response_tracker_enforces_request_response_correlation() {
    let mut tracker = SidecarResponseTracker::default();
    let request = SidecarRequestFrame::new(
        -9,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::ToolInvocation(ToolInvocationRequest {
            invocation_id: "invoke-1".to_string(),
            tool_key: "toolkit:tool".to_string(),
            input: json!({ "value": 1 }),
            timeout_ms: 1_000,
        }),
    );
    tracker
        .register_request(&request)
        .expect("register sidecar request");

    tracker
        .accept_response(&SidecarResponseFrame::new(
            -9,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            SidecarResponsePayload::ToolInvocationResult(ToolInvocationResultResponse {
                invocation_id: "invoke-1".to_string(),
                result: Some(json!({ "ok": true })),
                error: None,
            }),
        ))
        .expect("accept sidecar response");

    assert_eq!(
        tracker.accept_response(&SidecarResponseFrame::new(
            -9,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            SidecarResponsePayload::ToolInvocationResult(ToolInvocationResultResponse {
                invocation_id: "invoke-1".to_string(),
                result: None,
                error: Some("duplicate".to_string()),
            }),
        )),
        Err(SidecarResponseTrackerError::DuplicateResponse { request_id: -9 }),
    );
}

#[test]
fn codec_rejects_request_id_direction_mismatches() {
    let host_response = ProtocolFrame::Response(ResponseFrame::new(
        -1,
        OwnershipScope::connection("conn-1"),
        ResponsePayload::Authenticated(AuthenticatedResponse {
            sidecar_id: "sidecar-1".to_string(),
            connection_id: "conn-1".to_string(),
            max_frame_bytes: 1024,
        }),
    ));
    assert_eq!(
        validate_frame(&host_response),
        Err(ProtocolCodecError::InvalidRequestDirection {
            request_id: -1,
            expected: agent_os_sidecar::protocol::RequestDirection::Host,
        }),
    );

    let sidecar_request = ProtocolFrame::SidecarRequest(SidecarRequestFrame::new(
        1,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::ToolInvocation(ToolInvocationRequest {
            invocation_id: "invoke-2".to_string(),
            tool_key: "toolkit:tool".to_string(),
            input: json!({}),
            timeout_ms: 100,
        }),
    ));
    assert_eq!(
        validate_frame(&sidecar_request),
        Err(ProtocolCodecError::InvalidRequestDirection {
            request_id: 1,
            expected: agent_os_sidecar::protocol::RequestDirection::Sidecar,
        }),
    );
}

#[test]
fn schema_supports_configuration_and_structured_events() {
    let frame = ProtocolFrame::Request(RequestFrame::new(
        23,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        RequestPayload::ConfigureVm(agent_os_sidecar::protocol::ConfigureVmRequest {
            mounts: vec![agent_os_sidecar::protocol::MountDescriptor {
                guest_path: "/workspace".to_string(),
                read_only: false,
                plugin: agent_os_sidecar::protocol::MountPluginDescriptor {
                    id: "host_dir".to_string(),
                    config: json!({
                        "hostPath": "/tmp/project",
                        "readOnly": false,
                    }),
                },
            }],
            software: vec![SoftwareDescriptor {
                package_name: "@rivet-dev/agent-os".to_string(),
                root: "/pkg".to_string(),
            }],
            permissions: Some(PermissionsPolicy {
                fs: None,
                network: Some(PatternPermissionScope::Mode(PermissionMode::Ask)),
                child_process: None,
                env: None,
            }),
            module_access_cwd: None,
            instructions: vec!["keep timing mitigation enabled".to_string()],
            projected_modules: vec![ProjectedModuleDescriptor {
                package_name: "workspace".to_string(),
                entrypoint: "/workspace/index.ts".to_string(),
            }],
            command_permissions: BTreeMap::new(),
        }),
    ));

    validate_frame(&frame).expect("configuration request is valid");

    let event = EventFrame::new(
        OwnershipScope::session("conn-1", "session-1"),
        agent_os_sidecar::protocol::EventPayload::Structured(StructuredEvent {
            name: "guest.lifecycle".to_string(),
            detail: BTreeMap::from([(String::from("state"), String::from("ready"))]),
        }),
    );
    validate_frame(&ProtocolFrame::Event(event)).expect("structured event is valid");
}
