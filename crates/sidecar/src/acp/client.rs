use crate::acp::compat::SeenInboundRequestIds;
use crate::acp::json_rpc::{
    serialize_message, JsonRpcError, JsonRpcId, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse,
};
use crate::acp::AcpTimeoutDiagnostics;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, oneshot, Mutex as AsyncMutex};

const DEFAULT_TIMEOUT_MS: Duration = Duration::from_millis(120_000);
const INITIALIZE_TIMEOUT_MS: Duration = Duration::from_millis(10_000);
const SESSION_NEW_TIMEOUT_MS: Duration = Duration::from_millis(30_000);
const SESSION_PROMPT_TIMEOUT_MS: Duration = Duration::from_millis(600_000);
const EXIT_DRAIN_GRACE_MS: Duration = Duration::from_millis(50);
const DEFAULT_MAX_READ_LINE_BYTES: usize = 16 * 1024 * 1024;
const LEGACY_PERMISSION_METHOD: &str = "request/permission";
const ACP_PERMISSION_METHOD: &str = "session/request_permission";
const ACP_CANCEL_METHOD: &str = "session/cancel";
const RECENT_ACTIVITY_LIMIT: usize = 20;
const ACTIVITY_TEXT_LIMIT: usize = 240;

pub type InboundRequestFuture =
    Pin<Box<dyn Future<Output = Result<Option<InboundRequestOutcome>, String>> + Send + 'static>>;
pub type InboundRequestHandler = Arc<dyn Fn(JsonRpcRequest) -> InboundRequestFuture + Send + Sync>;
pub type AcpClientProcessStateProvider =
    Arc<dyn Fn() -> AcpClientProcessState + Send + Sync + 'static>;

