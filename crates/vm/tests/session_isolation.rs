mod support;

use agentos_vm::wire::{
    CreateVmRequest, GetSignalStateRequest, GuestRuntimeKind, RequestPayload, ResponsePayload,
    RootFilesystemDescriptor,
};
use std::collections::HashMap;
use support::{
    authenticate_wire, create_vm_wire, new_sidecar, open_session_wire, temp_dir, wire_request,
    wire_session, wire_vm,
};

#[test]
fn sessions_and_vms_reject_cross_connection_access() {
    let mut sidecar = new_sidecar("session-isolation");
    let cwd = temp_dir("session-isolation-cwd");

    let connection_a = authenticate_wire(&mut sidecar, "conn-a");
    let connection_b = authenticate_wire(&mut sidecar, "conn-b");

    let session_a = open_session_wire(&mut sidecar, 2, &connection_a);
    let session_b = open_session_wire(&mut sidecar, 3, &connection_b);
    let session_a_other = open_session_wire(&mut sidecar, 4, &connection_a);
    let (vm_a, _) = create_vm_wire(
        &mut sidecar,
        5,
        &connection_a,
        &session_a,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let session_reject = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_session(&connection_b, &session_a),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::from([(String::from("cwd"), cwd.to_string_lossy().into_owned())]),
                RootFilesystemDescriptor {
                    mode: agentos_vm::wire::RootFilesystemMode::Ephemeral,
                    disable_default_base_layer: false,
                    lowers: Vec::new(),
                    bootstrap_entries: Vec::new(),
                },
                None,
            )),
        ))
        .expect("dispatch mismatched session create_vm");
    match session_reject.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("not owned"));
        }
        other => panic!("unexpected session rejection response: {other:?}"),
    }

    let vm_reject = sidecar
        .dispatch_wire_blocking(wire_request(
            7,
            wire_vm(&connection_b, &session_b, &vm_a),
            RequestPayload::GetSignalStateRequest(GetSignalStateRequest {
                process_id: String::from("missing"),
            }),
        ))
        .expect("dispatch mismatched vm signal-state");
    match vm_reject.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("not owned"));
        }
        other => panic!("unexpected vm rejection response: {other:?}"),
    }

    let same_connection_vm_reject = sidecar
        .dispatch_wire_blocking(wire_request(
            8,
            wire_vm(&connection_a, &session_a_other, &vm_a),
            RequestPayload::GetSignalStateRequest(GetSignalStateRequest {
                process_id: String::from("missing"),
            }),
        ))
        .expect("dispatch same-connection mismatched-session signal-state");
    match same_connection_vm_reject.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("not owned"));
        }
        other => panic!("unexpected same-connection vm rejection response: {other:?}"),
    }

    let owner_signal_state = sidecar
        .dispatch_wire_blocking(wire_request(
            9,
            wire_vm(&connection_a, &session_a, &vm_a),
            RequestPayload::GetSignalStateRequest(GetSignalStateRequest {
                process_id: String::from("missing"),
            }),
        ))
        .expect("dispatch owner signal-state");
    match owner_signal_state.response.payload {
        ResponsePayload::SignalStateResponse(snapshot) => {
            assert_eq!(snapshot.process_id, "missing");
            assert!(snapshot.handlers.is_empty());
        }
        other => panic!("unexpected owner signal-state response: {other:?}"),
    }
}
