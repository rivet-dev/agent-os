mod support;

use agent_os_sidecar::protocol::{
    GuestRuntimeKind, OwnershipScope, SidecarRequestPayload, SidecarResponseFrame,
    SidecarResponsePayload, ToolInvocationRequest, ToolInvocationResultResponse,
};
use serde_json::json;
use support::{authenticate, create_vm, new_sidecar, open_session, temp_dir};

#[test]
fn native_sidecar_tracks_sidecar_initiated_requests_and_responses() {
    let mut sidecar = new_sidecar("bidirectional-frames");
    let connection_id = authenticate(&mut sidecar, "client-hint");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &temp_dir("bidirectional-vm"),
    );

    let request_id = sidecar
        .queue_sidecar_request(
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            SidecarRequestPayload::ToolInvocation(ToolInvocationRequest {
                invocation_id: "invoke-1".to_string(),
                tool_key: "toolkit:tool".to_string(),
                input: json!({ "prompt": "ping" }),
                timeout_ms: 1_000,
            }),
        )
        .expect("queue sidecar request");
    assert_eq!(request_id, -1);

    let outbound = sidecar
        .pop_sidecar_request()
        .expect("pending outbound request");
    assert_eq!(outbound.request_id, -1);

    sidecar
        .accept_sidecar_response(SidecarResponseFrame::new(
            outbound.request_id,
            outbound.ownership.clone(),
            SidecarResponsePayload::ToolInvocationResult(ToolInvocationResultResponse {
                invocation_id: "invoke-1".to_string(),
                result: Some(json!({ "ok": true })),
                error: None,
            }),
        ))
        .expect("accept sidecar response");

    let completed = sidecar
        .take_sidecar_response(outbound.request_id)
        .expect("completed sidecar response");
    assert_eq!(completed.request_id, -1);
    assert!(matches!(
        completed.payload,
        SidecarResponsePayload::ToolInvocationResult(_)
    ));
}
