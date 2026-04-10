use agent_os_sidecar::acp::{
    deserialize_message, AcpClient, AcpClientError, AcpClientOptions, AcpClientProcessState,
    InboundRequestHandler, InboundRequestOutcome, JsonRpcError, JsonRpcId, JsonRpcMessage,
    JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

fn new_client(
    options: AcpClientOptions,
) -> (
    AcpClient,
    tokio::io::Lines<BufReader<tokio::io::ReadHalf<DuplexStream>>>,
    tokio::io::WriteHalf<DuplexStream>,
) {
    let (client_stream, server_stream) = tokio::io::duplex(8 * 1024);
    let (client_reader, client_writer) = split(client_stream);
    let (server_reader, server_writer) = split(server_stream);
    let client = AcpClient::new(client_reader, client_writer, options);
    (client, BufReader::new(server_reader).lines(), server_writer)
}

async fn read_message(
    reader: &mut tokio::io::Lines<BufReader<tokio::io::ReadHalf<DuplexStream>>>,
) -> JsonRpcMessage {
    let line = reader
        .next_line()
        .await
        .expect("read line")
        .expect("line should exist");
    deserialize_message(&line).expect("decode json-rpc line")
}

async fn write_raw(writer: &mut tokio::io::WriteHalf<DuplexStream>, line: &str) {
    writer
        .write_all(line.as_bytes())
        .await
        .expect("write raw line");
    writer.flush().await.expect("flush raw line");
}

async fn write_message(writer: &mut tokio::io::WriteHalf<DuplexStream>, message: &JsonRpcMessage) {
    let encoded = agent_os_sidecar::acp::serialize_message(message).expect("encode json-rpc");
    write_raw(writer, &encoded).await;
}

async fn recv_notification(
    receiver: &mut tokio::sync::broadcast::Receiver<JsonRpcNotification>,
) -> JsonRpcNotification {
    tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("notification timeout")
        .expect("receive notification")
}

#[tokio::test(flavor = "current_thread")]
async fn client_correlates_responses_and_forwards_notifications() {
    let (client, mut reader, mut writer) = new_client(AcpClientOptions::default());
    let mut notifications = client.subscribe_notifications();

    let request_task = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request(
                    "session/prompt",
                    Some(json!({ "sessionId": "session-1", "prompt": [{ "type": "text", "text": "hi" }] })),
                )
                .await
        }
    });

    let request = read_message(&mut reader).await;
    match request {
        JsonRpcMessage::Request(message) => {
            assert_eq!(message.method, "session/prompt");
            write_message(
                &mut writer,
                &JsonRpcMessage::Notification(JsonRpcNotification {
                    jsonrpc: String::from("2.0"),
                    method: String::from("session/update"),
                    params: Some(json!({ "status": "thinking" })),
                }),
            )
            .await;
            write_message(
                &mut writer,
                &JsonRpcMessage::Response(JsonRpcResponse {
                    jsonrpc: String::from("2.0"),
                    id: message.id,
                    result: Some(json!({ "status": "complete" })),
                    error: None,
                }),
            )
            .await;
        }
        other => panic!("unexpected outbound frame: {other:?}"),
    }

    let notification = recv_notification(&mut notifications).await;
    assert_eq!(notification.method, "session/update");
    assert_eq!(notification.params, Some(json!({ "status": "thinking" })));

    let response = request_task.await.expect("request task").expect("request");
    assert_eq!(response.result, Some(json!({ "status": "complete" })));
}