#[derive(Debug, Clone, PartialEq)]
pub struct InboundRequestOutcome {
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcpClientProcessState {
    pub exit_code: Option<i32>,
    pub killed: Option<bool>,
}

#[derive(Clone)]
pub struct AcpClient {
    inner: Arc<AcpClientInner>,
}

#[derive(Clone)]
pub struct AcpClientOptions {
    pub timeout: Duration,
    pub method_timeouts: BTreeMap<String, Duration>,
    pub request_handler: Option<InboundRequestHandler>,
    pub process_state_provider: Option<AcpClientProcessStateProvider>,
    pub max_read_line_bytes: usize,
}

impl Default for AcpClientOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT_MS,
            method_timeouts: AcpClient::default_method_timeouts(),
            request_handler: None,
            process_state_provider: None,
            max_read_line_bytes: DEFAULT_MAX_READ_LINE_BYTES,
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
    seen_inbound_request_ids: Mutex<SeenInboundRequestIds>,
    pending_permission_requests: Mutex<BTreeMap<String, PendingPermissionRequest>>,
    request_handler: Mutex<Option<InboundRequestHandler>>,
    notification_tx: broadcast::Sender<JsonRpcNotification>,
    recent_activity: Mutex<VecDeque<String>>,
    next_id: AtomicI64,
    closed: AtomicBool,
    terminal_error: Mutex<Option<AcpClientError>>,
    transport_state: Mutex<String>,
    timeout: Duration,
    method_timeouts: BTreeMap<String, Duration>,
    process_state_provider: Option<AcpClientProcessStateProvider>,
    max_read_line_bytes: usize,
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
            seen_inbound_request_ids: Mutex::new(SeenInboundRequestIds::default()),
            pending_permission_requests: Mutex::new(BTreeMap::new()),
            request_handler: Mutex::new(options.request_handler),
            notification_tx,
            recent_activity: Mutex::new(VecDeque::with_capacity(RECENT_ACTIVITY_LIMIT)),
            next_id: AtomicI64::new(1),
            closed: AtomicBool::new(false),
            terminal_error: Mutex::new(None),
            transport_state: Mutex::new(String::from("transport_open")),
            timeout: options.timeout,
            method_timeouts: options.method_timeouts,
            process_state_provider: options.process_state_provider,
            max_read_line_bytes: options.max_read_line_bytes,
        });

        tokio::spawn(read_loop(BufReader::new(reader), Arc::clone(&inner)));

        Self { inner }
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<JsonRpcNotification> {
        self.inner.notification_tx.subscribe()
    }

    pub fn default_method_timeouts() -> BTreeMap<String, Duration> {
        BTreeMap::from([
            (String::from("initialize"), INITIALIZE_TIMEOUT_MS),
            (String::from("session/new"), SESSION_NEW_TIMEOUT_MS),
            (String::from("session/prompt"), SESSION_PROMPT_TIMEOUT_MS),
        ])
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
        if let Some(error) = self.inner.terminal_error() {
            return Err(error);
        }

        let method = method.into();
        if let Some(response) = self
            .maybe_handle_permission_response(&method, params.clone())
            .await?
        {
            return Ok(response);
        }
        let request_timeout = self.inner.timeout_for_method(&method);

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

        let response = match tokio::time::timeout(request_timeout, rx).await {
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
                self.dispatch_timeout_cancel(&method, params.as_ref()).await;
                return Err(self
                    .inner
                    .create_timeout_error(&method, &id, request_timeout));
            }
        };

        if method != ACP_CANCEL_METHOD || !is_cancel_method_not_found(&response) {
            return Ok(response);
        }

        self.notify(method.clone(), params).await?;
        Ok(JsonRpcResponse::success(
            response.id,
            json!({
                "cancelled": false,
                "requested": true,
                "via": "notification-fallback",
            }),
        ))
    }

    pub async fn notify(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<(), AcpClientError> {
        if let Some(error) = self.inner.terminal_error() {
            return Err(error);
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
            let mut terminal_error = self
                .inner
                .terminal_error
                .lock()
                .expect("terminal error lock poisoned");
            terminal_error
                .get_or_insert_with(|| AcpClientError::Closed(String::from("AcpClient closed")));
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
        let response = JsonRpcResponse::success(pending.id.clone(), result);

        self.inner
            .record_activity(format!("sent permission response id={}", pending.id));
        self.write_message(JsonRpcMessage::Response(response.clone()))
            .await?;
        Ok(Some(response))
    }

    async fn write_message(&self, message: JsonRpcMessage) -> Result<(), AcpClientError> {
        write_with_inner(&self.inner, message).await
    }

    async fn dispatch_timeout_cancel(&self, method: &str, params: Option<&Value>) {
        let Some(cancel_params) = timeout_cancel_params(method, params) else {
            return;
        };
        let _ = self.notify(ACP_CANCEL_METHOD, Some(cancel_params)).await;
    }
}

impl AcpClientInner {
    fn terminal_error(&self) -> Option<AcpClientError> {
        self.terminal_error
            .lock()
            .expect("terminal error lock poisoned")
            .clone()
    }

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

    fn create_timeout_error(
        &self,
        method: &str,
        id: &JsonRpcId,
        timeout: Duration,
    ) -> AcpClientError {
        let transport_state = self
            .transport_state
            .lock()
            .expect("transport state lock poisoned")
            .clone();
        let recent_activity = self
            .recent_activity
            .lock()
            .expect("recent activity lock poisoned")
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let process_state = self
            .process_state_provider
            .as_ref()
            .map(|provider| provider())
            .unwrap_or_default();
        let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        let diagnostics = AcpTimeoutDiagnostics::new(
            method,
            id.clone(),
            timeout_ms,
            process_state.exit_code,
            process_state.killed,
            Some(transport_state),
            recent_activity,
        );
        AcpClientError::Timeout(diagnostics.message())
    }

    fn timeout_for_method(&self, method: &str) -> Duration {
        self.method_timeouts
            .get(method)
            .copied()
            .unwrap_or(self.timeout)
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

    fn fail_transport(&self, error: AcpClientError) -> AcpClientError {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let mut terminal_error = self
                .terminal_error
                .lock()
                .expect("terminal error lock poisoned");
            terminal_error.get_or_insert_with(|| error.clone());
            self.reject_all(error.clone());
        }
        error
    }
}

async fn read_loop<R>(mut reader: BufReader<R>, inner: Arc<AcpClientInner>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let max_read_line_bytes = inner.max_read_line_bytes;
    loop {
        match read_bounded_line(&mut reader, max_read_line_bytes).await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let message = match crate::acp::deserialize_message(trimmed) {
                    Ok(message) => message,
                    Err(error) => {
                        inner.record_activity(format!(
                            "invalid_json_rpc code={} {}",
                            error.code(),
                            truncate_activity_text(error.message())
                        ));
                        if write_with_inner(&inner, JsonRpcMessage::Response(error.to_response()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        continue;
                    }
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
                inner.fail_transport(AcpClientError::Io(format!(
                    "failed to read ACP frame: {error}"
                )));
                return;
            }
        }
    }

    tokio::time::sleep(EXIT_DRAIN_GRACE_MS).await;
    if !inner.closed.load(Ordering::SeqCst) {
        inner.fail_transport(AcpClientError::Closed(String::from("Agent process exited")));
    }
}

async fn read_bounded_line<R>(
    reader: &mut BufReader<R>,
    max_read_line_bytes: usize,
) -> std::io::Result<Option<String>>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::new();

    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }

        let (chunk, consume_len, line_complete) =
            if let Some(newline_pos) = available.iter().position(|byte| *byte == b'\n') {
                (&available[..newline_pos], newline_pos + 1, true)
            } else {
                (available, available.len(), false)
            };

        if line.len().saturating_add(chunk.len()) > max_read_line_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("ACP adapter emitted a line longer than {max_read_line_bytes} bytes"),
            ));
        }

        line.extend_from_slice(chunk);
        reader.consume(consume_len);

        if line_complete {
            break;
        }
    }

    if line.last() == Some(&b'\r') {
        line.pop();
    }

    String::from_utf8(line)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
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
        let response = method_not_found_response(&request);
        if write_with_inner(&inner, JsonRpcMessage::Response(response))
            .await
            .is_err()
        {
            return;
        }
        return;
    };

    let response = match tokio::time::timeout(inner.timeout, handler(request.clone())).await {
        Ok(result) => match result {
            Ok(Some(outcome)) if outcome.error.is_some() => JsonRpcResponse::error_response(
                request.id,
                outcome.error.expect("guard ensured error is present"),
            ),
            Ok(Some(outcome)) => {
                JsonRpcResponse::success(request.id, outcome.result.unwrap_or(Value::Null))
            }
            Ok(None) => method_not_found_response(&request),
            Err(message) => JsonRpcResponse::error_response(
                request.id,
                JsonRpcError {
                    code: -32000,
                    message,
                    data: None,
                },
            ),
        },
        Err(_) => {
            inner.record_activity(format!(
                "timed out waiting for inbound host handler {} id={}",
                request.method, request.id
            ));
            method_not_found_response(&request)
        }
    };

    if write_with_inner(&inner, JsonRpcMessage::Response(response))
        .await
        .is_err()
    {
        return;
    }
}

