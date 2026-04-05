mod support;

use agent_os_sidecar::protocol::{
    CreateVmRequest, GetSignalStateRequest, GuestRuntimeKind, OwnershipScope, RequestPayload,
    ResponsePayload,
};
use support::{authenticate, create_vm, new_sidecar, open_session, request, temp_dir};

#[test]
fn sessions_and_vms_reject_cross_connection_access() {
    let mut sidecar = new_sidecar("session-isolation");
    let cwd = temp_dir("session-isolation-cwd");

    let connection_a = authenticate(&mut sidecar, "conn-a");
    let connection_b = authenticate(&mut sidecar, "conn-b");

    let session_a = open_session(&mut sidecar, 2, &connection_a);
    let session_b = open_session(&mut sidecar, 3, &connection_b);
    let (vm_a, _) = create_vm(
        &mut sidecar,
        4,
        &connection_a,
        &session_a,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let session_reject = sidecar
        .dispatch(request(
            5,
            OwnershipScope::session(&connection_b, &session_a),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                metadata: std::collections::BTreeMap::from([(
                    String::from("cwd"),
                    cwd.to_string_lossy().into_owned(),
                )]),
                root_filesystem: Default::default(),
            }),
        ))
        .expect("dispatch mismatched session create_vm");
    match session_reject.response.payload {
        ResponsePayload::Rejected(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("not owned"));
        }
        other => panic!("unexpected session rejection response: {other:?}"),
    }

    let vm_reject = sidecar
        .dispatch(request(
            6,
            OwnershipScope::vm(&connection_b, &session_b, &vm_a),
            RequestPayload::GetSignalState(GetSignalStateRequest {
                process_id: String::from("missing"),
            }),
        ))
        .expect("dispatch mismatched vm signal-state");
    match vm_reject.response.payload {
        ResponsePayload::Rejected(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("not owned"));
        }
        other => panic!("unexpected vm rejection response: {other:?}"),
    }

    let owner_signal_state = sidecar
        .dispatch(request(
            7,
            OwnershipScope::vm(&connection_a, &session_a, &vm_a),
            RequestPayload::GetSignalState(GetSignalStateRequest {
                process_id: String::from("missing"),
            }),
        ))
        .expect("dispatch owner signal-state");
    match owner_signal_state.response.payload {
        ResponsePayload::SignalState(snapshot) => {
            assert_eq!(snapshot.process_id, "missing");
            assert!(snapshot.handlers.is_empty());
        }
        other => panic!("unexpected owner signal-state response: {other:?}"),
    }
}
