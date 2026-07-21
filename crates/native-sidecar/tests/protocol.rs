use agentos_native_sidecar::protocol::{
    validate_frame, AuthenticateRequest, AuthenticatedResponse, CreateVmRequest, EventFrame,
    EventPayload, ExtEnvelope, GetZombieTimerCountRequest, GuestFilesystemCallRequest,
    GuestFilesystemOperation, GuestRuntimeKind, HostCallbackRequest, HostCallbackResultResponse,
    JsBridgeResultResponse, NativeFrameCodec, NativePayloadCodec, OpenSessionRequest,
    OwnershipScope, PatternPermissionScope, PermissionMode, PermissionsPolicy, ProcessOutputEvent,
    ProcessStartedResponse, ProjectedModuleDescriptor, ProtocolCodecError, ProtocolFrame,
    RequestFrame, RequestPayload, ResponseFrame, ResponsePayload, ResponseTracker,
    ResponseTrackerError, RootFilesystemDescriptor, RootFilesystemEntry, RootFilesystemEntryKind,
    RootFilesystemLowerDescriptor, SidecarPlacement, SidecarPlacementShared, SidecarRequestFrame,
    SidecarRequestPayload, SidecarResponseFrame, SidecarResponsePayload, SidecarResponseTracker,
    SidecarResponseTrackerError, SnapshotRootFilesystemLower, SoftwareDescriptor, StreamChannel,
    StructuredEvent, VmLifecycleEvent, VmLifecycleState, WriteStdinRequest,
};
use serde_json::json;
use std::hint::black_box;
use std::time::Instant;

const BARE_SCHEMA_V1: &str =
    include_str!("../../sidecar-protocol/protocol/agentos_sidecar_v1.bare");
const BARE_MIGRATION_PLAN: &str = include_str!("../../sidecar-protocol/protocol/README.md");