#[cfg(test)]
impl AcpClient {
    fn seen_inbound_request_id_count_for_tests(&self) -> usize {
        self.inner
            .seen_inbound_request_ids
            .lock()
            .expect("seen request ids lock poisoned")
            .len()
    }

    fn recent_activity_for_tests(&self) -> Vec<String> {
        self.inner
            .recent_activity
            .lock()
            .expect("recent activity lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    fn transport_state_for_tests(&self) -> String {
        self.inner
            .transport_state
            .lock()
            .expect("transport state lock poisoned")
            .clone()
    }
}

fn method_not_found_response(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::error_response(
        request.id.clone(),
        JsonRpcError {
            code: -32601,
            message: format!("Method not found: {}", request.method),
            data: None,
        },
    )
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
        .map_err(|error| {
            *inner
                .transport_state
                .lock()
                .expect("transport state lock poisoned") = format!("transport_write_error {error}");
            inner.record_activity(format!("process_exit transport_write_error={error}"));
            inner.fail_transport(AcpClientError::Io(format!(
                "failed to write ACP frame: {error}"
            )))
        })?;
    writer.flush().await.map_err(|error| {
        *inner
            .transport_state
            .lock()
            .expect("transport state lock poisoned") = format!("transport_flush_error {error}");
        inner.record_activity(format!("process_exit transport_flush_error={error}"));
        inner.fail_transport(AcpClientError::Io(format!(
            "failed to flush ACP frame: {error}"
        )))
    })?;
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
    let Some(error) = response.error() else {
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

fn timeout_cancel_params(method: &str, params: Option<&Value>) -> Option<Value> {
    if method == ACP_CANCEL_METHOD {
        return None;
    }

    let params = params?.as_object()?;
    let session_id = params.get("sessionId")?.clone();
    Some(json!({ "sessionId": session_id }))
}

fn truncate_activity_text(value: &str) -> String {
    if value.len() <= ACTIVITY_TEXT_LIMIT {
        return String::from(value);
    }
    format!("{}...", &value[..ACTIVITY_TEXT_LIMIT])
}

fn summarize_inbound_message(message: &JsonRpcMessage) -> String {
    match message {
        JsonRpcMessage::Response(response) => match response.error() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::time::timeout;

    #[tokio::test(flavor = "current_thread")]
    async fn client_seen_request_ids_stay_bounded_after_many_unique_requests() {
        let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
        let (client_reader, client_writer) = split(client_stream);
        let (server_reader, mut server_writer) = split(server_stream);
        let client = AcpClient::new(client_reader, client_writer, AcpClientOptions::default());

        let response_drain = tokio::spawn(async move {
            let mut lines = BufReader::new(server_reader).lines();
            let mut responses = 0usize;
            while responses < 100_000 {
                let line = lines
                    .next_line()
                    .await
                    .expect("read response line")
                    .expect("response line should exist");
                let message = crate::acp::deserialize_message(&line).expect("decode response");
                match message {
                    JsonRpcMessage::Response(_) => responses += 1,
                    other => {
                        panic!("unexpected outbound frame while draining responses: {other:?}")
                    }
                }
            }
        });

        for request_id in 0..100_000 {
            let message = JsonRpcMessage::Request(JsonRpcRequest {
                jsonrpc: String::from("2.0"),
                id: JsonRpcId::Number(request_id),
                method: String::from("fs/read_text_file"),
                params: Some(json!({ "path": format!("/tmp/{request_id}.txt") })),
            });
            let encoded = serialize_message(&message).expect("encode request");
            server_writer
                .write_all(encoded.as_bytes())
                .await
                .expect("write request");
        }
        server_writer.flush().await.expect("flush requests");

        response_drain.await.expect("response drain");
        assert_eq!(
            client.seen_inbound_request_id_count_for_tests(),
            crate::acp::compat::SEEN_INBOUND_REQUEST_ID_RETENTION_LIMIT
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn client_fails_when_adapter_emits_a_line_longer_than_the_configured_limit() {
        const MAX_READ_LINE_BYTES: usize = 16 * 1024 * 1024;
        const OVERSIZED_LINE_BYTES: usize = 20 * 1024 * 1024;

        let (client_stream, server_stream) = tokio::io::duplex(OVERSIZED_LINE_BYTES + 1024);
        let (client_reader, client_writer) = split(client_stream);
        let (server_reader, mut server_writer) = split(server_stream);
        let client = AcpClient::new(
            client_reader,
            client_writer,
            AcpClientOptions {
                max_read_line_bytes: MAX_READ_LINE_BYTES,
                ..AcpClientOptions::default()
            },
        );
        let mut outbound_lines = BufReader::new(server_reader).lines();

        let pending_request = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .request(
                        "session/prompt",
                        Some(json!({ "sessionId": "oversized-line" })),
                    )
                    .await
            }
        });

        let outbound_request = outbound_lines
            .next_line()
            .await
            .expect("read outbound request")
            .expect("outbound request should exist");
        let outbound_request =
            crate::acp::deserialize_message(&outbound_request).expect("decode outbound request");
        match outbound_request {
            JsonRpcMessage::Request(request) => {
                assert_eq!(request.method, "session/prompt");
            }
            other => panic!("unexpected outbound frame: {other:?}"),
        }

        let oversized_writer = tokio::spawn(async move {
            let chunk = vec![b'x'; 1024 * 1024];
            let mut remaining = OVERSIZED_LINE_BYTES;
            while remaining > 0 {
                let next = remaining.min(chunk.len());
                server_writer
                    .write_all(&chunk[..next])
                    .await
                    .map_err(|error| error.kind())?;
                remaining -= next;
            }
            server_writer
                .write_all(b"\n")
                .await
                .map_err(|error| error.kind())?;
            server_writer.flush().await.map_err(|error| error.kind())
        });

        let pending_error = timeout(Duration::from_secs(5), pending_request)
            .await
            .expect("pending request timeout")
            .expect("pending request join")
            .expect_err("oversized line should fail the client");
        assert!(
            matches!(pending_error, AcpClientError::Io(_)),
            "unexpected error: {pending_error:?}"
        );
        assert!(
            pending_error
                .to_string()
                .contains("ACP adapter emitted a line longer than"),
            "unexpected oversized-line error: {pending_error}"
        );

        let transport_state = client.transport_state_for_tests();
        assert!(transport_state.contains("transport_error"));
        assert!(
            client
                .recent_activity_for_tests()
                .iter()
                .any(|entry| entry.contains("transport_error")),
            "recent activity should capture a transport_error entry"
        );

        let subsequent_error = client
            .request(
                "session/prompt",
                Some(json!({ "sessionId": "after-oversized-line" })),
            )
            .await
            .expect_err("subsequent request should fail immediately");
        assert_eq!(subsequent_error.to_string(), pending_error.to_string());

        let oversized_writer_result = timeout(Duration::from_secs(1), oversized_writer)
            .await
            .expect("oversized writer timeout")
            .expect("oversized writer join");
        assert!(
            oversized_writer_result.is_ok()
                || oversized_writer_result == Err(std::io::ErrorKind::BrokenPipe),
            "unexpected oversized writer result: {oversized_writer_result:?}"
        );
    }
}
