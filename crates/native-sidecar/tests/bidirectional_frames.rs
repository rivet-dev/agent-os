mod support;

use agentos_native_sidecar::wire::{
    GuestRuntimeKind, HostCallbackRequest, HostCallbackResultResponse, OwnershipScope,
    SidecarRequestPayload, SidecarResponseFrame, SidecarResponsePayload,
};
use serde_json::json;
use support::{
    authenticate_wire, create_vm_wire, new_sidecar, open_session_wire, temp_dir, wire_vm,
};

const SIDECAR_CALLBACK_LIMIT: usize = 10_000;

fn host_callback(index: usize) -> SidecarRequestPayload {
    SidecarRequestPayload::HostCallbackRequest(HostCallbackRequest {
        invocation_id: format!("invoke-{index}"),
        callback_key: "collection:binding".to_string(),
        input: json!({ "prompt": "ping", "index": index }).to_string(),
        timeout_ms: 1_000,
    })
}

fn host_callback_response(index: usize) -> SidecarResponsePayload {
    SidecarResponsePayload::HostCallbackResultResponse(HostCallbackResultResponse {
        invocation_id: format!("invoke-{index}"),
        result: Some(json!({ "ok": true }).to_string()),
        error: None,
    })
}

fn new_vm_scope(
    name: &str,
) -> (
    agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    OwnershipScope,
) {
    let mut sidecar = new_sidecar(name);
    let connection_id = authenticate_wire(&mut sidecar, "client-hint");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &temp_dir(&format!("{name}-vm")),
    );
    (sidecar, wire_vm(&connection_id, &session_id, &vm_id))
}

#[test]
fn native_sidecar_tracks_sidecar_initiated_requests_and_responses() {
    let (mut sidecar, ownership) = new_vm_scope("bidirectional-frames");

    let request_id = sidecar
        .queue_wire_sidecar_request(ownership.clone(), host_callback(1))
        .expect("queue wire sidecar request");
    assert_eq!(request_id, -1);

    let outbound = sidecar
        .pop_wire_sidecar_request()
        .expect("pop wire sidecar request")
        .expect("pending outbound request");
    assert_eq!(outbound.request_id, -1);

    sidecar
        .accept_wire_sidecar_response(SidecarResponseFrame {
            schema: agentos_native_sidecar::wire::protocol_schema(),
            request_id: outbound.request_id,
            ownership: outbound.ownership.clone(),
            payload: host_callback_response(1),
        })
        .expect("accept wire sidecar response");

    let completed = sidecar
        .take_wire_sidecar_response(outbound.request_id)
        .expect("take wire sidecar response")
        .expect("completed sidecar response");
    assert_eq!(completed.request_id, -1);
    assert!(matches!(
        completed.payload,
        SidecarResponsePayload::HostCallbackResultResponse(_)
    ));
}

#[test]
fn native_sidecar_bounds_undrained_outbound_sidecar_requests() {
    let (mut sidecar, ownership) = new_vm_scope("bidirectional-outbound-bound");

    for index in 0..SIDECAR_CALLBACK_LIMIT {
        sidecar
            .queue_wire_sidecar_request(ownership.clone(), host_callback(index))
            .expect("queue wire sidecar request within outbound limit");
    }

    let error = sidecar
        .queue_wire_sidecar_request(ownership, host_callback(SIDECAR_CALLBACK_LIMIT))
        .expect_err("undrained outbound queue should be bounded");
    assert!(
        error
            .to_string()
            .contains("outbound sidecar request queue exceeded"),
        "unexpected outbound queue error: {error}"
    );
}

#[test]
fn native_sidecar_bounds_popped_unanswered_sidecar_requests() {
    let (mut sidecar, ownership) = new_vm_scope("bidirectional-pending-bound");

    for index in 0..SIDECAR_CALLBACK_LIMIT {
        sidecar
            .queue_wire_sidecar_request(ownership.clone(), host_callback(index))
            .expect("queue wire sidecar request within pending limit");
        sidecar
            .pop_wire_sidecar_request()
            .expect("pop wire sidecar request")
            .expect("pop queued sidecar request");
    }

    let error = sidecar
        .queue_wire_sidecar_request(ownership, host_callback(SIDECAR_CALLBACK_LIMIT))
        .expect_err("pending response tracker should be bounded");
    assert!(
        error
            .to_string()
            .contains("sidecar response tracker exceeded"),
        "unexpected pending tracker error: {error}"
    );
}

#[test]
fn native_sidecar_bounds_completed_sidecar_responses() {
    let (mut sidecar, ownership) = new_vm_scope("bidirectional-completed-bound");
    let mut oldest_request_id = None;

    for index in 0..SIDECAR_CALLBACK_LIMIT {
        let request_id = sidecar
            .queue_wire_sidecar_request(ownership.clone(), host_callback(index))
            .expect("queue wire sidecar request");
        let outbound = sidecar
            .pop_wire_sidecar_request()
            .expect("pop wire sidecar request")
            .expect("pop queued sidecar request");
        assert_eq!(outbound.request_id, request_id);
        sidecar
            .accept_wire_sidecar_response(SidecarResponseFrame {
                schema: agentos_native_sidecar::wire::protocol_schema(),
                request_id,
                ownership: ownership.clone(),
                payload: host_callback_response(index),
            })
            .expect("accept wire sidecar response");
        oldest_request_id.get_or_insert(request_id);
    }

    let rejected_request_id = sidecar
        .queue_wire_sidecar_request(ownership.clone(), host_callback(SIDECAR_CALLBACK_LIMIT))
        .expect("queue completion-pressure request");
    sidecar
        .pop_wire_sidecar_request()
        .expect("pop completion-pressure request")
        .expect("completion-pressure request must be queued");
    let error = sidecar
        .accept_wire_sidecar_response(SidecarResponseFrame {
            schema: agentos_native_sidecar::wire::protocol_schema(),
            request_id: rejected_request_id,
            ownership: ownership.clone(),
            payload: host_callback_response(SIDECAR_CALLBACK_LIMIT),
        })
        .expect_err("completed response pressure must reject without eviction");
    assert!(
        error.to_string().contains("ERR_AGENTOS_RESOURCE_LIMIT"),
        "completion pressure must return the typed resource limit: {error}"
    );

    let oldest_request_id = oldest_request_id.expect("at least one accepted response");
    assert!(
        sidecar
            .take_wire_sidecar_response(oldest_request_id)
            .expect("take retained oldest wire sidecar response")
            .is_some(),
        "completion pressure must not evict the oldest response"
    );
    sidecar
        .accept_wire_sidecar_response(SidecarResponseFrame {
            schema: agentos_native_sidecar::wire::protocol_schema(),
            request_id: rejected_request_id,
            ownership,
            payload: host_callback_response(SIDECAR_CALLBACK_LIMIT),
        })
        .expect("draining one completion permits retry");
    assert!(sidecar
        .take_wire_sidecar_response(rejected_request_id)
        .expect("take retried wire sidecar response")
        .is_some());
}
