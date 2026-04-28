#[path = "../src/acp/mod.rs"]
mod acp;
#[path = "../src/protocol.rs"]
mod protocol;

use acp::compat::{
    is_cancel_method_not_found, maybe_normalize_permission_response,
    normalize_inbound_permission_request,
};
use acp::session::{AcpSessionState, ACP_SESSION_EVENT_RETENTION_LIMIT};
use acp::{
    deserialize_message, serialize_message, AcpClient, AcpClientError, AcpClientOptions,
    InboundRequestHandler, InboundRequestOutcome, JsonRpcError, JsonRpcId, JsonRpcMessage,
    JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};
use serde::ser::Error as _;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{split, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream};

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

struct FailingNotification;

impl Serialize for FailingNotification {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Err(S::Error::custom(
            "intentional ACP notification serialization failure",
        ))
    }
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

struct FailOnWrite {
    inner: tokio::io::WriteHalf<DuplexStream>,
    fail_writes: Arc<AtomicBool>,
}

impl AsyncWrite for FailOnWrite {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "simulated hung-up peer",
            )));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "simulated hung-up peer",
            )));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "simulated hung-up peer",
            )));
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn new_client_with_failing_writer(
    options: AcpClientOptions,
) -> (
    AcpClient,
    tokio::io::Lines<BufReader<tokio::io::ReadHalf<DuplexStream>>>,
    tokio::io::WriteHalf<DuplexStream>,
    Arc<AtomicBool>,
) {
    let (client_stream, server_stream) = tokio::io::duplex(8 * 1024);
    let (client_reader, client_writer) = split(client_stream);
    let (server_reader, server_writer) = split(server_stream);
    let fail_writes = Arc::new(AtomicBool::new(false));
    let client = AcpClient::new(
        client_reader,
        FailOnWrite {
            inner: client_writer,
            fail_writes: Arc::clone(&fail_writes),
        },
        options,
    );
    (
        client,
        BufReader::new(server_reader).lines(),
        server_writer,
        fail_writes,
    )
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
        .expect("write raw json-rpc line");
    writer.flush().await.expect("flush raw json-rpc line");
}

async fn write_message(writer: &mut tokio::io::WriteHalf<DuplexStream>, message: &JsonRpcMessage) {
    let encoded = serialize_message(message).expect("encode json-rpc");
    write_raw(writer, &encoded).await;
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

    let state = session.state_response().expect("session state");
    assert_eq!(state.session_id, "mock-agent-session");
    assert_eq!(state.agent_type, "pi");
    assert_eq!(state.process_id, "acp-agent-1");
    assert!(!state.closed);
    assert!(state.events.is_empty());
}

#[test]
fn initialize_request_uses_requested_protocol_version_and_client_capabilities() {
    let client_capabilities = json!({
        "fs": {
            "readTextFile": true,
            "writeTextFile": false,
        },
        "terminal": false,
    });
    let request = acp::session::build_initialize_request(2, client_capabilities.clone());
    let params = request
        .params
        .expect("initialize request params")
        .as_object()
        .cloned()
        .expect("initialize params object");

    assert_eq!(request.method, "initialize");
    assert_eq!(params.get("protocolVersion"), Some(&json!(2)));
    assert_eq!(params.get("clientCapabilities"), Some(&client_capabilities));
}

#[test]
fn initialize_result_accepts_matching_protocol_version() {
    let init_result = Map::from_iter([(String::from("protocolVersion"), json!(1))]);

    assert_eq!(
        acp::session::validate_initialize_result(&init_result, 1),
        Ok(1)
    );
}

#[test]
fn initialize_result_reports_protocol_version_mismatch() {
    let init_result = Map::from_iter([(String::from("protocolVersion"), json!(2))]);

    match acp::session::validate_initialize_result(&init_result, 1) {
        Err(acp::session::AcpInitializeError::ProtocolVersionMismatch {
            requested,
            reported,
        }) => {
            assert_eq!(requested, 1);
            assert_eq!(reported, 2);
        }
        other => panic!("expected protocol version mismatch, got {other:?}"),
    }
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
    let state = session.state_response().expect("session state");
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

    let state = session.state_response().expect("session state");
    assert_eq!(state.events.len(), 2);
    assert_eq!(state.events[0].sequence_number, 0);
    assert_eq!(state.events[1].sequence_number, 1);
    assert_eq!(state.config_options.len(), 1);
    assert_eq!(state.config_options[0]["currentValue"], "high");
}

