use crate::acp::json_rpc::{
    serialize_message, JsonRpcError, JsonRpcId, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse,
};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, oneshot, Mutex as AsyncMutex};

const DEFAULT_TIMEOUT_MS: Duration = Duration::from_millis(120_000);
const EXIT_DRAIN_GRACE_MS: Duration = Duration::from_millis(50);
const LEGACY_PERMISSION_METHOD: &str = "request/permission";
const ACP_PERMISSION_METHOD: &str = "session/request_permission";
const ACP_CANCEL_METHOD: &str = "session/cancel";
const RECENT_ACTIVITY_LIMIT: usize = 20;
const ACTIVITY_TEXT_LIMIT: usize = 240;

pub type InboundRequestFuture =
    Pin<Box<dyn Future<Output = Result<Option<InboundRequestOutcome>, String>> + Send + 'static>>;
pub type InboundRequestHandler = Arc<dyn Fn(JsonRpcRequest) -> InboundRequestFuture + Send + Sync>;

#[derive(Debug, Clone, PartialEq)]
pub struct InboundRequestOutcome {
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Clone)]
pub struct AcpClient {
    inner: Arc<AcpClientInner>,
}

#[derive(Clone)]
pub struct AcpClientOptions {
    pub timeout: Duration,
    pub request_handler: Option<InboundRequestHandler>,
}

impl Default for AcpClientOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT_MS,
            request_handler: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpClientError {
    Closed(String),
    Timeout(String),
    Io(String),
}

