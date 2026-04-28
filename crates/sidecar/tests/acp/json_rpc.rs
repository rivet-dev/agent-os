use agent_os_sidecar::acp::{
    deserialize_message, is_request, is_response, serialize_message, JsonRpcError, JsonRpcId,
    JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    JsonRpcResponseShapeError,
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
    let response = JsonRpcMessage::Response(JsonRpcResponse::success(
        JsonRpcId::String(String::from("req-1")),
        json!({ "ok": true }),
    ));
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
        Ok(request.clone())
    );
    assert_eq!(
        deserialize_message(encoded_response.trim()),
        Ok(response.clone())
    );
    assert_eq!(
        deserialize_message(encoded_notification.trim()),
        Ok(notification.clone())
    );
    assert!(is_request(&request));
    assert!(is_response(&response));
    assert!(!is_request(&notification));
    assert!(!is_response(&notification));
}

#[test]
fn json_rpc_deserializer_rejects_invalid_lines() {
    let parse_error = deserialize_message("not json").expect_err("invalid json should fail");
    assert_eq!(parse_error.code(), -32700);
    assert_eq!(parse_error.id(), &JsonRpcId::Null);

    let invalid_version = deserialize_message(r#"{"jsonrpc":"1.0","id":1,"method":"initialize"}"#)
        .expect_err("wrong jsonrpc version should fail");
    assert_eq!(invalid_version.code(), -32600);
    assert_eq!(invalid_version.id(), &JsonRpcId::Number(1));

    let missing_id = deserialize_message(r#"{"jsonrpc":"2.0","result":{"ok":true}}"#)
        .expect_err("response without id should fail");
    assert_eq!(missing_id.code(), -32600);
    assert_eq!(missing_id.id(), &JsonRpcId::Null);

    let invalid_params =
        deserialize_message(r#"{"jsonrpc":"2.0","id":9,"method":"initialize","params":"bad"}"#)
            .expect_err("non-object params should fail");
    assert_eq!(invalid_params.code(), -32600);
    assert_eq!(invalid_params.id(), &JsonRpcId::Number(9));
}

#[test]
fn json_rpc_error_serializes_optional_data() {
    let response = JsonRpcMessage::Response(JsonRpcResponse::error_response(
        JsonRpcId::Null,
        JsonRpcError {
            code: -32601,
            message: String::from("Method not found"),
            data: Some(json!({ "method": "session/cancel" })),
        },
    ));

    let encoded = serialize_message(&response).expect("encode error response");
    assert!(encoded.contains("\"data\":{\"method\":\"session/cancel\"}"));
}

#[test]
fn json_rpc_response_rejects_both_result_and_error() {
    let error = JsonRpcResponse::try_from_parts(
        String::from("2.0"),
        JsonRpcId::Number(1),
        Some(json!({ "ok": true })),
        Some(JsonRpcError {
            code: -32000,
            message: String::from("boom"),
            data: None,
        }),
    )
    .expect_err("response shape should be rejected");

    assert_eq!(error, JsonRpcResponseShapeError::BothResultAndError);
}