#[test]
fn session_state_event_buffer_is_bounded_and_drains_acknowledged_sequences() {
    let mut session = session("pi");

    for index in 0..10_000 {
        session.record_notification(JsonRpcNotification {
            jsonrpc: String::from("2.0"),
            method: String::from("session/update"),
            params: Some(json!({
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "text": format!("chunk-{index}") },
                },
            })),
        });
    }

    assert_eq!(session.events.len(), ACP_SESSION_EVENT_RETENTION_LIMIT);
    assert_eq!(
        session
            .events
            .front()
            .expect("retained front event")
            .sequence_number,
        10_000_u64 - ACP_SESSION_EVENT_RETENTION_LIMIT as u64
    );
    assert_eq!(
        session
            .events
            .back()
            .expect("retained back event")
            .sequence_number,
        9_999
    );

    let acknowledged = 9_500;
    let state = session
        .acknowledged_state_response(Some(acknowledged))
        .expect("acknowledged session state");

    assert!(state
        .events
        .iter()
        .all(|event| event.sequence_number > acknowledged));
    assert_eq!(
        session
            .events
            .front()
            .expect("front event after drain")
            .sequence_number,
        acknowledged + 1
    );
    assert_eq!(session.events.len(), 9_999 - acknowledged as usize);
}

#[test]
fn mode_changes_inject_synthetic_session_update_when_agent_omits_notification() {
    let mut session = session("mock-no-update-agent");
    let params = Map::from_iter([(String::from("modeId"), Value::String(String::from("plan")))]);

    let synthetic = session
        .apply_request_success("session/set_mode", &params, 0)
        .expect("mode update should succeed")
        .expect("synthetic mode update");
    assert_eq!(synthetic.method, "session/update");
    assert_eq!(
        session
            .state_response()
            .expect("session state")
            .modes
            .expect("modes")["currentModeId"],
        Value::String(String::from("plan"))
    );
}

#[test]
fn mode_changes_do_not_duplicate_existing_session_updates() {
    let mut session = session("mock-no-update-agent");
    session.record_notification(JsonRpcNotification {
        jsonrpc: String::from("2.0"),
        method: String::from("session/update"),
        params: Some(json!({
            "update": {
                "sessionUpdate": "current_mode_update",
                "currentModeId": "plan",
            },
        })),
    });

    let params = Map::from_iter([(String::from("modeId"), Value::String(String::from("plan")))]);
    let synthetic = session
        .apply_request_success("session/set_mode", &params, 0)
        .expect("mode update should succeed");

    assert!(synthetic.is_none());
    assert_eq!(
        session
            .state_response()
            .expect("session state")
            .events
            .len(),
        1
    );
}

#[test]
fn config_changes_inject_synthetic_session_update_when_agent_omits_notification() {
    let mut session = session("mock-no-update-agent");
    let params = Map::from_iter([
        (
            String::from("configId"),
            Value::String(String::from("thought-opt")),
        ),
        (String::from("value"), Value::String(String::from("high"))),
    ]);

    let synthetic = session
        .apply_request_success("session/set_config_option", &params, 0)
        .expect("config update should succeed")
        .expect("synthetic config update");
    assert_eq!(synthetic.method, "session/update");
    assert_eq!(
        synthetic.params.expect("config params")["update"]["sessionUpdate"],
        Value::String(String::from("config_option_update"))
    );
    assert_eq!(
        session
            .state_response()
            .expect("session state")
            .config_options[1]["currentValue"],
        "high"
    );
}

#[test]
fn config_changes_do_not_duplicate_existing_session_updates() {
    let mut session = session("mock-no-update-agent");
    session.record_notification(JsonRpcNotification {
        jsonrpc: String::from("2.0"),
        method: String::from("session/update"),
        params: Some(json!({
            "update": {
                "sessionUpdate": "config_option_update",
                "configOptions": [
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
                        "currentValue": "high",
                    },
                ],
            },
        })),
    });

    let params = Map::from_iter([
        (
            String::from("configId"),
            Value::String(String::from("thought-opt")),
        ),
        (String::from("value"), Value::String(String::from("high"))),
    ]);
    let synthetic = session
        .apply_request_success("session/set_config_option", &params, 0)
        .expect("config update should succeed");

    assert!(synthetic.is_none());
    assert_eq!(
        session
            .state_response()
            .expect("session state")
            .events
            .len(),
        1
    );
    assert_eq!(
        session
            .state_response()
            .expect("session state")
            .config_options[1]["currentValue"],
        "high"
    );
}