impl std::fmt::Display for AcpClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed(message) | Self::Timeout(message) | Self::Io(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for AcpClientError {}

struct PendingPermissionRequest {
    id: JsonRpcId,
    method: String,
    options: Option<Vec<Map<String, Value>>>,
}

struct AcpClientInner {
    writer: AsyncMutex<Pin<Box<dyn AsyncWrite + Send>>>,
    pending: Mutex<BTreeMap<JsonRpcId, oneshot::Sender<Result<JsonRpcResponse, AcpClientError>>>>,
    seen_inbound_request_ids: Mutex<BTreeSet<JsonRpcId>>,
    pending_permission_requests: Mutex<BTreeMap<String, PendingPermissionRequest>>,
    request_handler: Mutex<Option<InboundRequestHandler>>,
    notification_tx: broadcast::Sender<JsonRpcNotification>,
    recent_activity: Mutex<VecDeque<String>>,
    next_id: AtomicI64,
    closed: AtomicBool,
    transport_state: Mutex<String>,
    timeout: Duration,
}

impl AcpClient {
    pub fn new<R, W>(reader: R, writer: W, options: AcpClientOptions) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (notification_tx, _) = broadcast::channel(64);
        let inner = Arc::new(AcpClientInner {
            writer: AsyncMutex::new(Box::pin(writer)),
            pending: Mutex::new(BTreeMap::new()),
            seen_inbound_request_ids: Mutex::new(BTreeSet::new()),
            pending_permission_requests: Mutex::new(BTreeMap::new()),
            request_handler: Mutex::new(options.request_handler),
            notification_tx,
            recent_activity: Mutex::new(VecDeque::with_capacity(RECENT_ACTIVITY_LIMIT)),
            next_id: AtomicI64::new(1),
            closed: AtomicBool::new(false),
            transport_state: Mutex::new(String::from("transport_open")),
            timeout: options.timeout,
        });

        tokio::spawn(read_loop(BufReader::new(reader), Arc::clone(&inner)));

        Self { inner }
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<JsonRpcNotification> {
        self.inner.notification_tx.subscribe()
    }

    pub fn set_request_handler(&self, handler: Option<InboundRequestHandler>) {
        *self
            .inner
            .request_handler
            .lock()
            .expect("request handler lock poisoned") = handler;
    }

    pub async fn request(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse, AcpClientError> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(AcpClientError::Closed(String::from("AcpClient is closed")));
        }

        let method = method.into();
        if let Some(response) = self
            .maybe_handle_permission_response(&method, params.clone())
            .await?
        {
            return Ok(response);
        }

        let id = JsonRpcId::Number(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let message = JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: id.clone(),
            method: method.clone(),
            params: params.clone(),
        };

        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .expect("pending lock poisoned")
            .insert(id.clone(), tx);

        self.inner
            .record_activity(format!("sent request {method} id={id}"));
        if let Err(error) = self.write_message(JsonRpcMessage::Request(message)).await {
            self.inner
                .pending
                .lock()
                .expect("pending lock poisoned")
                .remove(&id);
            return Err(error);
        }

        let response = match tokio::time::timeout(self.inner.timeout, rx).await {
            Ok(Ok(Ok(response))) => response,
            Ok(Ok(Err(error))) => return Err(error),
            Ok(Err(_)) => {
                return Err(AcpClientError::Closed(String::from(
                    "ACP client request channel closed before a response arrived",
                )));
            }
            Err(_) => {
                self.inner
                    .pending
                    .lock()
                    .expect("pending lock poisoned")
                    .remove(&id);
                return Err(self.inner.create_timeout_error(&method, &id));
            }
        };

        if method != ACP_CANCEL_METHOD || !is_cancel_method_not_found(&response) {
            return Ok(response);
        }

        self.notify(method.clone(), params).await?;
        Ok(JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: response.id,
            result: Some(json!({
                "cancelled": false,
                "requested": true,
                "via": "notification-fallback",
            })),
            error: None,
        })
    }

    pub async fn notify(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<(), AcpClientError> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(AcpClientError::Closed(String::from("AcpClient is closed")));
        }

        let method = method.into();
        self.inner
            .record_activity(format!("sent notification {method}"));
        self.write_message(JsonRpcMessage::Notification(JsonRpcNotification {
            jsonrpc: String::from("2.0"),
            method,
            params,
        }))
        .await
    }

    pub async fn close(&self) -> Result<(), AcpClientError> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        {
            let mut writer = self.inner.writer.lock().await;
            writer.shutdown().await.map_err(|error| {
                AcpClientError::Io(format!("failed to close ACP writer: {error}"))
            })?;
        }
        self.inner
            .reject_all(AcpClientError::Closed(String::from("AcpClient closed")));
        Ok(())
    }

    async fn maybe_handle_permission_response(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Option<JsonRpcResponse>, AcpClientError> {
        if method != LEGACY_PERMISSION_METHOD && method != ACP_PERMISSION_METHOD {
            return Ok(None);
        }

        let payload = to_record(params);
        let permission_id = match payload.get("permissionId") {
            Some(Value::String(value)) => value.clone(),
            Some(Value::Number(value)) => value.to_string(),
            _ => return Ok(None),
        };

        let pending = self
            .inner
            .pending_permission_requests
            .lock()
            .expect("permission lock poisoned")
            .remove(&permission_id);
        let Some(pending) = pending else {
            return Ok(None);
        };
        if pending.method != ACP_PERMISSION_METHOD {
            return Ok(None);
        }

        let result = normalize_permission_result(&payload, &pending);
        let response = JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: pending.id.clone(),
            result: Some(result),
            error: None,
        };

        self.inner
            .record_activity(format!("sent permission response id={}", pending.id));
        self.write_message(JsonRpcMessage::Response(response.clone()))
            .await?;
        Ok(Some(response))
    }

    async fn write_message(&self, message: JsonRpcMessage) -> Result<(), AcpClientError> {
        let encoded = serialize_message(&message).map_err(|error| {
            AcpClientError::Io(format!("failed to serialize ACP frame: {error}"))
        })?;
        let mut writer = self.inner.writer.lock().await;
        writer
            .write_all(encoded.as_bytes())
            .await
            .map_err(|error| AcpClientError::Io(format!("failed to write ACP frame: {error}")))?;
        writer
            .flush()
            .await
            .map_err(|error| AcpClientError::Io(format!("failed to flush ACP frame: {error}")))?;
        Ok(())
    }
}

impl AcpClientInner {
    fn record_activity(&self, entry: String) {
        let mut recent = self
            .recent_activity
            .lock()
            .expect("recent activity lock poisoned");
        recent.push_back(entry);
        while recent.len() > RECENT_ACTIVITY_LIMIT {
            recent.pop_front();
        }
    }

    fn create_timeout_error(&self, method: &str, id: &JsonRpcId) -> AcpClientError {
        let transport_state = self
            .transport_state
            .lock()
            .expect("transport state lock poisoned")
            .clone();
        let recent = self
            .recent_activity
            .lock()
            .expect("recent activity lock poisoned");
        let activity = if recent.is_empty() {
            String::from("no recent ACP activity")
        } else {
            recent.iter().cloned().collect::<Vec<_>>().join(" | ")
        };
        AcpClientError::Timeout(format!(
            "ACP request {method} (id={id}) timed out after {}ms. {transport_state}. Recent ACP activity: {activity}",
            self.timeout.as_millis()
        ))
    }

