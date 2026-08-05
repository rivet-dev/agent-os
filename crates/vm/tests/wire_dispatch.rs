mod support;

use agentos_vm::wire::{
    AuthenticateRequest, ConnectionOwnership, OwnershipScope, RequestFrame, RequestPayload,
    ResponsePayload, WireFrameCodec, PROTOCOL_VERSION,
};
use support::{new_sidecar, TEST_AUTH_TOKEN};

#[test]
fn wire_frame_codec_round_trips_generated_request_frames() {
    let codec = WireFrameCodec::default();
    let frame = authenticate_request_frame();

    let encoded = codec
        .encode(&agentos_vm::wire::ProtocolFrame::RequestFrame(
            frame.clone(),
        ))
        .expect("encode wire frame");
    let decoded = codec.decode(&encoded).expect("decode wire frame");

    assert_eq!(
        decoded,
        agentos_vm::wire::ProtocolFrame::RequestFrame(frame)
    );
}

#[test]
fn native_sidecar_dispatches_generated_wire_request_frames() {
    let mut sidecar = new_sidecar("wire-dispatch");

    let result = sidecar
        .dispatch_wire_blocking(authenticate_request_frame())
        .expect("dispatch generated wire authenticate request");

    match result.response.payload {
        ResponsePayload::AuthenticatedResponse(response) => {
            assert!(!response.connection_id.is_empty());
            assert_eq!(result.events, Vec::new());
        }
        other => panic!("unexpected wire response: {other:?}"),
    }
}

fn authenticate_request_frame() -> RequestFrame {
    RequestFrame {
        schema: agentos_vm::wire::protocol_schema(),
        request_id: 1,
        ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
            connection_id: String::from("conn-1"),
        }),
        payload: RequestPayload::AuthenticateRequest(AuthenticateRequest {
            client_name: String::from("generated-wire-test"),
            auth_token: String::from(TEST_AUTH_TOKEN),
            protocol_version: PROTOCOL_VERSION,
            bridge_version: agentos_vm_host_interface::bridge_contract().version,
        }),
    }
}
