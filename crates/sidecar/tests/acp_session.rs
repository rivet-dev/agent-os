#[path = "../src/acp/mod.rs"]
mod acp;
#[path = "../src/protocol.rs"]
mod protocol;

use acp::compat::{
    is_cancel_method_not_found, maybe_normalize_permission_response,
    normalize_inbound_permission_request,
};
use acp::session::AcpSessionState;
use acp::{JsonRpcError, JsonRpcId, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Map, Value};

fn sample_init_result() -> Map<String, Value> {
    Map::from_iter([
        (
            String::from("agentInfo"),
            json!({ "name": "Mock ACP", "version": "1.0.0" }),
        ),
        (
            String::from("agentCapabilities"),
            json!({
                "permissions": true,
                "plan_mode": true,
                "tool_calls": true,
            }),
        ),
        (
            String::from("modes"),
            json!({
                "currentModeId": "build",
                "availableModes": [
                    { "id": "build", "label": "Build" },
                    { "id": "plan", "label": "Plan" },
                ],
            }),
        ),
        (
            String::from("configOptions"),
            json!([
                {
                    "id": "model-opt",
                    "category": "model",
                    "label": "Model",
                    "currentValue": "default",
                },
                {
                    "id": "thought-opt",
                    "category": "thought_level",
                    "label": "Thought Level",
                    "currentValue": "medium",
                },
            ]),
        ),
    ])
}

fn sample_session_result() -> Map<String, Value> {
    Map::from_iter([
        (String::from("sessionId"), json!("mock-agent-session")),
        (
            String::from("models"),
            json!({
                "currentModelId": "anthropic/claude-sonnet-4-20250514",
                "availableModels": [
                    {
                        "modelId": "anthropic/claude-sonnet-4-20250514",
                        "name": "Sonnet 4",
                    },
                    {
                        "modelId": "anthropic/claude-opus-4-1-20250805",
                        "name": "Opus 4.1",
                    },
                ],
            }),
        ),
    ])
}

fn session(agent_type: &str) -> AcpSessionState {
    AcpSessionState::new(
        String::from("mock-agent-session"),
        String::from("vm-1"),
        String::from(agent_type),
        String::from("acp-agent-1"),
        None,
        &sample_init_result(),
        &sample_session_result(),
    )
}

fn codex_session_with_standard_model_option() -> AcpSessionState {
    AcpSessionState::new(
        String::from("mock-agent-session"),
        String::from("vm-1"),
        String::from("codex"),
        String::from("acp-agent-1"),
        None,
        &sample_init_result(),
        &Map::from_iter([
            (String::from("sessionId"), json!("mock-agent-session")),
            (
                String::from("configOptions"),
                json!([
                    {
                        "id": "model",
                        "category": "model",
                        "label": "Model",
                        "currentValue": "gpt-5-codex",
                    },
                    {
                        "id": "thought_level",
                        "category": "thought_level",
                        "label": "Thought Level",
                        "currentValue": "medium",
                    },
                ]),
            ),
            (
                String::from("models"),
                json!({
                    "currentModelId": "gpt-5-codex",
                    "availableModels": [
                        {
                            "modelId": "gpt-5-codex",
                            "name": "Codex Default",
                        },
                        {
                            "modelId": "gpt-5.4",
                            "name": "GPT-5.4",
                        },
                    ],
                }),
            ),
        ]),
    )
}

#[test]
fn session_state_tracks_metadata_and_derived_model_option() {
    let session = session("pi");

    let created = session.created_response();
    assert_eq!(created.session_id, "mock-agent-session");
    assert_eq!(
        created.agent_info.expect("agent info")["name"],
        Value::String(String::from("Mock ACP"))
    );
    assert_eq!(
        created.modes.expect("modes")["currentModeId"],
        Value::String(String::from("build"))
    );
    assert!(created
        .config_options
        .iter()
        .any(|option| { option.get("id").and_then(Value::as_str) == Some("model") }));

    let state = session.state_response();
    assert_eq!(state.session_id, "mock-agent-session");
    assert_eq!(state.agent_type, "pi");
    assert_eq!(state.process_id, "acp-agent-1");
    assert!(!state.closed);
    assert!(state.events.is_empty());
}