    fn reject_all(&self, error: AcpClientError) {
        let responders = {
            let mut pending = self.pending.lock().expect("pending lock poisoned");
            std::mem::take(&mut *pending)
        };
        for (_, responder) in responders {
            let _ = responder.send(Err(error.clone()));
        }
        self.pending_permission_requests
            .lock()
            .expect("permission lock poisoned")
            .clear();
        self.seen_inbound_request_ids
            .lock()
            .expect("seen request ids lock poisoned")
            .clear();
    }
}

async fn read_loop<R>(reader: BufReader<R>, inner: Arc<AcpClientInner>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut lines = reader.lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let Some(message) = crate::acp::deserialize_message(trimmed) else {
                    inner.record_activity(format!("non_json {}", truncate_activity_text(trimmed)));
                    continue;
                };
                inner.record_activity(summarize_inbound_message(&message));

                match message {
                    JsonRpcMessage::Response(response) => {
                        if let Some(pending) = inner
                            .pending
                            .lock()
                            .expect("pending lock poisoned")
                            .remove(&response.id)
                        {
                            let _ = pending.send(Ok(response));
                        }
                    }
                    JsonRpcMessage::Request(request) => {
                        handle_inbound_request(Arc::clone(&inner), request).await;
                    }
                    JsonRpcMessage::Notification(notification) => {
                        let _ = inner.notification_tx.send(notification);
                    }
                }
            }
            Ok(None) => {
                *inner
                    .transport_state
                    .lock()
                    .expect("transport state lock poisoned") = String::from("transport_closed");
                inner.record_activity(String::from("process_exit transport_closed"));
                break;
            }
            Err(error) => {
                *inner
                    .transport_state
                    .lock()
                    .expect("transport state lock poisoned") = format!("transport_error {error}");
                inner.record_activity(format!("process_exit transport_error={error}"));
                break;
            }
        }
    }

    tokio::time::sleep(EXIT_DRAIN_GRACE_MS).await;
    if !inner.closed.swap(true, Ordering::SeqCst) {
        inner.reject_all(AcpClientError::Closed(String::from("Agent process exited")));
    }
}

async fn handle_inbound_request(inner: Arc<AcpClientInner>, request: JsonRpcRequest) {
    {
        let mut seen = inner
            .seen_inbound_request_ids
            .lock()
            .expect("seen request ids lock poisoned");
        if seen.contains(&request.id) {
            return;
        }
        seen.insert(request.id.clone());
    }

    if request.method == ACP_PERMISSION_METHOD {
        let params = to_record(request.params.clone());
        let permission_id = request.id.to_string();
        inner
            .pending_permission_requests
            .lock()
            .expect("permission lock poisoned")
            .insert(
                permission_id.clone(),
                PendingPermissionRequest {
                    id: request.id.clone(),
                    method: request.method.clone(),
                    options: params
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_object)
                                .cloned()
                                .collect::<Vec<_>>()
                        }),
                },
            );

        let mut notification_params = params;
        notification_params.insert(
            String::from("permissionId"),
            Value::String(permission_id.clone()),
        );
        notification_params.insert(
            String::from("_acpMethod"),
            Value::String(request.method.clone()),
        );
        let _ = inner.notification_tx.send(JsonRpcNotification {
            jsonrpc: String::from("2.0"),
            method: String::from(LEGACY_PERMISSION_METHOD),
            params: Some(Value::Object(notification_params)),
        });
        return;
    }

    let mut notification_params = to_record(request.params.clone());
    notification_params.insert(
        String::from("requestId"),
        serde_json::to_value(&request.id).expect("serialize request id"),
    );
    let _ = inner.notification_tx.send(JsonRpcNotification {
        jsonrpc: String::from("2.0"),
        method: request.method.clone(),
        params: Some(Value::Object(notification_params)),
    });

    let handler = inner
        .request_handler
        .lock()
        .expect("request handler lock poisoned")
        .clone();
    let Some(handler) = handler else {
        let response = JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: request.id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", request.method),
                data: None,
            }),
        };
        let _ = write_with_inner(&inner, JsonRpcMessage::Response(response)).await;
        return;
    };

    let response = match handler(request.clone()).await {
        Ok(Some(outcome)) if outcome.error.is_some() => JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: request.id,
            result: None,
            error: outcome.error,
        },
        Ok(Some(outcome)) => JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: request.id,
            result: Some(outcome.result.unwrap_or(Value::Null)),
            error: None,
        },
        Ok(None) => JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: request.id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", request.method),
                data: None,
            }),
        },
        Err(message) => JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id: request.id,
            result: None,
            error: Some(JsonRpcError {
                code: -32000,
                message,
                data: None,
            }),
        },
    };

    let _ = write_with_inner(&inner, JsonRpcMessage::Response(response)).await;
}

