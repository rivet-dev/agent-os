use agentos_protocol::generated::v1::{
    AcpCreateSessionRequest, AcpDurableEvent, AcpDurablePermissionRequest, AcpDurableSessionEvent,
    AcpEvent, AcpOpenSessionResponse, AcpRequest, AcpResponse, AcpRuntimeKind,
    AcpSessionCreatedResponse,
};

#[test]
fn acp_protocol_round_trips_create_session() {
    let request = AcpRequest::AcpCreateSessionRequest(AcpCreateSessionRequest {
        agent_type: String::from("codex"),
        runtime: AcpRuntimeKind::JavaScript,
        cwd: String::from("/home/agentos"),
        additional_directories: vec![String::from("/tmp/reference")],
        args: vec![String::from("--model"), String::from("gpt-5")],
        env: [(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"))]
            .into_iter()
            .collect(),
        protocol_version: 1,
        client_capabilities: String::from("{}"),
        mcp_servers: String::from("{}"),
        skip_os_instructions: false,
        additional_instructions: Some(String::from("be concise")),
    });

    let encoded = serde_bare::to_vec(&request).expect("encode acp request");
    let decoded: AcpRequest = serde_bare::from_slice(&encoded).expect("decode acp request");
    assert_eq!(decoded, request);
}

#[test]
fn acp_protocol_round_trips_generic_durable_permission_event() {
    let event = AcpEvent::AcpDurableSessionEvent(AcpDurableSessionEvent {
        session_id: String::from("public-session"),
        sequence: 42,
        timestamp: String::from("2026-07-18T12:00:00.000Z"),
        event: AcpDurableEvent::AcpDurablePermissionRequest(AcpDurablePermissionRequest {
            request_id: String::from("019-public-request"),
            request: String::from(
                r#"{"sessionId":"public-session","options":[{"optionId":"once","kind":"allow_once"}],"_meta":{"preserved":true}}"#,
            ),
        }),
    });

    let encoded = serde_bare::to_vec(&event).expect("encode durable permission event");
    let decoded: AcpEvent =
        serde_bare::from_slice(&encoded).expect("decode durable permission event");
    assert_eq!(decoded, event);
}

#[test]
fn acp_protocol_round_trips_unit_open_session_response() {
    let response = AcpResponse::AcpOpenSessionResponse(AcpOpenSessionResponse { reserved: false });

    let encoded = serde_bare::to_vec(&response).expect("encode ACP open response");
    let decoded: AcpResponse = serde_bare::from_slice(&encoded).expect("decode ACP open response");
    assert_eq!(decoded, response);
}

#[test]
fn acp_protocol_round_trips_session_created_response() {
    let response = AcpResponse::AcpSessionCreatedResponse(AcpSessionCreatedResponse {
        session_id: String::from("acp-session-1"),
        pid: Some(42),
        modes: Some(String::from(r#"{"currentModeId":"default"}"#)),
        config_options: vec![String::from(r#"{"id":"model","values":["gpt-5"]}"#)],
        agent_capabilities: Some(String::from("{}")),
        agent_info: None,
    });

    let encoded = serde_bare::to_vec(&response).expect("encode acp response");
    let decoded: AcpResponse = serde_bare::from_slice(&encoded).expect("decode acp response");
    assert_eq!(decoded, response);
}