#[test]
fn config_changes_accept_non_string_values() {
    let mut session = session("mock-no-update-agent");
    let params = Map::from_iter([
        (
            String::from("configId"),
            Value::String(String::from("thought-opt")),
        ),
        (String::from("value"), Value::Bool(true)),
    ]);

    let synthetic = session
        .apply_request_success("session/set_config_option", &params, 0)
        .expect("config update should succeed")
        .expect("synthetic config update");

    assert_eq!(synthetic.method, "session/update");
    assert_eq!(
        session
            .state_response()
            .expect("session state")
            .config_options[1]["currentValue"],
        Value::Bool(true)
    );
}

#[test]
fn config_changes_return_typed_error_for_malformed_params() {
    let mut session = session("mock-no-update-agent");
    let params = Map::from_iter([
        (String::from("configId"), Value::Bool(true)),
        (String::from("value"), Value::Bool(true)),
    ]);

    let error = session
        .apply_request_success("session/set_config_option", &params, 0)
        .expect_err("malformed params should fail");
    let json_rpc = error.to_json_rpc_error("session/set_config_option");

    assert_eq!(json_rpc.code, -32602);
    assert_eq!(
        json_rpc.message,
        "Invalid params for session/set_config_option: configId must be a string"
    );
    assert_eq!(
        json_rpc.data.expect("typed error data")["kind"],
        json!("invalid_config_option_params")
    );
}

#[test]
fn config_changes_return_typed_error_for_malformed_option_entries() {
    let mut session = session("mock-no-update-agent");
    session
        .config_options
        .push(Value::String(String::from("broken")));
    let params = Map::from_iter([
        (
            String::from("configId"),
            Value::String(String::from("thought-opt")),
        ),
        (String::from("value"), Value::Bool(true)),
    ]);

    let error = session
        .apply_request_success("session/set_config_option", &params, 0)
        .expect_err("malformed config options should fail");
    let json_rpc = error.to_json_rpc_error("session/set_config_option");

    assert_eq!(json_rpc.code, -32602);
    assert_eq!(
        json_rpc.message,
        "Invalid params for session/set_config_option: config option entry 3 is malformed: expected an object"
    );
    let data = json_rpc.data.expect("typed error data");
    assert_eq!(data["kind"], json!("malformed_config_option_entry"));
    assert_eq!(data["index"], json!(3));
}

#[test]
fn acp_state_response_returns_typed_error_for_unserializable_notification_payloads() {
    let mut session = session("pi");
    session.record_notification(JsonRpcNotification {
        jsonrpc: String::from("2.0"),
        method: String::from("session/update"),
        params: Some(json!({
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "still healthy" },
            },
        })),
    });

    let error = session
        .state_response_with_test_notification(99, &FailingNotification)
        .expect_err("test notification should fail serialization");
    assert!(error
        .to_string()
        .contains("failed to serialize ACP notification"));

    let healthy = session.state_response().expect("healthy session state");
    assert_eq!(healthy.events.len(), 1);
    assert_eq!(healthy.events[0].notification["method"], "session/update");
}

#[test]
fn cancel_method_not_found_detects_session_cancel_response_shape() {
    let response = JsonRpcResponse::error_response(
        JsonRpcId::Number(1),
        JsonRpcError {
            code: -32601,
            message: String::from("Method not found: session/cancel"),
            data: Some(json!({ "method": "session/cancel" })),
        },
    );

    assert!(is_cancel_method_not_found(&response));
}