async fn write_with_inner(
    inner: &AcpClientInner,
    message: JsonRpcMessage,
) -> Result<(), AcpClientError> {
    let encoded = serialize_message(&message)
        .map_err(|error| AcpClientError::Io(format!("failed to serialize ACP frame: {error}")))?;
    let mut writer = inner.writer.lock().await;
    writer
        .write_all(encoded.as_bytes())
        .await
        .map_err(|error| AcpClientError::Io(format!("failed to write ACP frame: {error}")))?;
    writer
        .flush()
        .await
        .map_err(|error| AcpClientError::Io(format!("failed to flush ACP frame: {error}")))?;
    Ok(())
}

fn normalize_permission_result(
    params: &Map<String, Value>,
    pending: &PendingPermissionRequest,
) -> Value {
    if let Some(outcome) = params.get("outcome") {
        if outcome.is_object() {
            return json!({ "outcome": outcome });
        }
    }

    let requested_reply = params.get("reply").and_then(Value::as_str);
    if let Some(selected_option_id) =
        resolve_permission_option_id(&pending.options, requested_reply)
    {
        return json!({
            "outcome": {
                "outcome": "selected",
                "optionId": selected_option_id,
            }
        });
    }

    match requested_reply {
        Some("always") => json!({
            "outcome": {
                "outcome": "selected",
                "optionId": "allow_always",
            }
        }),
        Some("once") => json!({
            "outcome": {
                "outcome": "selected",
                "optionId": "allow_once",
            }
        }),
        Some("reject") => json!({
            "outcome": {
                "outcome": "selected",
                "optionId": "reject_once",
            }
        }),
        _ => json!({
            "outcome": {
                "outcome": "cancelled",
            }
        }),
    }
}

fn resolve_permission_option_id(
    options: &Option<Vec<Map<String, Value>>>,
    reply: Option<&str>,
) -> Option<String> {
    let reply = reply?;
    let targets = match reply {
        "always" => (["always", "allow_always"], ["allow_always"]),
        "once" => (["once", "allow_once"], ["allow_once"]),
        "reject" => (["reject", "reject_once"], ["reject_once"]),
        _ => return None,
    };

    let options = options.as_ref()?;
    let matched = options.iter().find(|option| {
        let option_id_matches = option
            .get("optionId")
            .and_then(Value::as_str)
            .map(|value| targets.0.contains(&value))
            .unwrap_or(false);
        let kind_matches = option
            .get("kind")
            .and_then(Value::as_str)
            .map(|value| targets.1.contains(&value))
            .unwrap_or(false);
        option_id_matches || kind_matches
    })?;

    matched
        .get("optionId")
        .and_then(Value::as_str)
        .map(String::from)
}

fn is_cancel_method_not_found(response: &JsonRpcResponse) -> bool {
    let Some(error) = &response.error else {
        return false;
    };
    if error.code != -32601 {
        return false;
    }

    if let Some(data) = error.data.as_ref().and_then(Value::as_object) {
        if data
            .get("method")
            .and_then(Value::as_str)
            .is_some_and(|method| method == ACP_CANCEL_METHOD)
        {
            return true;
        }
    }

    error.message.contains(ACP_CANCEL_METHOD)
}

fn to_record(value: Option<Value>) -> Map<String, Value> {
    match value {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn truncate_activity_text(value: &str) -> String {
    if value.len() <= ACTIVITY_TEXT_LIMIT {
        return String::from(value);
    }
    format!("{}...", &value[..ACTIVITY_TEXT_LIMIT])
}

fn summarize_inbound_message(message: &JsonRpcMessage) -> String {
    match message {
        JsonRpcMessage::Response(response) => match &response.error {
            Some(error) => truncate_activity_text(&format!(
                "received response id={} error={}:{}",
                response.id, error.code, error.message
            )),
            None => format!("received response id={}", response.id),
        },
        JsonRpcMessage::Request(request) => truncate_activity_text(&format!(
            "received request {} id={}",
            request.method, request.id
        )),
        JsonRpcMessage::Notification(notification) => {
            truncate_activity_text(&format!("received notification {}", notification.method))
        }
    }
}