#[test]
fn guest_runtime_kind_round_trips_through_generated_codec() {
    // `GuestRuntimeKind` is now the generated wire type. The wire contract is BARE
    // (positional tags), which is what round-trips here. The previous snake_case
    // human-readable JSON form was a hand-codec artifact with no production consumer
    // (the TS side keeps its own live snake_case map under the §6c facade).
    let encoded = serde_bare::to_vec(&GuestRuntimeKind::Python).expect("bare encode runtime");
    let decoded: GuestRuntimeKind = serde_bare::from_slice(&encoded).expect("bare decode runtime");
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
            protocol_version: agentos_native_sidecar::wire::PROTOCOL_VERSION,
            bridge_version: agentos_bridge::bridge_contract().version,
        }),
    ));

    let encoded = codec.encode(&frame).expect("encode");
    let decoded = codec.decode(&encoded).expect("decode");

    assert_eq!(decoded, frame);

    let session_frame = ProtocolFrame::Request(RequestFrame::new(
        2,
        OwnershipScope::connection("conn-1"),
        RequestPayload::OpenSession(OpenSessionRequest {
            placement: SidecarPlacement::SidecarPlacementShared(SidecarPlacementShared {
                pool: Some("default".to_string()),
            }),
            metadata: std::collections::HashMap::from([(
                String::from("owner"),
                String::from("packages/core"),
            )]),
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
        agentos_native_sidecar::protocol::EventPayload::VmLifecycle(VmLifecycleEvent {
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
#[ignore = "manual microbench for Ext envelope event encoding overhead"]
fn ext_envelope_event_encoding_microbench() {
    let codec = NativeFrameCodec::with_payload_codec(1024 * 1024, NativePayloadCodec::Bare);
    let ownership = OwnershipScope::vm("conn-1", "session-1", "vm-1");
    let process_output = ProcessOutputEvent {
        process_id: String::from("proc-1"),
        channel: StreamChannel::Stdout,
        chunk: vec![b'x'; 256],
    };
    let direct_frame = ProtocolFrame::Event(EventFrame::new(
        ownership.clone(),
        EventPayload::ProcessOutput(process_output.clone()),
    ));
    let iterations = 200_000;

    let direct_start = Instant::now();
    let mut direct_bytes = 0usize;
    for _ in 0..iterations {
        let encoded = codec
            .encode(black_box(&direct_frame))
            .expect("encode direct");
        direct_bytes = direct_bytes.wrapping_add(encoded.len());
        black_box(encoded);
    }
    let direct_elapsed = direct_start.elapsed();

    let ext_start = Instant::now();
    let mut ext_bytes = 0usize;
    for _ in 0..iterations {
        let inner = serde_bare::to_vec(black_box(&process_output)).expect("encode inner output");
        let ext_frame = ProtocolFrame::Event(EventFrame::new(
            ownership.clone(),
            EventPayload::Ext(ExtEnvelope {
                namespace: String::from("dev.rivet.agentos.acp"),
                payload: inner,
            }),
        ));
        let encoded = codec.encode(black_box(&ext_frame)).expect("encode ext");
        ext_bytes = ext_bytes.wrapping_add(encoded.len());
        black_box(encoded);
    }
    let ext_elapsed = ext_start.elapsed();

    let direct_ns = direct_elapsed.as_nanos() as f64 / iterations as f64;
    let ext_ns = ext_elapsed.as_nanos() as f64 / iterations as f64;
    println!(
        "ext_envelope_event_encoding_microbench direct_ns_per_event={direct_ns:.2} ext_ns_per_event={ext_ns:.2} ratio={:.2} direct_avg_bytes={:.1} ext_avg_bytes={:.1}",
        ext_ns / direct_ns,
        direct_bytes as f64 / iterations as f64,
        ext_bytes as f64 / iterations as f64
    );
}

#[test]
fn codec_round_trips_sidecar_request_and_response_frames() {
    let codec = NativeFrameCodec::default();
    let request = ProtocolFrame::SidecarRequest(SidecarRequestFrame::new(
        -7,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::HostCallback(HostCallbackRequest {
            invocation_id: "invoke-1".to_string(),
            callback_key: "collection:binding".to_string(),
            input: json!({ "prompt": "ping" }).to_string(),
            timeout_ms: 5_000,
        }),
    ));
    let response = ProtocolFrame::SidecarResponse(SidecarResponseFrame::new(
        -7,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarResponsePayload::HostCallbackResult(HostCallbackResultResponse {
            invocation_id: "invoke-1".to_string(),
            result: Some(json!({ "ok": true }).to_string()),
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
fn bare_codec_round_trips_frames_with_json_utf8_fields() {
    let codec = NativeFrameCodec::with_payload_codec(1024 * 1024, NativePayloadCodec::Bare);
    let frame = ProtocolFrame::SidecarRequest(SidecarRequestFrame::new(
        -12,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::HostCallback(HostCallbackRequest {
            invocation_id: "invoke-12".to_string(),
            callback_key: "browser:search".to_string(),
            input: json!({
                "cursor": "abc123",
                "includeSchema": true,
            })
            .to_string(),
            timeout_ms: 2_000,
        }),
    ));

    let encoded = codec.encode(&frame).expect("encode bare frame");
    assert_eq!(
        encoded[4], 3,
        "BARE sidecar_request frames should start with tag 3"
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
            protocol_version: agentos_native_sidecar::wire::PROTOCOL_VERSION,
            bridge_version: agentos_bridge::bridge_contract().version,
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
fn json_codec_round_trips_guest_filesystem_requests_with_optional_fields() {
    let codec = NativeFrameCodec::default();
    let frame = ProtocolFrame::Request(RequestFrame::new(
        17,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
            operation: GuestFilesystemOperation::Truncate,
            path: String::from("/workspace/hard.txt"),
            destination_path: Some(String::from("/workspace/note.txt")),
            target: Some(String::from("/workspace/target.txt")),
            content: Some(String::from("stdio-sidecar-fs")),
            encoding: None,
            recursive: true,
            max_depth: None,
            mode: Some(0o644),
            uid: Some(1000),
            gid: Some(1000),
            atime_ms: Some(1_700_000_000_000),
            mtime_ms: Some(1_710_000_000_000),
            len: Some(5),
            offset: None,
        }),
    ));

    let encoded = codec
        .encode(&frame)
        .expect("encode json guest filesystem request");
    let decoded = codec
        .decode(&encoded)
        .expect("decode json guest filesystem request");

    assert_eq!(decoded, frame);
}

#[test]
fn bare_codec_round_trips_guest_filesystem_requests_with_optional_fields() {
    let codec = NativeFrameCodec::with_payload_codec(1024 * 1024, NativePayloadCodec::Bare);
    let frame = ProtocolFrame::Request(RequestFrame::new(
        17,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
            operation: GuestFilesystemOperation::Truncate,
            path: String::from("/workspace/hard.txt"),
            destination_path: Some(String::from("/workspace/note.txt")),
            target: Some(String::from("/workspace/target.txt")),
            content: Some(String::from("stdio-sidecar-fs")),
            encoding: None,
            recursive: true,
            max_depth: None,
            mode: Some(0o644),
            uid: Some(1000),
            gid: Some(1000),
            atime_ms: Some(1_700_000_000_000),
            mtime_ms: Some(1_710_000_000_000),
            len: Some(5),
            offset: None,
        }),
    ));

    let encoded = codec
        .encode(&frame)
        .expect("encode bare guest filesystem request");
    let decoded = codec
        .decode(&encoded)
        .expect("decode bare guest filesystem request");

    assert_eq!(decoded, frame);
}

#[test]
fn bare_codec_round_trips_root_filesystem_lower_descriptors() {
    let lower = RootFilesystemLowerDescriptor::BundledBaseFilesystemLower;
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
            RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(
                SnapshotRootFilesystemLower {
                    entries: vec![RootFilesystemEntry {
                        path: String::from("/workspace"),
                        kind: RootFilesystemEntryKind::Directory,
                        ..Default::default()
                    }],
                },
            ),
            RootFilesystemLowerDescriptor::BundledBaseFilesystemLower,
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
        RequestPayload::CreateVm(CreateVmRequest::legacy_test_config(
            GuestRuntimeKind::JavaScript,
            std::collections::HashMap::from([(String::from("cwd"), String::from("/workspace"))]),
            RootFilesystemDescriptor {
                disable_default_base_layer: true,
                lowers: vec![
                    RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(
                        SnapshotRootFilesystemLower {
                            entries: vec![RootFilesystemEntry {
                                path: String::from("/workspace"),
                                kind: RootFilesystemEntryKind::Directory,
                                ..Default::default()
                            }],
                        },
                    ),
                    RootFilesystemLowerDescriptor::BundledBaseFilesystemLower,
                ],
                ..Default::default()
            },
            None,
        )),
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
        SidecarRequestPayload::HostCallback(HostCallbackRequest {
            invocation_id: "invoke-1".to_string(),
            callback_key: "browser:search".to_string(),
            input: json!({ "query": "ping" }).to_string(),
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
        RequestPayload::CreateVm(CreateVmRequest::legacy_test_config(
            GuestRuntimeKind::JavaScript,
            std::collections::HashMap::new(),
            Default::default(),
            None,
        )),
    ));

    assert_eq!(
        validate_frame(&frame),
        Err(ProtocolCodecError::InvalidOwnershipScope {
            required: agentos_native_sidecar::protocol::OwnershipRequirement::Session,
            actual: agentos_native_sidecar::protocol::OwnershipRequirement::Connection,
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
            chunk: "x".repeat(256).into_bytes(),
        }),
    ));

    assert!(matches!(
        codec.encode(&frame),
        Err(ProtocolCodecError::FrameTooLarge { .. })
    ));

    let oversized_declared_len = 65_u32;
    let mut encoded = oversized_declared_len.to_be_bytes().to_vec();
    encoded.extend(std::iter::repeat_n(0_u8, oversized_declared_len as usize));
    assert_eq!(
        codec.decode(&encoded),
        Err(ProtocolCodecError::FrameTooLarge {
            size: oversized_declared_len as usize,
            max: 64,
        })
    );
}

#[test]
fn response_tracker_enforces_request_response_correlation_and_duplicate_hardening() {
    let mut tracker = ResponseTracker::default();
    let request = RequestFrame::new(
        77,
        OwnershipScope::session("conn-1", "session-1"),
        RequestPayload::CreateVm(CreateVmRequest::legacy_test_config(
            GuestRuntimeKind::JavaScript,
            std::collections::HashMap::new(),
            Default::default(),
            None,
        )),
    );
    tracker
        .register_request(&request)
        .expect("register request");

    let response = ResponseFrame::new(
        77,
        OwnershipScope::session("conn-1", "session-1"),
        ResponsePayload::VmCreated(agentos_native_sidecar::protocol::VmCreatedResponse {
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
            ResponsePayload::VmCreated(agentos_native_sidecar::protocol::VmCreatedResponse {
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
        RequestPayload::CreateVm(CreateVmRequest::legacy_test_config(
            GuestRuntimeKind::WebAssembly,
            std::collections::HashMap::from([(String::from("runtime"), String::from("wasm"))]),
            Default::default(),
            None,
        )),
    );
    tracker
        .register_request(&request)
        .expect("register request");

    assert_eq!(
        tracker.accept_response(&ResponseFrame::new(
            90,
            OwnershipScope::session("conn-1", "session-2"),
            ResponsePayload::VmCreated(agentos_native_sidecar::protocol::VmCreatedResponse {
                vm_id: "vm-1".to_string(),
            }),
        )),
        Err(ResponseTrackerError::OwnershipMismatch {
            request_id: 90,
            expected: Box::new(OwnershipScope::session("conn-1", "session-1")),
            actual: Box::new(OwnershipScope::session("conn-1", "session-2")),
        }),
    );
    tracker
        .accept_response(&ResponseFrame::new(
            90,
            OwnershipScope::session("conn-1", "session-1"),
            ResponsePayload::VmCreated(agentos_native_sidecar::protocol::VmCreatedResponse {
                vm_id: "vm-1".to_string(),
            }),
        ))
        .expect("valid response should still be pending after ownership mismatch");

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
    tracker
        .accept_response(&ResponseFrame::new(
            90,
            OwnershipScope::session("conn-1", "session-1"),
            ResponsePayload::VmCreated(agentos_native_sidecar::protocol::VmCreatedResponse {
                vm_id: "vm-1".to_string(),
            }),
        ))
        .expect("valid response should still be pending after kind mismatch");
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
                agentos_native_sidecar::protocol::ZombieTimerCountResponse { count: 2 },
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
                protocol_version: agentos_native_sidecar::wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
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
        SidecarRequestPayload::HostCallback(HostCallbackRequest {
            invocation_id: "invoke-1".to_string(),
            callback_key: "collection:binding".to_string(),
            input: json!({ "value": 1 }).to_string(),
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
            SidecarResponsePayload::HostCallbackResult(HostCallbackResultResponse {
                invocation_id: "invoke-1".to_string(),
                result: Some(json!({ "ok": true }).to_string()),
                error: None,
            }),
        ))
        .expect("accept sidecar response");

    assert_eq!(
        tracker.accept_response(&SidecarResponseFrame::new(
            -9,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            SidecarResponsePayload::HostCallbackResult(HostCallbackResultResponse {
                invocation_id: "invoke-1".to_string(),
                result: None,
                error: Some("duplicate".to_string()),
            }),
        )),
        Err(SidecarResponseTrackerError::DuplicateResponse { request_id: -9 }),
    );
}

#[test]
fn sidecar_response_tracker_keeps_pending_entries_after_mismatches() {
    let request = SidecarRequestFrame::new(
        -10,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::HostCallback(HostCallbackRequest {
            invocation_id: "invoke-10".to_string(),
            callback_key: "collection:binding".to_string(),
            input: json!({ "value": 10 }).to_string(),
            timeout_ms: 1_000,
        }),
    );

    let mut tracker = SidecarResponseTracker::default();
    tracker
        .register_request(&request)
        .expect("register sidecar request");
    assert_eq!(
        tracker.accept_response(&SidecarResponseFrame::new(
            -10,
            OwnershipScope::vm("conn-1", "session-1", "vm-2"),
            SidecarResponsePayload::HostCallbackResult(HostCallbackResultResponse {
                invocation_id: "invoke-10".to_string(),
                result: Some(json!({ "ok": true }).to_string()),
                error: None,
            }),
        )),
        Err(SidecarResponseTrackerError::OwnershipMismatch {
            request_id: -10,
            expected: Box::new(OwnershipScope::vm("conn-1", "session-1", "vm-1")),
            actual: Box::new(OwnershipScope::vm("conn-1", "session-1", "vm-2")),
        }),
    );
    tracker
        .accept_response(&SidecarResponseFrame::new(
            -10,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            SidecarResponsePayload::HostCallbackResult(HostCallbackResultResponse {
                invocation_id: "invoke-10".to_string(),
                result: Some(json!({ "ok": true }).to_string()),
                error: None,
            }),
        ))
        .expect("valid sidecar response should still be pending after ownership mismatch");

    let mut tracker = SidecarResponseTracker::default();
    tracker
        .register_request(&request)
        .expect("register sidecar request again");
    assert_eq!(
        tracker.accept_response(&SidecarResponseFrame::new(
            -10,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            SidecarResponsePayload::JsBridgeResult(JsBridgeResultResponse {
                call_id: "bridge-10".to_string(),
                result: Some(json!({ "ok": true }).to_string()),
                error: None,
            }),
        )),
        Err(SidecarResponseTrackerError::ResponseKindMismatch {
            request_id: -10,
            expected: "host_callback_result".to_string(),
            actual: "js_bridge_result".to_string(),
        }),
    );
    tracker
        .accept_response(&SidecarResponseFrame::new(
            -10,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            SidecarResponsePayload::HostCallbackResult(HostCallbackResultResponse {
                invocation_id: "invoke-10".to_string(),
                result: Some(json!({ "ok": true }).to_string()),
                error: None,
            }),
        ))
        .expect("valid sidecar response should still be pending after kind mismatch");
}

#[test]
fn sidecar_response_tracker_caps_completed_entries() {
    let mut tracker = SidecarResponseTracker::with_completed_cap(3);

    for sequence in 1..=10 {
        let request_id = -sequence;
        let request = SidecarRequestFrame::new(
            request_id,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            SidecarRequestPayload::HostCallback(HostCallbackRequest {
                invocation_id: format!("invoke-{sequence}"),
                callback_key: "collection:binding".to_string(),
                input: json!({ "value": sequence }).to_string(),
                timeout_ms: 1_000,
            }),
        );
        tracker
            .register_request(&request)
            .expect("register sidecar request");
        tracker
            .accept_response(&SidecarResponseFrame::new(
                request_id,
                OwnershipScope::vm("conn-1", "session-1", "vm-1"),
                SidecarResponsePayload::HostCallbackResult(HostCallbackResultResponse {
                    invocation_id: format!("invoke-{sequence}"),
                    result: Some(json!({ "ok": true }).to_string()),
                    error: None,
                }),
            ))
            .expect("accept sidecar response");

        assert!(
            tracker.completed_count() <= 3,
            "sidecar completed set should stay bounded"
        );
    }

    assert_eq!(tracker.completed_count(), 3);
}

#[test]
fn codec_rejects_request_id_direction_mismatches() {
    let zero_request = ProtocolFrame::Request(RequestFrame::new(
        0,
        OwnershipScope::connection("conn-1"),
        RequestPayload::Authenticate(AuthenticateRequest {
            client_name: "packages/core".to_string(),
            auth_token: "signed-token".to_string(),
            protocol_version: agentos_native_sidecar::wire::PROTOCOL_VERSION,
            bridge_version: agentos_bridge::bridge_contract().version,
        }),
    ));
    assert_eq!(
        validate_frame(&zero_request),
        Err(ProtocolCodecError::InvalidRequestId)
    );

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
            expected: agentos_native_sidecar::protocol::RequestDirection::Host,
        }),
    );

    let sidecar_request = ProtocolFrame::SidecarRequest(SidecarRequestFrame::new(
        1,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        SidecarRequestPayload::HostCallback(HostCallbackRequest {
            invocation_id: "invoke-2".to_string(),
            callback_key: "collection:binding".to_string(),
            input: json!({}).to_string(),
            timeout_ms: 100,
        }),
    ));
    assert_eq!(
        validate_frame(&sidecar_request),
        Err(ProtocolCodecError::InvalidRequestDirection {
            request_id: 1,
            expected: agentos_native_sidecar::protocol::RequestDirection::Sidecar,
        }),
    );
}

#[test]
fn schema_supports_configuration_and_structured_events() {
    let frame = ProtocolFrame::Request(RequestFrame::new(
        23,
        OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        RequestPayload::ConfigureVm(agentos_native_sidecar::protocol::ConfigureVmRequest {
            mounts: vec![agentos_native_sidecar::protocol::MountDescriptor {
                guest_path: "/workspace".to_string(),
                guest_source: "host_dir".to_string(),
                guest_fstype: "host_dir".to_string(),
                read_only: false,
                plugin: agentos_native_sidecar::protocol::MountPluginDescriptor {
                    id: "host_dir".to_string(),
                    config: json!({
                        "hostPath": "/tmp/project",
                        "readOnly": false,
                    })
                    .to_string(),
                },
            }],
            software: vec![SoftwareDescriptor {
                package_name: "@rivet-dev/agentos-runtime-core".to_string(),
                root: "/pkg".to_string(),
            }],
            permissions: Some(PermissionsPolicy {
                fs: None,
                network: Some(PatternPermissionScope::PermissionMode(PermissionMode::Ask)),
                child_process: None,
                process: None,
                env: None,
                binding: None,
            }),
            module_access_cwd: None,
            instructions: vec!["keep timing mitigation enabled".to_string()],
            projected_modules: vec![ProjectedModuleDescriptor {
                package_name: "workspace".to_string(),
                entrypoint: "/workspace/index.ts".to_string(),
            }],
            command_permissions: std::collections::HashMap::new(),
            loopback_exempt_ports: Vec::new(),
            packages: Vec::new(),
            packages_mount_at: String::new(),
            bootstrap_commands: Vec::new(),
            binding_shim_commands: Vec::new(),
        }),
    ));

    validate_frame(&frame).expect("configuration request is valid");

    let event = EventFrame::new(
        OwnershipScope::session("conn-1", "session-1"),
        agentos_native_sidecar::protocol::EventPayload::Structured(StructuredEvent {
            name: "guest.lifecycle".to_string(),
            detail: std::collections::HashMap::from([(
                String::from("state"),
                String::from("ready"),
            )]),
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
        "DisposeVmRequest",
        "BootstrapRootFilesystemRequest",
        "ConfigureVmRequest",
        "RegisterHostCallbacksRequest",
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
        "GetResourceSnapshotRequest",
        "FindListenerRequest",
        "FindBoundUdpRequest",
        "GetSignalStateRequest",
        "GetZombieTimerCountRequest",
        "HostFilesystemCallRequest",
        "PersistenceLoadRequest",
        "PersistenceFlushRequest",
        "VmFetchRequest",
        "ExtEnvelope",
        "AuthenticatedResponse",
        "SessionOpenedResponse",
        "VmCreatedResponse",
        "VmDisposedResponse",
        "RootFilesystemBootstrappedResponse",
        "VmConfiguredResponse",
        "HostCallbacksRegisteredResponse",
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
        "ResourceSnapshotResponse",
        "ListenerSnapshotResponse",
        "BoundUdpSnapshotResponse",
        "SignalStateResponse",
        "ZombieTimerCountResponse",
        "FilesystemResultResponse",
        "PermissionDecisionResponse",
        "PersistenceStateResponse",
        "PersistenceFlushedResponse",
        "RejectedResponse",
        "VmFetchResponse",
        "VmLifecycleEvent",
        "ProcessOutputEvent",
        "ProcessExitedEvent",
        "StructuredEvent",
        "HostCallbackRequest",
        "JsBridgeCallRequest",
        "HostCallbackResultResponse",
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