#[tokio::test(flavor = "current_thread")]
async fn client_shims_modern_permission_requests_to_legacy_notifications() {
    let (client, mut reader, mut writer) = new_client(AcpClientOptions::default());
    let mut notifications = client.subscribe_notifications();

    let prompt_task = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request("session/prompt", Some(json!({ "sessionId": "session-1" })))
                .await
        }
    });

    let prompt_request = read_message(&mut reader).await;
    let prompt_id = match prompt_request {
        JsonRpcMessage::Request(request) => {
            assert_eq!(request.method, "session/prompt");
            request.id
        }
        other => panic!("unexpected prompt frame: {other:?}"),
    };

    write_message(
        &mut writer,
        &JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: JsonRpcId::String(String::from("perm-modern-1")),
            method: String::from("session/request_permission"),
            params: Some(json!({
                "sessionId": "session-1",
                "options": [
                    { "optionId": "allow_once", "kind": "allow_once" },
                    { "optionId": "allow_always", "kind": "allow_always" },
                    { "optionId": "reject_once", "kind": "reject_once" }
                ]
            })),
        }),
    )
    .await;

    let notification = recv_notification(&mut notifications).await;
    assert_eq!(notification.method, "request/permission");
    let params = notification.params.expect("permission params");
    assert_eq!(params["permissionId"], json!("perm-modern-1"));
    assert_eq!(params["_acpMethod"], json!("session/request_permission"));

    let permission_response = client
        .request(
            "request/permission",
            Some(json!({
                "permissionId": "perm-modern-1",
                "reply": "always"
            })),
        )
        .await
        .expect("permission response");
    assert_eq!(
        permission_response.result,
        Some(json!({
            "outcome": {
                "outcome": "selected",
                "optionId": "allow_always"
            }
        }))
    );

    let outbound_permission = read_message(&mut reader).await;
    match outbound_permission {
        JsonRpcMessage::Response(response) => {
            assert_eq!(
                response.id,
                JsonRpcId::String(String::from("perm-modern-1"))
            );
            assert_eq!(
                response.result,
                Some(json!({
                    "outcome": {
                        "outcome": "selected",
                        "optionId": "allow_always"
                    }
                }))
            );
        }
        other => panic!("unexpected permission response frame: {other:?}"),
    }

    write_message(
        &mut writer,
        &JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: prompt_id,
            result: Some(json!({ "status": "complete" })),
            error: None,
        }),
    )
    .await;

    let prompt_response = prompt_task
        .await
        .expect("prompt task")
        .expect("prompt response");
    assert_eq!(
        prompt_response.result,
        Some(json!({ "status": "complete" }))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn client_normalizes_opencode_style_permission_option_ids() {
    let (client, mut reader, mut writer) = new_client(AcpClientOptions::default());
    let mut notifications = client.subscribe_notifications();

    let prompt_task = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request("session/prompt", Some(json!({ "sessionId": "session-oc" })))
                .await
        }
    });

    let prompt_request = read_message(&mut reader).await;
    let prompt_id = match prompt_request {
        JsonRpcMessage::Request(request) => request.id,
        other => panic!("unexpected prompt frame: {other:?}"),
    };

    write_message(
        &mut writer,
        &JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: JsonRpcId::String(String::from("perm-opencode-1")),
            method: String::from("session/request_permission"),
            params: Some(json!({
                "sessionId": "session-oc",
                "options": [
                    { "optionId": "once", "kind": "allow_once" },
                    { "optionId": "always", "kind": "allow_always" },
                    { "optionId": "reject", "kind": "reject_once" }
                ]
            })),
        }),
    )
    .await;

    let _ = recv_notification(&mut notifications).await;

    client
        .request(
            "request/permission",
            Some(json!({
                "permissionId": "perm-opencode-1",
                "reply": "always"
            })),
        )
        .await
        .expect("permission response");

    let outbound_permission = read_message(&mut reader).await;
    match outbound_permission {
        JsonRpcMessage::Response(response) => {
            assert_eq!(
                response.result,
                Some(json!({
                    "outcome": {
                        "outcome": "selected",
                        "optionId": "always"
                    }
                }))
            );
        }
        other => panic!("unexpected permission response frame: {other:?}"),
    }

    write_message(
        &mut writer,
        &JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: prompt_id,
            result: Some(json!({ "done": true })),
            error: None,
        }),
    )
    .await;

    let prompt_response = prompt_task
        .await
        .expect("prompt task")
        .expect("prompt response");
    assert_eq!(prompt_response.result, Some(json!({ "done": true })));
}

#[tokio::test(flavor = "current_thread")]
async fn client_deduplicates_repeated_permission_request_ids() {
    let (client, mut reader, mut writer) = new_client(AcpClientOptions::default());
    let mut notifications = client.subscribe_notifications();

    let prompt_task = tokio::spawn({
        let client = client.clone();
        async move { client.request("session/prompt", Some(json!({}))).await }
    });

    let prompt_request = read_message(&mut reader).await;
    let prompt_id = match prompt_request {
        JsonRpcMessage::Request(request) => request.id,
        other => panic!("unexpected prompt frame: {other:?}"),
    };

    let permission_request = JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: String::from("2.0"),
        id: JsonRpcId::String(String::from("perm-dup-1")),
        method: String::from("session/request_permission"),
        params: Some(json!({
            "options": [
                { "optionId": "allow_once", "kind": "allow_once" },
                { "optionId": "reject_once", "kind": "reject_once" }
            ]
        })),
    });
    write_message(&mut writer, &permission_request).await;
    write_message(&mut writer, &permission_request).await;

    let notification = recv_notification(&mut notifications).await;
    assert_eq!(
        notification.params.expect("permission params")["permissionId"],
        json!("perm-dup-1")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), notifications.recv())
            .await
            .is_err()
    );

    client
        .request(
            "request/permission",
            Some(json!({
                "permissionId": "perm-dup-1",
                "reply": "once"
            })),
        )
        .await
        .expect("permission response");

    let outbound_permission = read_message(&mut reader).await;
    match outbound_permission {
        JsonRpcMessage::Response(response) => {
            assert_eq!(response.id, JsonRpcId::String(String::from("perm-dup-1")));
        }
        other => panic!("unexpected permission response frame: {other:?}"),
    }

    write_message(
        &mut writer,
        &JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: prompt_id,
            result: Some(json!({ "done": true })),
            error: None,
        }),
    )
    .await;

    let _ = prompt_task
        .await
        .expect("prompt task")
        .expect("prompt response");
}

