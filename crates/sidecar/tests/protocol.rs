use agent_os_sidecar::protocol::{
    validate_frame, AuthenticateRequest, AuthenticatedResponse, CreateVmRequest, EventFrame,
    GetZombieTimerCountRequest, GuestRuntimeKind, NativeFrameCodec, NativePayloadCodec,
    OpenSessionRequest, OwnershipScope, PatternPermissionScope, PermissionMode, PermissionsPolicy,
    ProcessStartedResponse, ProjectedModuleDescriptor, ProtocolCodecError, ProtocolFrame,
    RequestFrame, RequestPayload, ResponseFrame, ResponsePayload, ResponseTracker,
    ResponseTrackerError, RootFilesystemDescriptor, RootFilesystemEntry, RootFilesystemEntryKind,
    RootFilesystemLowerDescriptor, SidecarPlacement, SidecarRequestFrame, SidecarRequestPayload,
    SidecarResponseFrame, SidecarResponsePayload, SidecarResponseTracker,
    SidecarResponseTrackerError, SoftwareDescriptor, StructuredEvent, ToolInvocationRequest,
    ToolInvocationResultResponse, TransformHttpRequest, TransformHttpResultResponse,
    VmLifecycleEvent, VmLifecycleState, WriteStdinRequest,
};
use serde_json::json;
use std::collections::BTreeMap;

