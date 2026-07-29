mod support;

use agentos_native_sidecar::wire::{
    AuthenticateRequest, CreateVmRequest, ExtEnvelope, GuestRuntimeKind, OpenSessionRequest,
    RequestPayload, ResponsePayload, RootFilesystemDescriptor, SidecarPlacement,
};
use std::collections::HashMap;
use support::{
    authenticate_wire, authenticate_wire_with_token, new_sidecar, new_sidecar_with_auth_token,
    open_session_wire, temp_dir, wire_connection, wire_request, wire_session, TEST_AUTH_TOKEN,
};

#[test]
fn authenticate_ignores_client_connection_hints_and_preserves_existing_owners() {
    let mut sidecar = new_sidecar("connection-auth");

    let connection_a = authenticate_wire(&mut sidecar, "client-a");
    let session_a = open_session_wire(&mut sidecar, 2, &connection_a);

    let auth_b = authenticate_wire_with_token(&mut sidecar, 3, &connection_a, TEST_AUTH_TOKEN);
    let connection_b = match auth_b.response.payload {
        ResponsePayload::AuthenticatedResponse(response) => {
            assert_eq!(
                auth_b.response.ownership,
                wire_connection(&response.connection_id)
            );
            assert_ne!(response.connection_id, connection_a);
            response.connection_id
        }
        other => panic!("unexpected second auth response: {other:?}"),
    };

    let cwd = temp_dir("connection-auth-cwd");
    let create_vm = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_session(&connection_b, &session_a),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::from([(String::from("cwd"), cwd.to_string_lossy().into_owned())]),
                RootFilesystemDescriptor {
                    mode: agentos_native_sidecar::wire::RootFilesystemMode::Ephemeral,
                    disable_default_base_layer: false,
                    lowers: Vec::new(),
                    bootstrap_entries: Vec::new(),
                },
                None,
            )),
        ))
        .expect("dispatch cross-connection create_vm");

    match create_vm.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("not owned"));
        }
        other => panic!("unexpected create_vm response: {other:?}"),
    }
}

#[test]
fn authenticate_rejects_invalid_auth_tokens() {
    let mut sidecar = new_sidecar_with_auth_token("connection-auth-invalid", "expected-token");

    let rejected_connection = "client-a";
    let result = authenticate_wire_with_token(&mut sidecar, 1, rejected_connection, "wrong-token");

    match result.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "unauthorized");
            assert!(response.message.contains("invalid auth token"));
        }
        other => panic!("unexpected invalid auth response: {other:?}"),
    }

    assert_rejected_auth_does_not_open_connection(&mut sidecar, 2, rejected_connection);
    assert_rejected_auth_does_not_open_connection(&mut sidecar, 3, "conn-1");
}

#[test]
fn authenticate_rejects_bridge_contract_version_mismatch() {
    let mut sidecar = new_sidecar("connection-auth-bridge-version");
    let rejected_connection = "client-a";

    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            1,
            wire_connection(rejected_connection),
            RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("bridge-version-test"),
                auth_token: String::from(TEST_AUTH_TOKEN),
                protocol_version: agentos_native_sidecar::wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version + 1,
            }),
        ))
        .expect("dispatch mismatched authenticate");

    match result.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "bridge_version_mismatch");
            assert!(response.message.contains("expected"));
            assert!(response.message.contains("got"));
        }
        other => panic!("unexpected bridge version auth response: {other:?}"),
    }

    assert_rejected_auth_does_not_open_connection(&mut sidecar, 2, rejected_connection);
    assert_rejected_auth_does_not_open_connection(&mut sidecar, 3, "conn-1");
}

#[test]
fn authenticate_rejects_protocol_version_mismatch() {
    let mut sidecar = new_sidecar("connection-auth-protocol-version");
    let rejected_connection = "client-a";

    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            1,
            wire_connection(rejected_connection),
            RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("protocol-version-test"),
                auth_token: String::from(TEST_AUTH_TOKEN),
                protocol_version: agentos_native_sidecar::wire::PROTOCOL_VERSION + 1,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        ))
        .expect("dispatch mismatched authenticate");

    match result.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "protocol_version_mismatch");
            assert!(response.message.contains("expected"));
            assert!(response.message.contains("got"));
        }
        other => panic!("unexpected protocol version auth response: {other:?}"),
    }

    assert_rejected_auth_does_not_open_connection(&mut sidecar, 2, rejected_connection);
    assert_rejected_auth_does_not_open_connection(&mut sidecar, 3, "conn-1");
}

#[test]
fn ext_requests_fail_closed_when_namespace_is_unregistered() {
    let mut sidecar = new_sidecar("connection-auth-ext");
    let connection_id = authenticate_wire(&mut sidecar, "ext-client");

    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            2,
            wire_connection(&connection_id),
            RequestPayload::ExtEnvelope(ExtEnvelope {
                namespace: "dev.rivet.agentos.test".to_string(),
                payload: b"hello-ext".to_vec(),
            }),
        ))
        .expect("dispatch ext request");

    match result.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "unknown_extension");
            assert!(response.message.contains("dev.rivet.agentos.test"));
        }
        other => panic!("unexpected ext response: {other:?}"),
    }
}

fn assert_rejected_auth_does_not_open_connection(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: i64,
    connection_id: &str,
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_connection(connection_id),
            RequestPayload::OpenSessionRequest(OpenSessionRequest {
                placement: SidecarPlacement::SidecarPlacementShared(
                    agentos_native_sidecar::wire::SidecarPlacementShared { pool: None },
                ),
                metadata: HashMap::new(),
            }),
        ))
        .expect("dispatch open session after rejected authenticate");

    match result.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("has not authenticated"));
        }
        other => panic!("unexpected post-rejection session response: {other:?}"),
    }
}