#[tokio::test(flavor = "current_thread")]
async fn acp_inbound_requests_wait_for_host_response_before_falling_back() {
    let handler: InboundRequestHandler = Arc::new(|request| {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            Ok(Some(InboundRequestOutcome {
                result: Some(json!({
                    "echo": request.params.unwrap_or(Value::Null)
                })),
                error: None,
            }))
        })
    });

    let (_client, mut reader, mut writer) = new_client(AcpClientOptions {
        timeout: Duration::from_millis(100),
        method_timeouts: BTreeMap::new(),
        request_handler: Some(handler),
        process_state_provider: None,
        max_read_line_bytes: 16 * 1024 * 1024,
    });
    let started_at = Instant::now();

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

    let response = read_message(&mut reader).await;
    assert!(started_at.elapsed() >= Duration::from_millis(20));
    assert_eq!(
        response,
        JsonRpcMessage::Response(JsonRpcResponse::success(
            JsonRpcId::Number(41),
            json!({
                "echo": {
                    "path": "/workspace/notes.txt",
                },
            }),
        ))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn acp_inbound_requests_return_method_not_found_after_handler_timeout() {
    let handler: InboundRequestHandler = Arc::new(|_request| {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(Some(InboundRequestOutcome {
                result: Some(json!({ "late": true })),
                error: None,
            }))
        })
    });

    let (_client, mut reader, mut writer) = new_client(AcpClientOptions {
        timeout: Duration::from_millis(10),
        method_timeouts: BTreeMap::new(),
        request_handler: Some(handler),
        process_state_provider: None,
        max_read_line_bytes: 16 * 1024 * 1024,
    });
    let started_at = Instant::now();

    write_message(
        &mut writer,
        &JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: JsonRpcId::Number(42),
            method: String::from("host/missing"),
            params: None,
        }),
    )
    .await;

    let response = read_message(&mut reader).await;
    assert!(started_at.elapsed() >= Duration::from_millis(10));
    assert_eq!(
        response,
        JsonRpcMessage::Response(JsonRpcResponse::error_response(
            JsonRpcId::Number(42),
            JsonRpcError {
                code: -32601,
                message: String::from("Method not found: host/missing"),
                data: None,
            },
        ))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_acp_frames_with_missing_ids_return_invalid_request_errors() {
    let (_client, mut reader, mut writer) = new_client(AcpClientOptions::default());

    write_raw(&mut writer, r#"{"jsonrpc":"2.0","result":{"ok":true}}"#).await;
    write_raw(&mut writer, "\n").await;

    let response = read_message(&mut reader).await;
    assert_eq!(
        response,
        JsonRpcMessage::Response(JsonRpcResponse::error_response(
            JsonRpcId::Null,
            JsonRpcError {
                code: -32600,
                message: String::from("Invalid Request: response is missing id"),
                data: None,
            },
        ))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn acp_response_write_failures_put_the_client_into_a_failed_state() {
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

    let (client, mut reader, mut writer, fail_writes) =
        new_client_with_failing_writer(AcpClientOptions {
            timeout: Duration::from_secs(1),
            method_timeouts: BTreeMap::new(),
            request_handler: Some(handler),
            process_state_provider: None,
            max_read_line_bytes: 16 * 1024 * 1024,
        });

    let pending_request = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request("session/prompt", Some(json!({ "sessionId": "pending" })))
                .await
        }
    });

    let outbound_request = read_message(&mut reader).await;
    match outbound_request {
        JsonRpcMessage::Request(request) => {
            assert_eq!(request.method, "session/prompt");
        }
        other => panic!("unexpected outbound request: {other:?}"),
    }

    fail_writes.store(true, Ordering::SeqCst);

    write_message(
        &mut writer,
        &JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: JsonRpcId::Number(77),
            method: String::from("fs/read_text_file"),
            params: Some(json!({ "path": "/workspace/notes.txt" })),
        }),
    )
    .await;

    let pending_error = tokio::time::timeout(Duration::from_secs(1), pending_request)
        .await
        .expect("pending request timeout")
        .expect("pending request join")
        .expect_err("pending request should fail after response write error");
    assert!(
        matches!(pending_error, AcpClientError::Io(_)),
        "unexpected pending error: {pending_error:?}"
    );
    assert!(
        pending_error
            .to_string()
            .contains("failed to write ACP frame"),
        "unexpected pending error message: {pending_error}"
    );

    let started_at = Instant::now();
    let subsequent_error = client
        .request(
            "session/prompt",
            Some(json!({ "sessionId": "after-failure" })),
        )
        .await
        .expect_err("subsequent request should fail fast");
    assert!(started_at.elapsed() < Duration::from_millis(50));
    assert!(
        matches!(subsequent_error, AcpClientError::Io(_)),
        "unexpected subsequent error: {subsequent_error:?}"
    );
    assert_eq!(subsequent_error.to_string(), pending_error.to_string());
}