#[tokio::test(flavor = "current_thread")]
async fn client_falls_back_to_cancel_notification_when_request_form_is_unsupported() {
    let (client, mut reader, mut writer) = new_client(AcpClientOptions::default());

    let cancel_task = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request("session/cancel", Some(json!({ "sessionId": "session-1" })))
                .await
        }
    });

    let outbound_request = read_message(&mut reader).await;
    let request_id = match outbound_request {
        JsonRpcMessage::Request(request) => {
            assert_eq!(request.method, "session/cancel");
            request.id
        }
        other => panic!("unexpected cancel request: {other:?}"),
    };

    write_message(
        &mut writer,
        &JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: request_id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: String::from("Method not found: session/cancel"),
                data: Some(json!({ "method": "session/cancel" })),
            }),
        }),
    )
    .await;

    let fallback = read_message(&mut reader).await;
    match fallback {
        JsonRpcMessage::Notification(notification) => {
            assert_eq!(notification.method, "session/cancel");
            assert_eq!(
                notification.params,
                Some(json!({ "sessionId": "session-1" }))
            );
        }
        other => panic!("unexpected fallback frame: {other:?}"),
    }

    let response = cancel_task
        .await
        .expect("cancel task")
        .expect("cancel response");
    assert_eq!(
        response.result,
        Some(json!({
            "cancelled": false,
            "requested": true,
            "via": "notification-fallback"
        }))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn client_timeout_errors_include_recent_activity() {
    let (client, mut reader, mut writer) = new_client(AcpClientOptions {
        timeout: Duration::from_millis(50),
        request_handler: None,
        process_state_provider: Some(Arc::new(|| AcpClientProcessState {
            exit_code: Some(137),
            killed: Some(true),
        })),
    });

    let request_task = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request("session/prompt", Some(json!({ "sessionId": "hang" })))
                .await
        }
    });

    let outbound_request = read_message(&mut reader).await;
    match outbound_request {
        JsonRpcMessage::Request(request) => {
            assert_eq!(request.method, "session/prompt");
        }
        other => panic!("unexpected request frame: {other:?}"),
    }

    write_raw(&mut writer, "[sandbox.require] start node:url /\n").await;
    write_message(
        &mut writer,
        &JsonRpcMessage::Notification(JsonRpcNotification {
            jsonrpc: String::from("2.0"),
            method: String::from("session/update"),
            params: Some(json!({ "status": "thinking" })),
        }),
    )
    .await;

    let error = request_task
        .await
        .expect("request task")
        .expect_err("request should time out");
    let message = error.to_string();
    assert!(message.contains("Recent ACP activity"));
    assert!(message.contains("non_json [sandbox.require] start node:url /"));
    assert!(message.contains("received notification session/update"));
    assert!(message.contains("process exitCode=137"));
    assert!(message.contains("killed=true"));
}

#[tokio::test(flavor = "current_thread")]
async fn client_waits_for_exit_drain_before_rejecting_pending_requests() {
    let (client, mut reader, mut writer) = new_client(AcpClientOptions {
        timeout: Duration::from_secs(1),
        request_handler: None,
        process_state_provider: None,
    });

    let started_at = Instant::now();
    let request_task = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request("session/prompt", Some(json!({ "sessionId": "exit" })))
                .await
        }
    });

    let outbound_request = read_message(&mut reader).await;
    match outbound_request {
        JsonRpcMessage::Request(request) => {
            assert_eq!(request.method, "session/prompt");
        }
        other => panic!("unexpected request frame: {other:?}"),
    }

    writer.shutdown().await.expect("shutdown server writer");
    drop(writer);
    drop(reader);

    let error = request_task
        .await
        .expect("request task")
        .expect_err("request should fail after exit");
    assert!(
        matches!(error, AcpClientError::Closed(_)),
        "unexpected error: {error:?}"
    );
    assert!(started_at.elapsed() >= Duration::from_millis(45));
}

#[tokio::test(flavor = "current_thread")]
async fn client_handles_inbound_requests_with_registered_handler() {
    let handler: InboundRequestHandler = Arc::new(|request| {
        Box::pin(async move {
            Ok(Some(InboundRequestOutcome {
                result: Some(json!({
                    "echo": request.params.unwrap_or(Value::Null)
                })),
                error: None,
            }))
        })
    });

    let (client, mut reader, mut writer) = new_client(AcpClientOptions {
        timeout: Duration::from_secs(1),
        request_handler: Some(handler),
        process_state_provider: None,
    });
    let mut notifications = client.subscribe_notifications();

    write_message(
        &mut writer,
        &JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: JsonRpcId::Number(41),
            method: String::from("fs/read_text_file"),
            params: Some(json!({ "path": "/workspace/notes.txt" })),
        }),
    )
    .await;

    let notification = recv_notification(&mut notifications).await;
    assert_eq!(notification.method, "fs/read_text_file");
    assert_eq!(
        notification.params,
        Some(json!({
            "path": "/workspace/notes.txt",
            "requestId": 41
        }))
    );

    let response = read_message(&mut reader).await;
    match response {
        JsonRpcMessage::Response(response) => {
            assert_eq!(response.id, JsonRpcId::Number(41));
            assert_eq!(
                response.result,
                Some(json!({
                    "echo": {
                        "path": "/workspace/notes.txt"
                    }
                }))
            );
        }
        other => panic!("unexpected inbound response frame: {other:?}"),
    }
}