const BARE_SCHEMA_V1: &str = include_str!("../protocol/agent_os_sidecar_v1.bare");
const BARE_MIGRATION_PLAN: &str = include_str!("../protocol/README.md");

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
fn codec_round_trips_transform_http_request_and_response() {
    let codec = NativeFrameCodec::default();
    let request = ProtocolFrame::SidecarRequest(SidecarRequestFrame::new(
        -20,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::TransformHttpRequest(TransformHttpRequest {
            request_id: "http-42".to_string(),
            url: "https://api.example.com/v1/data".to_string(),
            method: "POST".to_string(),
            headers: json!({
                "authorization": ["Bearer __CREDENTIAL_REF_abc__"],
                "content-type": ["application/json"],
            }),
            body: Some(r#"{"key":"value"}"#.to_string()),
        }),
    ));
    let response = ProtocolFrame::SidecarResponse(SidecarResponseFrame::new(
        -20,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarResponsePayload::TransformHttpResult(TransformHttpResultResponse {
            request_id: "http-42".to_string(),
            url: None,
            method: None,
            headers: Some(json!({
                "authorization": ["Bearer real-secret-token"],
                "content-type": ["application/json"],
            })),
            body: None,
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
fn bare_codec_round_trips_transform_http_request_frames() {
    let codec = NativeFrameCodec::with_payload_codec(1024 * 1024, NativePayloadCodec::Bare);
    let request = ProtocolFrame::SidecarRequest(SidecarRequestFrame::new(
        -21,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::TransformHttpRequest(TransformHttpRequest {
            request_id: "http-99".to_string(),
            url: "https://api.stripe.com/v1/charges".to_string(),
            method: "GET".to_string(),
            headers: json!({"accept": ["application/json"]}),
            body: None,
        }),
    ));

    let encoded = codec.encode(&request).expect("encode bare transform request");
    let decoded = codec.decode(&encoded).expect("decode bare transform request");
    assert_eq!(decoded, request);

    let response = ProtocolFrame::SidecarResponse(SidecarResponseFrame::new(
        -21,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarResponsePayload::TransformHttpResult(TransformHttpResultResponse {
            request_id: "http-99".to_string(),
            url: Some("https://api.stripe.com/v1/charges".to_string()),
            method: Some("GET".to_string()),
            headers: Some(json!({"accept": ["application/json"], "authorization": ["Bearer sk_live_xxx"]})),
            body: None,
            error: None,
        }),
    ));

    let encoded = codec.encode(&response).expect("encode bare transform response");
    let decoded = codec.decode(&encoded).expect("decode bare transform response");
    assert_eq!(decoded, response);
}

#[test]
fn transform_http_result_with_error_round_trips() {
    let codec = NativeFrameCodec::default();
    let response = ProtocolFrame::SidecarResponse(SidecarResponseFrame::new(
        -22,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarResponsePayload::TransformHttpResult(TransformHttpResultResponse {
            request_id: "http-fail".to_string(),
            url: None,
            method: None,
            headers: None,
            body: None,
            error: Some("credential resolver unavailable".to_string()),
        }),
    ));

    assert_eq!(
        codec.decode(&codec.encode(&response).unwrap()).unwrap(),
        response
    );
}

#[test]
fn bare_codec_round_trips_frames_with_json_utf8_fields() {
    let codec = NativeFrameCodec::with_payload_codec(1024 * 1024, NativePayloadCodec::Bare);
    let frame = ProtocolFrame::SidecarRequest(SidecarRequestFrame::new(
        -12,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::ToolInvocation(ToolInvocationRequest {
            invocation_id: "invoke-12".to_string(),
            tool_key: "toolkit:search".to_string(),
            input: json!({
                "cursor": "abc123",
                "includeSchema": true,
            }),
            timeout_ms: 2_000,
        }),
    ));

    let encoded = codec.encode(&frame).expect("encode bare frame");
    assert_eq!(
        encoded[4], 4,
        "BARE sidecar_request frames should start with tag 4"
    );

    let decoded = codec.decode(&encoded).expect("decode bare frame");
    assert_eq!(decoded, frame);
}

#[test]
fn bare_codec_round_trips_authenticate_request_frames() {
    let codec = NativeFrameCodec::with_payload_codec(1024 * 1024, NativePayloadCodec::Bare);
    let frame = ProtocolFrame::Request(RequestFrame::new(
        1,
        OwnershipScope::connection("client-hint"),
        RequestPayload::Authenticate(AuthenticateRequest {
            client_name: "packages-core-vitest".to_string(),
            auth_token: "packages-core-vitest-token".to_string(),
        }),
    ));

    let encoded = codec
        .encode(&frame)
        .expect("encode bare authenticate request");
    let decoded = codec
        .decode(&encoded)
        .expect("decode bare authenticate request");

    assert_eq!(decoded, frame);
}

#[test]
fn bare_codec_round_trips_root_filesystem_lower_descriptors() {
    let lower = RootFilesystemLowerDescriptor::BundledBaseFilesystem;
    let encoded = serde_bare::to_vec(&lower).expect("encode bare root filesystem lower");
    let decoded: RootFilesystemLowerDescriptor =
        serde_bare::from_slice(&encoded).expect("decode bare root filesystem lower");

    assert_eq!(decoded, lower);
}

#[test]
fn bare_codec_round_trips_root_filesystem_descriptors_with_snapshot_lowers() {
    let descriptor = RootFilesystemDescriptor {
        disable_default_base_layer: true,
        lowers: vec![
            RootFilesystemLowerDescriptor::Snapshot {
                entries: vec![RootFilesystemEntry {
                    path: String::from("/workspace"),
                    kind: RootFilesystemEntryKind::Directory,
                    ..Default::default()
                }],
            },
            RootFilesystemLowerDescriptor::BundledBaseFilesystem,
        ],
        ..Default::default()
    };

    let encoded = serde_bare::to_vec(&descriptor).expect("encode bare root filesystem descriptor");
    let decoded: RootFilesystemDescriptor =
        serde_bare::from_slice(&encoded).expect("decode bare root filesystem descriptor");

    assert_eq!(decoded, descriptor);
}

#[test]
fn bare_codec_round_trips_create_vm_requests_with_snapshot_lowers() {
    let codec = NativeFrameCodec::with_payload_codec(1024 * 1024, NativePayloadCodec::Bare);
    let frame = ProtocolFrame::Request(RequestFrame::new(
        2,
        OwnershipScope::session("conn-1", "session-1"),
        RequestPayload::CreateVm(CreateVmRequest {
            runtime: GuestRuntimeKind::JavaScript,
            metadata: BTreeMap::from([(String::from("cwd"), String::from("/workspace"))]),
            root_filesystem: RootFilesystemDescriptor {
                disable_default_base_layer: true,
                lowers: vec![
                    RootFilesystemLowerDescriptor::Snapshot {
                        entries: vec![RootFilesystemEntry {
                            path: String::from("/workspace"),
                            kind: RootFilesystemEntryKind::Directory,
                            ..Default::default()
                        }],
                    },
                    RootFilesystemLowerDescriptor::BundledBaseFilesystem,
                ],
                ..Default::default()
            },
            permissions: None,
        }),
    ));

    let encoded = codec.encode(&frame).expect("encode bare create_vm request");
    let decoded = codec
        .decode(&encoded)
        .expect("decode bare create_vm request");

    assert_eq!(decoded, frame);
}

#[test]
fn codec_auto_detects_json_and_bare_payloads() {
    let json_codec = NativeFrameCodec::default();
    let bare_codec = NativeFrameCodec::with_payload_codec(1024 * 1024, NativePayloadCodec::Bare);
    let frame = ProtocolFrame::SidecarRequest(SidecarRequestFrame::new(
        -11,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::ToolInvocation(ToolInvocationRequest {
            invocation_id: "invoke-1".to_string(),
            tool_key: "toolkit:search".to_string(),
            input: json!({ "query": "ping" }),
            timeout_ms: 2_000,
        }),
    ));

    let json_encoded = json_codec.encode(&frame).expect("encode json frame");
    let bare_encoded = bare_codec.encode(&frame).expect("encode bare frame");

    assert_eq!(json_codec.decode(&json_encoded).unwrap(), frame);
    assert_eq!(json_codec.decode(&bare_encoded).unwrap(), frame);
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
            allowed_node_builtins: Vec::new(),
            loopback_exempt_ports: Vec::new(),
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

#[test]
fn checked_in_bare_schema_covers_all_top_level_frame_payload_types() {
    for type_name in [
        "type ProtocolFrame union {",
        "type RequestPayload union {",
        "type ResponsePayload union {",
        "type EventPayload union {",
        "type SidecarRequestPayload union {",
        "type SidecarResponsePayload union {",
        "AuthenticateRequest",
        "OpenSessionRequest",
        "CreateVmRequest",
        "CreateSessionRequest",
        "SessionRequest",
        "GetSessionStateRequest",
        "CloseAgentSessionRequest",
        "DisposeVmRequest",
        "BootstrapRootFilesystemRequest",
        "ConfigureVmRequest",
        "RegisterToolkitRequest",
        "CreateLayerRequest",
        "SealLayerRequest",
        "ImportSnapshotRequest",
        "ExportSnapshotRequest",
        "CreateOverlayRequest",
        "GuestFilesystemCallRequest",
        "SnapshotRootFilesystemRequest",
        "ExecuteRequest",
        "WriteStdinRequest",
        "CloseStdinRequest",
        "KillProcessRequest",
        "GetProcessSnapshotRequest",
        "FindListenerRequest",
        "FindBoundUdpRequest",
        "GetSignalStateRequest",
        "GetZombieTimerCountRequest",
        "HostFilesystemCallRequest",
        "PermissionRequest",
        "PersistenceLoadRequest",
        "PersistenceFlushRequest",
        "AuthenticatedResponse",
        "SessionOpenedResponse",
        "VmCreatedResponse",
        "SessionCreatedResponse",
        "SessionRpcResponse",
        "SessionStateResponse",
        "AgentSessionClosedResponse",
        "VmDisposedResponse",
        "RootFilesystemBootstrappedResponse",
        "VmConfiguredResponse",
        "ToolkitRegisteredResponse",
        "LayerCreatedResponse",
        "LayerSealedResponse",
        "SnapshotImportedResponse",
        "SnapshotExportedResponse",
        "OverlayCreatedResponse",
        "GuestFilesystemResultResponse",
        "RootFilesystemSnapshotResponse",
        "ProcessStartedResponse",
        "StdinWrittenResponse",
        "StdinClosedResponse",
        "ProcessKilledResponse",
        "ProcessSnapshotResponse",
        "ListenerSnapshotResponse",
        "BoundUdpSnapshotResponse",
        "SignalStateResponse",
        "ZombieTimerCountResponse",
        "FilesystemResultResponse",
        "PermissionDecisionResponse",
        "PersistenceStateResponse",
        "PersistenceFlushedResponse",
        "RejectedResponse",
        "VmLifecycleEvent",
        "ProcessOutputEvent",
        "ProcessExitedEvent",
        "StructuredEvent",
        "ToolInvocationRequest",
        "SidecarPermissionRequest",
        "JsBridgeCallRequest",
        "ToolInvocationResultResponse",
        "SidecarPermissionResultResponse",
        "JsBridgeResultResponse",
    ] {
        assert!(
            BARE_SCHEMA_V1.contains(type_name),
            "schema is missing `{type_name}`"
        );
    }
}

#[test]
fn checked_in_bare_migration_plan_documents_dual_stack_constraints() {
    for needle in [
        "4-byte big-endian length prefix",
        "ProtocolSchema.version",
        "request_id",
        "positive",
        "negative",
        "JsonUtf8",
        "first successfully decoded frame",
        "JSON frames begin with `{`",
        "delete JSON encoding",
    ] {
        assert!(
            BARE_MIGRATION_PLAN.contains(needle),
            "migration plan is missing `{needle}`"
        );
    }
}