#[tokio::test(flavor = "current_thread")]
async fn acp_request_method_timeout_overrides_apply_to_initialize_and_prompt() {
    let (client, mut reader, mut writer) = new_client(AcpClientOptions {
        timeout: Duration::from_millis(25),
        method_timeouts: BTreeMap::from([
            (String::from("initialize"), Duration::from_millis(5)),
            (String::from("session/prompt"), Duration::from_millis(80)),
        ]),
        request_handler: None,
        process_state_provider: None,
        max_read_line_bytes: 16 * 1024 * 1024,
    });

    let initialize_started = Instant::now();
    let initialize = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request("initialize", Some(json!({ "protocolVersion": 1 })))
                .await
        }
    });

    let initialize_request = read_message(&mut reader).await;
    match initialize_request {
        JsonRpcMessage::Request(request) => {
            assert_eq!(request.method, "initialize");
        }
        other => panic!("unexpected initialize request: {other:?}"),
    }

    let initialize_error = initialize
        .await
        .expect("initialize join")
        .expect_err("initialize should time out");
    assert!(matches!(initialize_error, AcpClientError::Timeout(_)));
    assert!(initialize_started.elapsed() < Duration::from_millis(20));
    assert!(initialize_error
        .to_string()
        .contains("ACP request initialize (id=1) timed out after 5ms"));

    let prompt = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request("session/prompt", Some(json!({ "sessionId": "long-lived" })))
                .await
        }
    });

    let prompt_request = read_message(&mut reader).await;
    let prompt_id = match prompt_request {
        JsonRpcMessage::Request(request) => {
            assert_eq!(request.method, "session/prompt");
            request.id
        }
        other => panic!("unexpected prompt request: {other:?}"),
    };

    tokio::time::sleep(Duration::from_millis(40)).await;
    write_message(
        &mut writer,
        &JsonRpcMessage::Response(JsonRpcResponse::success(
            prompt_id,
            json!({ "status": "complete" }),
        )),
    )
    .await;

    let prompt_response = prompt.await.expect("prompt join").expect("prompt response");
    assert_eq!(
        prompt_response.result(),
        Some(&json!({ "status": "complete" }))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn acp_timed_out_session_prompt_sends_cancel_and_ignores_late_response() {
    let (client, mut reader, mut writer) = new_client(AcpClientOptions {
        timeout: Duration::from_millis(20),
        method_timeouts: BTreeMap::from([(String::from("initialize"), Duration::from_millis(50))]),
        request_handler: None,
        process_state_provider: None,
        max_read_line_bytes: 16 * 1024 * 1024,
    });

    let prompt = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request(
                    "session/prompt",
                    Some(json!({ "sessionId": "session-timeout" })),
                )
                .await
        }
    });

    let outbound_request = read_message(&mut reader).await;
    let prompt_id = match outbound_request {
        JsonRpcMessage::Request(request) => {
            assert_eq!(request.method, "session/prompt");
            request.id
        }
        other => panic!("unexpected prompt request: {other:?}"),
    };

    let cancel = read_message(&mut reader).await;
    match cancel {
        JsonRpcMessage::Notification(notification) => {
            assert_eq!(notification.method, "session/cancel");
            assert_eq!(
                notification.params,
                Some(json!({ "sessionId": "session-timeout" }))
            );
        }
        other => panic!("unexpected timeout cancel frame: {other:?}"),
    }

    let prompt_error = prompt
        .await
        .expect("prompt join")
        .expect_err("prompt should time out");
    assert!(matches!(prompt_error, AcpClientError::Timeout(_)));

    write_message(
        &mut writer,
        &JsonRpcMessage::Response(JsonRpcResponse::success(
            prompt_id,
            json!({ "status": "late" }),
        )),
    )
    .await;

    let initialize = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request("initialize", Some(json!({ "protocolVersion": 1 })))
                .await
        }
    });

    let initialize_request = read_message(&mut reader).await;
    let initialize_id = match initialize_request {
        JsonRpcMessage::Request(request) => {
            assert_eq!(request.method, "initialize");
            request.id
        }
        other => panic!("unexpected initialize request: {other:?}"),
    };

    write_message(
        &mut writer,
        &JsonRpcMessage::Response(JsonRpcResponse::success(
            initialize_id,
            json!({ "protocolVersion": 1 }),
        )),
    )
    .await;

    let initialize_response = initialize
        .await
        .expect("initialize join")
        .expect("initialize response");
    assert_eq!(
        initialize_response.result(),
        Some(&json!({ "protocolVersion": 1 }))
    );
}