#[test]
fn session_state_does_not_duplicate_existing_model_options() {
    let session = codex_session_with_standard_model_option();
    let model_options = session
        .created_response()
        .config_options
        .into_iter()
        .filter(|option| {
            option
                .get("category")
                .and_then(Value::as_str)
                .is_some_and(|category| category == "model")
        })
        .collect::<Vec<_>>();

    assert_eq!(model_options.len(), 1);
    assert_eq!(model_options[0]["id"], "model");
    assert_eq!(model_options[0]["currentValue"], "gpt-5-codex");
}

#[test]
fn permission_requests_are_normalized_and_deduped() {
    let mut session = session("pi");
    let request = JsonRpcRequest {
        jsonrpc: String::from("2.0"),
        id: JsonRpcId::Number(90),
        method: String::from("session/request_permission"),
        params: Some(json!({
            "sessionId": "mock-agent-session",
            "options": [
                { "optionId": "once", "kind": "allow_once" },
                { "optionId": "always", "kind": "allow_always" },
                { "optionId": "reject", "kind": "reject_once" },
            ],
        })),
    };

    let normalized = normalize_inbound_permission_request(
        &request,
        &mut session.seen_inbound_request_ids,
        &mut session.pending_permission_requests,
    )
    .expect("normalized permission request");
    assert_eq!(normalized.method, "request/permission");
    assert_eq!(
        normalized
            .params
            .as_ref()
            .and_then(|params| params.get("permissionId"))
            .and_then(Value::as_str),
        Some("90")
    );

    let duplicate = normalize_inbound_permission_request(
        &request,
        &mut session.seen_inbound_request_ids,
        &mut session.pending_permission_requests,
    );
    assert!(duplicate.is_none());

    session.record_notification(normalized);
    let state = session.state_response();
    assert_eq!(state.events.len(), 1);
    assert_eq!(state.events[0].sequence_number, 0);

    let (reply_id, result) = maybe_normalize_permission_response(
        "request/permission",
        Some(json!({
            "permissionId": "90",
            "reply": "always",
        })),
        &mut session.pending_permission_requests,
    )
    .expect("normalized permission reply");
    assert_eq!(reply_id, JsonRpcId::Number(90));
    assert_eq!(result["outcome"]["optionId"], "always");
}

#[test]
fn notifications_record_sequence_numbers_and_session_updates() {
    let mut session = session("pi");
    session.record_notification(JsonRpcNotification {
        jsonrpc: String::from("2.0"),
        method: String::from("session/update"),
        params: Some(json!({
            "update": {
                "sessionUpdate": "config_option_update",
                "configOptions": [
                    {
                        "id": "thought-opt",
                        "category": "thought_level",
                        "label": "Thought Level",
                        "currentValue": "high",
                    },
                ],
            },
        })),
    });
    session.record_notification(JsonRpcNotification {
        jsonrpc: String::from("2.0"),
        method: String::from("session/update"),
        params: Some(json!({
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "hello from mock agent" },
            },
        })),
    });

    let state = session.state_response();
    assert_eq!(state.events.len(), 2);
    assert_eq!(state.events[0].sequence_number, 0);
    assert_eq!(state.events[1].sequence_number, 1);
    assert_eq!(state.config_options.len(), 1);
    assert_eq!(state.config_options[0]["currentValue"], "high");
}

#[test]
fn opencode_mode_changes_inject_synthetic_session_update() {
    let mut session = session("opencode");
    let params = Map::from_iter([(String::from("modeId"), Value::String(String::from("plan")))]);

    let synthetic = session
        .apply_request_success("session/set_mode", &params, 0)
        .expect("synthetic mode update");
    assert_eq!(synthetic.method, "session/update");
    assert_eq!(
        session.state_response().modes.expect("modes")["currentModeId"],
        Value::String(String::from("plan"))
    );
}

#[test]
fn cancel_method_not_found_detects_session_cancel_response_shape() {
    let response = JsonRpcResponse {
        jsonrpc: String::from("2.0"),
        id: JsonRpcId::Number(1),
        result: None,
        error: Some(JsonRpcError {
            code: -32601,
            message: String::from("Method not found: session/cancel"),
            data: Some(json!({ "method": "session/cancel" })),
        }),
    };

    assert!(is_cancel_method_not_found(&response));
}
