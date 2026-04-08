use agent_os_sidecar::acp::{
    deserialize_message, is_request, is_response, serialize_message, JsonRpcError, JsonRpcId,
    JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};
use serde_json::json;

#[test]
fn json_rpc_codec_round_trips_all_message_shapes() {
    let request = JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: String::from("2.0"),
        id: JsonRpcId::Number(7),
        method: String::from("session/prompt"),
        params: Some(json!({ "sessionId": "session-1" })),
    });
    let response = JsonRpcMessage::Response(JsonRpcResponse {
        jsonrpc: String::from("2.0"),
        id: JsonRpcId::String(String::from("req-1")),
        result: Some(json!({ "ok": true })),
        error: None,
    });
    let notification = JsonRpcMessage::Notification(JsonRpcNotification {
        jsonrpc: String::from("2.0"),
        method: String::from("session/update"),
        params: Some(json!({ "status": "thinking" })),
    });

    let encoded_request = serialize_message(&request).expect("encode request");
    let encoded_response = serialize_message(&response).expect("encode response");
    let encoded_notification = serialize_message(&notification).expect("encode notification");

    assert_eq!(
        deserialize_message(encoded_request.trim()),
        Some(request.clone())
    );
    assert_eq!(
        deserialize_message(encoded_response.trim()),
        Some(response.clone())
    );
    assert_eq!(
        deserialize_message(encoded_notification.trim()),
        Some(notification.clone())
    );
    assert!(is_request(&request));
    assert!(is_response(&response));
    assert!(!is_request(&notification));
    assert!(!is_response(&notification));
}

#[test]
fn json_rpc_deserializer_rejects_invalid_lines() {
    assert_eq!(deserialize_message("not json"), None);
    assert_eq!(
        deserialize_message(r#"{"jsonrpc":"1.0","id":1,"method":"initialize"}"#),
        None
    );
    assert_eq!(
        deserialize_message(r#"{"jsonrpc":"2.0","result":{"ok":true}}"#),
        None
    );
}

#[test]
fn json_rpc_error_serializes_optional_data() {
    let response = JsonRpcMessage::Response(JsonRpcResponse {
        jsonrpc: String::from("2.0"),
        id: JsonRpcId::Null,
        result: None,
        error: Some(JsonRpcError {
            code: -32601,
            message: String::from("Method not found"),
            data: Some(json!({ "method": "session/cancel" })),
        }),
    });

    let encoded = serialize_message(&response).expect("encode error response");
    assert!(encoded.contains("\"data\":{\"method\":\"session/cancel\"}"));
}
