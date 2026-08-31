use super::super::*;
use crate::state::DeferredRpcError;

const HTTP_LOOPBACK_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const VM_FETCH_STREAM_CHUNK_MAX_BYTES: usize = 64 * 1024;
const VM_FETCH_STREAM_COUNT_LIMIT: usize = 256;
type VmFetchResponseHead = (u16, String, Vec<(String, String)>, VmFetchBodyMode);

fn http_loopback_request_timeout() -> Duration {
    std::env::var(HTTP_LOOPBACK_REQUEST_TIMEOUT_MS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(HTTP_LOOPBACK_REQUEST_TIMEOUT)
}

/// Block until `fd` is readable or `deadline` passes. Returns whether it became readable.
///
/// BLOCKING: parks the calling OS thread in `poll(2)`. The unix/tcp accept and
/// udp recv callers run on the sidecar's single-thread tokio runtime, so a
/// non-zero wait stalls the whole event loop for up to `deadline` — the same
/// stall as the fixed sleeps this replaced, and only acceptable because the
/// guest net path always polls with wait == 0. Keep deadlines bounded and do
/// not add wait > 0 callers on paths that service concurrent VM traffic.

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::execution) struct JavascriptHttpListenRequest {
    pub(in crate::execution) server_id: u64,
    #[serde(default)]
    pub(in crate::execution) port: Option<u16>,
    #[serde(default)]
    pub(in crate::execution) hostname: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(in crate::execution) struct JavascriptHttpRequestOptions {
    pub(in crate::execution) method: Option<String>,
    pub(in crate::execution) headers: BTreeMap<String, Value>,
    pub(in crate::execution) body: Option<String>,
    pub(in crate::execution) reject_unauthorized: Option<bool>,
}

#[derive(Debug, Clone)]
pub(in crate::execution) struct HttpHeaderCollection {
    normalized: BTreeMap<String, Vec<String>>,
    raw_pairs: Vec<(String, String)>,
}

struct LoopbackHttpResponseWaitRequest<'a, B> {
    bridge: &'a SharedBridge<B>,
    vm_id: &'a str,
    dns: &'a VmDnsConfig,
    socket_paths: &'a JavascriptSocketPathContext,
    kernel: &'a mut SidecarKernel,
    kernel_readiness: KernelSocketReadinessRegistry,
    process: &'a mut ActiveProcess,
    request_key: (u64, u64),
    capabilities: CapabilityRegistry,
}

pub(crate) struct LoopbackHttpDispatchRequest<'a, B> {
    pub(crate) bridge: &'a SharedBridge<B>,
    pub(crate) vm_id: &'a str,
    pub(crate) dns: &'a VmDnsConfig,
    pub(crate) socket_paths: &'a JavascriptSocketPathContext,
    pub(crate) kernel: &'a mut SidecarKernel,
    pub(crate) kernel_readiness: KernelSocketReadinessRegistry,
    pub(crate) process: &'a mut ActiveProcess,
    pub(crate) server_id: u64,
    pub(crate) request_json: &'a str,
    pub(crate) capabilities: CapabilityRegistry,
}

pub(in crate::execution) fn parse_http_header_collection(
    headers: &BTreeMap<String, Value>,
    label: &str,
) -> Result<HttpHeaderCollection, SidecarError> {
    let mut normalized = BTreeMap::<String, Vec<String>>::new();
    let mut raw_pairs = Vec::new();

    for (raw_name, value) in headers {
        let normalized_name = raw_name.to_ascii_lowercase();
        let values = match value {
            Value::String(text) => vec![text.clone()],
            Value::Array(values) => values
                .iter()
                .map(|entry| {
                    entry.as_str().map(str::to_owned).ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "{label} header {raw_name} must contain only strings"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            other => {
                return Err(SidecarError::InvalidState(format!(
                    "{label} header {raw_name} must be a string or string array, received {other}"
                )));
            }
        };
        raw_pairs.extend(
            values
                .iter()
                .cloned()
                .map(|entry| (raw_name.clone(), entry)),
        );
        normalized
            .entry(normalized_name)
            .or_default()
            .extend(values);
    }

    Ok(HttpHeaderCollection {
        normalized,
        raw_pairs,
    })
}

fn http_headers_json(headers: &HttpHeaderCollection) -> Value {
    let map = headers
        .normalized
        .iter()
        .map(|(name, values)| {
            let value = if values.len() == 1 {
                Value::String(values[0].clone())
            } else {
                Value::Array(values.iter().cloned().map(Value::String).collect())
            };
            (name.clone(), value)
        })
        .collect::<Map<String, Value>>();
    Value::Object(map)
}

fn http_raw_headers_json(headers: &HttpHeaderCollection) -> Value {
    Value::Array(
        headers
            .raw_pairs
            .iter()
            .flat_map(|(name, value)| [Value::String(name.clone()), Value::String(value.clone())])
            .collect(),
    )
}

pub(in crate::execution) fn is_loopback_request_host(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    matches!(bare, "localhost" | "127.0.0.1" | "::1")
}

pub(in crate::execution) fn serialize_http_loopback_request(
    url: &Url,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
) -> Result<String, SidecarError> {
    let body_base64 = options
        .body
        .as_ref()
        .map(|body| base64::engine::general_purpose::STANDARD.encode(body.as_bytes()));
    serde_json::to_string(&json!({
        "method": options.method.clone().unwrap_or_else(|| String::from("GET")),
        "url": http_request_target(url),
        "headers": http_headers_json(headers),
        "rawHeaders": http_raw_headers_json(headers),
        "bodyBase64": body_base64,
    }))
    .map_err(|error| SidecarError::Execution(format!("ERR_AGENTOS_NODE_SYNC_RPC: {error}")))
}

fn http_request_target(url: &Url) -> String {
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    format!(
        "{path}{}",
        url.query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default()
    )
}

pub(in crate::execution) fn find_kernel_http_listener_process(
    vm: &VmState,
    port: u16,
) -> Option<String> {
    vm.active_processes
        .iter()
        .find_map(|(process_id, process)| {
            process.tcp_listeners.values().find_map(|listener| {
                let socket_id = listener.kernel_socket_id?;
                let record = vm.kernel.socket_get(socket_id)?;
                let local_addr = record
                    .local_address()
                    .and_then(|address| resolve_tcp_bind_addr(address.host(), address.port()).ok())
                    .unwrap_or_else(|| listener.guest_local_addr());
                if local_addr.port() == port && is_vm_local_http_listener_addr(local_addr.ip()) {
                    Some(process_id.to_owned())
                } else {
                    None
                }
            })
        })
}

fn is_vm_local_http_listener_addr(ip: IpAddr) -> bool {
    ip.is_loopback() || ip.is_unspecified()
}

fn serialize_kernel_http_fetch_request(
    port: u16,
    path: &str,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
    body_bytes: Option<&[u8]>,
) -> Vec<u8> {
    let method = options.method.as_deref().unwrap_or("GET");
    let path = format!("/{}", path.trim_start_matches('/'));
    let mut lines = vec![format!("{method} {path} HTTP/1.1")];
    let mut has_host = false;
    let mut has_connection = false;
    let mut has_content_length = false;
    for (name, values) in &headers.normalized {
        match name.as_str() {
            "host" => has_host = true,
            "connection" => has_connection = true,
            "content-length" => has_content_length = true,
            _ => {}
        }
        lines.push(format!("{name}: {}", values.join(", ")));
    }
    if !has_host {
        lines.push(format!("Host: 127.0.0.1:{port}"));
    }
    if !has_connection {
        lines.push(String::from("Connection: close"));
    }
    let body = body_bytes.unwrap_or_else(|| options.body.as_deref().unwrap_or("").as_bytes());
    if !has_content_length && !body.is_empty() {
        lines.push(format!("Content-Length: {}", body.len()));
    }
    lines.push(String::new());
    lines.push(String::new());

    let mut request = lines.join("\r\n").into_bytes();
    request.extend_from_slice(body);
    request
}

pub(in crate::execution) fn kernel_http_fetch_target_exit_code(
    error: &SidecarError,
) -> Option<i32> {
    let SidecarError::Execution(message) = error else {
        return None;
    };
    message
        .strip_prefix("vm.fetch target exited before responding (exit code ")?
        .strip_suffix(')')?
        .parse()
        .ok()
}

fn find_http_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_stream_response_head(
    bytes: &[u8],
    request_method: &str,
    max_response_bytes: usize,
) -> Result<VmFetchResponseHead, SidecarError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_FETCH_INVALID_RESPONSE: response headers were not UTF-8: {error}"
        ))
    })?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut status_parts = status_line.splitn(3, ' ');
    let version = status_parts.next().unwrap_or_default();
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_FETCH_INVALID_RESPONSE: invalid status line {status_line:?}"
        )));
    }
    let status = status_parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| (100..=599).contains(value))
        .ok_or_else(|| {
            SidecarError::Execution(format!(
                "ERR_AGENTOS_VM_FETCH_INVALID_RESPONSE: invalid status line {status_line:?}"
            ))
        })?;
    let status_text = status_parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    let mut content_length = None;
    let mut chunked = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(|| {
            SidecarError::Execution(format!(
                "ERR_AGENTOS_VM_FETCH_INVALID_RESPONSE: malformed header {line:?}"
            ))
        })?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        if name == "content-length" {
            let parsed = value.parse::<usize>().map_err(|error| {
                SidecarError::Execution(format!(
                    "ERR_AGENTOS_VM_FETCH_INVALID_RESPONSE: invalid content-length {value:?}: {error}"
                ))
            })?;
            if content_length
                .replace(parsed)
                .is_some_and(|prior| prior != parsed)
            {
                return Err(SidecarError::Execution(String::from(
                    "ERR_AGENTOS_VM_FETCH_INVALID_RESPONSE: conflicting content-length headers",
                )));
            }
        }
        if name == "transfer-encoding"
            && value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
        {
            chunked = true;
        }
        headers.push((name, value));
    }
    if chunked && content_length.is_some() {
        return Err(SidecarError::Execution(String::from(
            "ERR_AGENTOS_VM_FETCH_INVALID_RESPONSE: response supplied both chunked encoding and content-length",
        )));
    }
    if content_length.is_some_and(|length| length > max_response_bytes) {
        return Err(SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_FETCH_LIMIT: response content-length exceeds max_fetch_response_bytes {max_response_bytes}; raise limits.http.maxFetchResponseBytes"
        )));
    }
    let body_mode =
        if request_method.eq_ignore_ascii_case("HEAD") || matches!(status, 100..=199 | 204 | 304) {
            VmFetchBodyMode::Empty
        } else if chunked {
            VmFetchBodyMode::Chunked {
                chunk_remaining: None,
            }
        } else if let Some(remaining) = content_length {
            if remaining == 0 {
                VmFetchBodyMode::Empty
            } else {
                VmFetchBodyMode::ContentLength { remaining }
            }
        } else {
            VmFetchBodyMode::UntilClose
        };
    Ok((status, status_text, headers, body_mode))
}

fn append_decoded_stream_bytes(
    state: &mut VmFetchStreamState,
    bytes: &[u8],
) -> Result<(), SidecarError> {
    let next = state
        .response_bytes
        .checked_add(bytes.len())
        .ok_or_else(|| {
            SidecarError::Execution(String::from(
                "ERR_AGENTOS_VM_FETCH_LIMIT: streamed response byte counter overflowed",
            ))
        })?;
    if next > state.max_response_bytes {
        return Err(SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_FETCH_LIMIT: streamed response exceeds max_fetch_response_bytes {}; raise limits.http.maxFetchResponseBytes",
            state.max_response_bytes
        )));
    }
    state.response_bytes = next;
    state.decoded_buffer.extend(bytes.iter().copied());
    Ok(())
}

fn decode_stream_body(state: &mut VmFetchStreamState) -> Result<(), SidecarError> {
    loop {
        match state.body_mode {
            VmFetchBodyMode::Empty => return Ok(()),
            VmFetchBodyMode::ContentLength { remaining } => {
                if remaining == 0 {
                    state.body_mode = VmFetchBodyMode::Empty;
                    continue;
                }
                let take = remaining.min(state.raw_buffer.len());
                if take == 0 {
                    if state.peer_closed {
                        return Err(SidecarError::Execution(String::from(
                            "ERR_AGENTOS_VM_FETCH_TRUNCATED: peer closed before content-length bytes arrived",
                        )));
                    }
                    return Ok(());
                }
                let bytes: Vec<u8> = state.raw_buffer.drain(..take).collect();
                append_decoded_stream_bytes(state, &bytes)?;
                state.body_mode = if take == remaining {
                    VmFetchBodyMode::Empty
                } else {
                    VmFetchBodyMode::ContentLength {
                        remaining: remaining - take,
                    }
                };
            }
            VmFetchBodyMode::UntilClose => {
                if !state.raw_buffer.is_empty() {
                    let bytes = std::mem::take(&mut state.raw_buffer);
                    append_decoded_stream_bytes(state, &bytes)?;
                }
                if state.peer_closed {
                    state.body_mode = VmFetchBodyMode::Empty;
                }
                return Ok(());
            }
            VmFetchBodyMode::Chunked { chunk_remaining } => {
                let remaining = if let Some(remaining) = chunk_remaining {
                    remaining
                } else {
                    let Some(line_end) = state
                        .raw_buffer
                        .windows(2)
                        .position(|window| window == b"\r\n")
                    else {
                        if state.peer_closed {
                            return Err(SidecarError::Execution(String::from(
                                "ERR_AGENTOS_VM_FETCH_TRUNCATED: peer closed inside chunk header",
                            )));
                        }
                        return Ok(());
                    };
                    let line = std::str::from_utf8(&state.raw_buffer[..line_end]).map_err(|error| {
                        SidecarError::Execution(format!(
                            "ERR_AGENTOS_VM_FETCH_INVALID_RESPONSE: chunk header was not UTF-8: {error}"
                        ))
                    })?;
                    let size_text = line.split(';').next().unwrap_or_default().trim();
                    let size = usize::from_str_radix(size_text, 16).map_err(|error| {
                        SidecarError::Execution(format!(
                            "ERR_AGENTOS_VM_FETCH_INVALID_RESPONSE: invalid chunk size {size_text:?}: {error}"
                        ))
                    })?;
                    state.raw_buffer.drain(..line_end + 2);
                    if size == 0 {
                        state.body_mode = VmFetchBodyMode::Empty;
                        return Ok(());
                    }
                    size
                };
                if state.raw_buffer.len() < remaining + 2 {
                    state.body_mode = VmFetchBodyMode::Chunked {
                        chunk_remaining: Some(remaining),
                    };
                    if state.peer_closed {
                        return Err(SidecarError::Execution(String::from(
                            "ERR_AGENTOS_VM_FETCH_TRUNCATED: peer closed inside chunk body",
                        )));
                    }
                    return Ok(());
                }
                if &state.raw_buffer[remaining..remaining + 2] != b"\r\n" {
                    return Err(SidecarError::Execution(String::from(
                        "ERR_AGENTOS_VM_FETCH_INVALID_RESPONSE: chunk body was not followed by CRLF",
                    )));
                }
                let bytes: Vec<u8> = state.raw_buffer.drain(..remaining).collect();
                state.raw_buffer.drain(..2);
                append_decoded_stream_bytes(state, &bytes)?;
                state.body_mode = VmFetchBodyMode::Chunked {
                    chunk_remaining: None,
                };
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn service_host_fetch_target_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    socket_paths: &JavascriptSocketPathContext,
    kernel: &mut SidecarKernel,
    kernel_readiness: &KernelSocketReadinessRegistry,
    process: &mut ActiveProcess,
    wait: Duration,
    capabilities: &CapabilityRegistry,
) -> Result<bool, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let event = if wait.is_zero() {
        process
            .execution
            .try_poll_event()
            .map_err(|error| SidecarError::Execution(error.to_string()))?
    } else {
        process
            .execution
            .poll_event(wait)
            .await
            .map_err(|error| SidecarError::Execution(error.to_string()))?
    };
    let Some(event) = event else { return Ok(false) };

    match event {
        ActiveExecutionEvent::JavascriptSyncRpcRequest(request)
            if request.method == "net.http_wait" =>
        {
            // The listener wait intentionally remains pending until server
            // close. A nested vm.fetch pump must not steal it from the main
            // sidecar dispatcher or wait for it inline.
            process.queue_pending_execution_event(
                ActiveExecutionEvent::JavascriptSyncRpcRequest(request),
            )?;
        }
        ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
            let response = service_javascript_sync_rpc(JavascriptSyncRpcServiceRequest {
                bridge,
                vm_id,
                dns,
                socket_paths,
                kernel,
                kernel_readiness: Arc::clone(kernel_readiness),
                process,
                sync_request: &request,
                capabilities: capabilities.clone(),
            })
            .await;
            settle_nested_javascript_sync_rpc(process, &request, response).await?;
        }
        ActiveExecutionEvent::Exited(code) => {
            return Err(SidecarError::Execution(format!(
                "vm.fetch target exited before responding (exit code {code})"
            )));
        }
        other => {
            process.queue_pending_execution_event(other)?;
        }
    }
    Ok(true)
}

async fn settle_nested_javascript_sync_rpc(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
    response: Result<JavascriptSyncRpcServiceResponse, SidecarError>,
) -> Result<(), SidecarError> {
    let response = match response {
        Ok(JavascriptSyncRpcServiceResponse::Deferred {
            receiver, timeout, ..
        }) => {
            let receive = async {
                receiver.await.unwrap_or_else(|_| {
                    Err(DeferredRpcError {
                        code: String::from("ERR_AGENTOS_DEFERRED_RPC_RESPONSE_CHANNEL_CLOSED"),
                        message: format!(
                            "deferred sync RPC response channel closed for {}",
                            request.method
                        ),
                    })
                })
            };
            let result = match timeout {
                Some(timeout) => match tokio::time::timeout(timeout, receive).await {
                    Ok(result) => result,
                    Err(_) => Err(DeferredRpcError {
                        code: String::from("ERR_AGENTOS_DEFERRED_RPC_TIMEOUT"),
                        message: format!(
                            "{} deferred response timed out after {} ms",
                            request.method,
                            timeout.as_millis()
                        ),
                    }),
                },
                None => receive.await,
            };
            match result {
                Ok(value) => Ok(JavascriptSyncRpcServiceResponse::Json(value)),
                Err(error) => {
                    return process
                        .execution
                        .respond_javascript_sync_rpc_error(request.id, error.code, error.message)
                        .or_else(ignore_stale_javascript_sync_rpc_response);
                }
            }
        }
        other => other,
    };
    match response {
        Ok(result) => process
            .execution
            .respond_javascript_sync_rpc_response(request.id, result)
            .or_else(ignore_stale_javascript_sync_rpc_response),
        Err(error) => process
            .execution
            .respond_javascript_sync_rpc_error(
                request.id,
                javascript_sync_rpc_error_code(&error),
                javascript_sync_rpc_error_message(&error),
            )
            .or_else(ignore_stale_javascript_sync_rpc_response),
    }
}

async fn drain_host_fetch_target_events<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
    target_process_id: &str,
    socket_paths: &JavascriptSocketPathContext,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let mut idle_turns = 0;
    for _ in 0..64 {
        let dns = vm.dns.clone();
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let capabilities = vm.capabilities.clone();
        let Some(process) = vm.active_processes.get_mut(target_process_id) else {
            break;
        };
        let serviced = service_host_fetch_target_event(
            bridge,
            vm_id,
            &dns,
            socket_paths,
            &mut vm.kernel,
            &kernel_readiness,
            process,
            Duration::from_millis(1),
            &capabilities,
        )
        .await?;
        if !serviced {
            idle_turns += 1;
            if idle_turns >= 8 {
                break;
            }
            tokio::task::yield_now().await;
        } else {
            idle_turns = 0;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::execution) async fn dispatch_kernel_http_fetch<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
    target_process_id: &str,
    port: u16,
    path: &str,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
    body_bytes: Option<&[u8]>,
    max_fetch_response_bytes: usize,
) -> Result<String, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let socket_paths = build_javascript_socket_path_context(vm)?;
    // This is an outbound connection, so bind port zero and let the kernel
    // reserve a distinct ephemeral source port. The JavaScript listen-port
    // allocator is for servers and does not track active client sockets.
    let local_port = 0;
    let pending_capability = reserve_capability(&vm.capabilities, CapabilityKind::TcpSocket)?;

    let kernel_pid = vm
        .active_processes
        .get(target_process_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "vm.fetch target process disappeared: {target_process_id}"
            ))
        })?
        .kernel_pid;
    let socket_id = vm
        .kernel
        .socket_create(EXECUTION_DRIVER_NAME, kernel_pid, SocketSpec::tcp())
        .map_err(kernel_error)?;
    let _fetch_capability = pending_capability
        .commit(CapabilityBackend::Kernel { socket_id })
        .map_err(|error| SidecarError::Execution(error.to_string()))?;

    let result = dispatch_kernel_http_fetch_with_socket(
        bridge,
        vm_id,
        vm,
        target_process_id,
        kernel_pid,
        socket_id,
        local_port,
        port,
        path,
        options,
        headers,
        body_bytes,
        &socket_paths,
        max_fetch_response_bytes,
    )
    .await;
    let close_result = vm
        .kernel
        .socket_close(EXECUTION_DRIVER_NAME, kernel_pid, socket_id)
        .map_err(kernel_error);
    let cleanup_result = if result.is_err() {
        drain_host_fetch_target_events(bridge, vm_id, vm, target_process_id, &socket_paths).await
    } else {
        Ok(())
    };
    match (result, close_result) {
        (Ok(response), Ok(())) => cleanup_result.map(|()| response),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::execution) async fn start_kernel_http_fetch_stream<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
    target_process_id: &str,
    port: u16,
    path: &str,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
    body_bytes: Option<&[u8]>,
    max_response_bytes: usize,
) -> Result<String, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if vm.vm_fetch_streams.len() >= VM_FETCH_STREAM_COUNT_LIMIT {
        return Err(SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_FETCH_STREAM_LIMIT: VM has {} open fetch streams; close or cancel a stream before opening another (limit {})",
            vm.vm_fetch_streams.len(),
            VM_FETCH_STREAM_COUNT_LIMIT
        )));
    }
    let socket_paths = build_javascript_socket_path_context(vm)?;
    // Keep the source port kernel-owned for the lifetime of the stream. Using
    // the listen-port allocator here can return the same port to every active
    // request because client sockets are not part of its reservation table.
    let local_port = 0;
    let pending_capability = reserve_capability(&vm.capabilities, CapabilityKind::TcpSocket)?;
    let kernel_pid = vm
        .active_processes
        .get(target_process_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "vm.fetch target process disappeared: {target_process_id}"
            ))
        })?
        .kernel_pid;
    let socket_id = vm
        .kernel
        .socket_create(EXECUTION_DRIVER_NAME, kernel_pid, SocketSpec::tcp())
        .map_err(kernel_error)?;
    let capability = pending_capability
        .commit(CapabilityBackend::Kernel { socket_id })
        .map_err(|error| SidecarError::Execution(error.to_string()))?;

    let result = async {
        vm.kernel
            .socket_bind_inet(
                EXECUTION_DRIVER_NAME,
                kernel_pid,
                socket_id,
                InetSocketAddress::new("127.0.0.1", local_port),
            )
            .map_err(kernel_error)?;
        vm.kernel
            .socket_connect_inet_loopback(
                EXECUTION_DRIVER_NAME,
                kernel_pid,
                socket_id,
                InetSocketAddress::new("127.0.0.1", port),
            )
            .map_err(kernel_error)?;
        let request_bytes =
            serialize_kernel_http_fetch_request(port, path, options, headers, body_bytes);
        vm.kernel
            .socket_write(EXECUTION_DRIVER_NAME, kernel_pid, socket_id, &request_bytes)
            .map_err(kernel_error)?;

        let deadline = Instant::now() + http_loopback_request_timeout();
        let mut response_buffer = Vec::new();
        let mut peer_closed = false;
        let (status, status_text, response_headers, body_mode) = loop {
            if let Some(header_end) = find_http_header_end(&response_buffer) {
                let parsed = parse_stream_response_head(
                    &response_buffer[..header_end],
                    options.method.as_deref().unwrap_or("GET"),
                    max_response_bytes,
                )?;
                if (100..200).contains(&parsed.0) && parsed.0 != 101 {
                    response_buffer.drain(..header_end + 4);
                    continue;
                }
                response_buffer.drain(..header_end + 4);
                break parsed;
            }
            if Instant::now() >= deadline {
                return Err(SidecarError::Execution(format!(
                    "ERR_AGENTOS_VM_FETCH_TIMEOUT: timed out waiting for response headers after {} ms; raise AGENTOS_HTTP_LOOPBACK_REQUEST_TIMEOUT_MS",
                    http_loopback_request_timeout().as_millis()
                )));
            }
            {
                let dns = vm.dns.clone();
                let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                let capabilities = vm.capabilities.clone();
                let process = vm.active_processes.get_mut(target_process_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "vm.fetch target process disappeared: {target_process_id}"
                    ))
                })?;
                service_host_fetch_target_event(
                    bridge,
                    vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    &kernel_readiness,
                    process,
                    Duration::ZERO,
                    &capabilities,
                )
                .await?;
            }
            let poll = vm
                .kernel
                .poll_targets(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    vec![PollTargetEntry::socket(socket_id, POLLIN | POLLHUP | POLLERR)],
                    0,
                )
                .map_err(kernel_error)?;
            let revents = poll
                .targets
                .first()
                .map(|entry| entry.revents)
                .unwrap_or_else(PollEvents::empty);
            if revents.intersects(POLLERR) {
                return Err(SidecarError::Execution(String::from(
                    "ERR_AGENTOS_VM_FETCH_SOCKET: kernel TCP socket reported POLLERR",
                )));
            }
            if revents.intersects(POLLIN) {
                loop {
                    match vm
                        .kernel
                        .socket_read(EXECUTION_DRIVER_NAME, kernel_pid, socket_id, 64 * 1024)
                    {
                        Ok(Some(bytes)) if !bytes.is_empty() => {
                            response_buffer.extend(bytes);
                            ensure_vm_fetch_raw_response_buffer_within_limit(
                                response_buffer.len(),
                                "vm.fetchStream",
                            )
                            .map_err(sidecar_core_execution_error)?;
                        }
                        Ok(Some(_)) => break,
                        Ok(None) => {
                            peer_closed = true;
                            break;
                        }
                        Err(error) if error.code() == "EAGAIN" => break,
                        Err(error) => return Err(kernel_error(error)),
                    }
                }
            }
            if revents.intersects(POLLHUP) {
                peer_closed = true;
            }
            if peer_closed && find_http_header_end(&response_buffer).is_none() {
                return Err(SidecarError::Execution(String::from(
                    "ERR_AGENTOS_VM_FETCH_TRUNCATED: peer closed before response headers completed",
                )));
            }
            tokio::task::yield_now().await;
        };

        vm.next_vm_fetch_stream_id = vm.next_vm_fetch_stream_id.wrapping_add(1);
        let stream_id = format!("{}:{}", vm.generation, vm.next_vm_fetch_stream_id);
        let mut state = VmFetchStreamState {
            target_process_id: target_process_id.to_owned(),
            kernel_pid,
            socket_id,
            _capability: capability,
            raw_buffer: response_buffer,
            decoded_buffer: VecDeque::new(),
            body_mode,
            peer_closed,
            response_bytes: 0,
            max_response_bytes,
            last_progress_at: Instant::now(),
        };
        decode_stream_body(&mut state)?;
        vm.vm_fetch_streams.insert(stream_id.clone(), state);
        serde_json::to_string(&json!({
            "streamId": stream_id,
            "status": status,
            "statusText": status_text,
            "headers": response_headers,
        }))
        .map_err(|error| SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_FETCH_SERIALIZE: failed to serialize response head: {error}"
        )))
    }
    .await;

    if result.is_err() {
        let _ = vm
            .kernel
            .socket_close(EXECUTION_DRIVER_NAME, kernel_pid, socket_id);
    }
    result
}

async fn close_fetch_stream_socket<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
    state: VmFetchStreamState,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let target_process_id = state.target_process_id.clone();
    let close_result = vm
        .kernel
        .socket_close(EXECUTION_DRIVER_NAME, state.kernel_pid, state.socket_id)
        .map_err(kernel_error);
    drop(state);
    let socket_paths = build_javascript_socket_path_context(vm)?;
    let cleanup_result =
        drain_host_fetch_target_events(bridge, vm_id, vm, &target_process_id, &socket_paths).await;
    close_result.and(cleanup_result)
}

pub(in crate::execution) async fn read_kernel_http_fetch_stream<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
    stream_id: &str,
    requested_max_bytes: usize,
) -> Result<String, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let max_bytes = requested_max_bytes.clamp(1, VM_FETCH_STREAM_CHUNK_MAX_BYTES);
    let mut state = vm.vm_fetch_streams.remove(stream_id).ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "ERR_AGENTOS_VM_FETCH_STREAM_NOT_FOUND: stream {stream_id:?} is closed or unknown"
        ))
    })?;
    let result = async {
        decode_stream_body(&mut state)?;
        while state.decoded_buffer.is_empty()
            && !matches!(state.body_mode, VmFetchBodyMode::Empty)
        {
            if state.last_progress_at.elapsed() >= http_loopback_request_timeout() {
                return Err(SidecarError::Execution(format!(
                    "ERR_AGENTOS_VM_FETCH_TIMEOUT: stream produced no data for {} ms; raise AGENTOS_HTTP_LOOPBACK_REQUEST_TIMEOUT_MS",
                    http_loopback_request_timeout().as_millis()
                )));
            }
            let socket_paths = build_javascript_socket_path_context(vm)?;
            {
                let dns = vm.dns.clone();
                let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                let capabilities = vm.capabilities.clone();
                let process = vm
                    .active_processes
                    .get_mut(&state.target_process_id)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "vm.fetch target process disappeared: {}",
                            state.target_process_id
                        ))
                    })?;
                service_host_fetch_target_event(
                    bridge,
                    vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    &kernel_readiness,
                    process,
                    Duration::ZERO,
                    &capabilities,
                )
                .await?;
            }
            let poll = vm
                .kernel
                .poll_targets(
                    EXECUTION_DRIVER_NAME,
                    state.kernel_pid,
                    vec![PollTargetEntry::socket(
                        state.socket_id,
                        POLLIN | POLLHUP | POLLERR,
                    )],
                    0,
                )
                .map_err(kernel_error)?;
            let revents = poll
                .targets
                .first()
                .map(|entry| entry.revents)
                .unwrap_or_else(PollEvents::empty);
            if revents.intersects(POLLERR) {
                return Err(SidecarError::Execution(String::from(
                    "ERR_AGENTOS_VM_FETCH_SOCKET: kernel TCP stream reported POLLERR",
                )));
            }
            let before = state.raw_buffer.len();
            if revents.intersects(POLLIN) {
                loop {
                    match vm.kernel.socket_read(
                        EXECUTION_DRIVER_NAME,
                        state.kernel_pid,
                        state.socket_id,
                        VM_FETCH_STREAM_CHUNK_MAX_BYTES,
                    ) {
                        Ok(Some(bytes)) if !bytes.is_empty() => {
                            state.raw_buffer.extend(bytes);
                            ensure_vm_fetch_raw_response_buffer_within_limit(
                                state.raw_buffer.len(),
                                "vm.fetchStream",
                            )
                            .map_err(sidecar_core_execution_error)?;
                        }
                        Ok(Some(_)) => break,
                        Ok(None) => {
                            state.peer_closed = true;
                            break;
                        }
                        Err(error) if error.code() == "EAGAIN" => break,
                        Err(error) => return Err(kernel_error(error)),
                    }
                }
            }
            if revents.intersects(POLLHUP) {
                state.peer_closed = true;
            }
            if state.raw_buffer.len() != before || state.peer_closed {
                state.last_progress_at = Instant::now();
            }
            decode_stream_body(&mut state)?;
            if state.decoded_buffer.is_empty()
                && !matches!(state.body_mode, VmFetchBodyMode::Empty)
            {
                tokio::task::yield_now().await;
            }
        }
        let take = max_bytes.min(state.decoded_buffer.len());
        let body: Vec<u8> = state.decoded_buffer.drain(..take).collect();
        let done = state.decoded_buffer.is_empty()
            && matches!(state.body_mode, VmFetchBodyMode::Empty);
        let response = serde_json::to_string(&json!({
            "body": base64::engine::general_purpose::STANDARD.encode(body),
            "done": done,
        }))
        .map_err(|error| SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_FETCH_SERIALIZE: failed to serialize stream chunk: {error}"
        )))?;
        Ok((response, done))
    }
    .await;

    match result {
        Ok((response, true)) => {
            close_fetch_stream_socket(bridge, vm_id, vm, state).await?;
            Ok(response)
        }
        Ok((response, false)) => {
            vm.vm_fetch_streams.insert(stream_id.to_owned(), state);
            Ok(response)
        }
        Err(error) => {
            if let Err(close_error) = close_fetch_stream_socket(bridge, vm_id, vm, state).await {
                tracing::error!(stream_id, error = %close_error, "failed to close errored VM fetch stream");
            }
            Err(error)
        }
    }
}

pub(in crate::execution) async fn cancel_kernel_http_fetch_stream<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
    stream_id: &str,
) -> Result<String, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let state = vm.vm_fetch_streams.remove(stream_id).ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "ERR_AGENTOS_VM_FETCH_STREAM_NOT_FOUND: stream {stream_id:?} is closed or unknown"
        ))
    })?;
    close_fetch_stream_socket(bridge, vm_id, vm, state).await?;
    Ok(String::from("{\"cancelled\":true}"))
}

/// Cancellation-safe ownership of a kernel socket used by a detached
/// `vm.fetch` operation. Socket I/O happens in short [`VmHandle`] commands;
/// readiness is awaited through this operation's dedicated notification.
struct OwnedKernelFetchSocket {
    vm: crate::state::VmHandle,
    kernel_readiness: KernelSocketReadinessRegistry,
    readiness_notify: Arc<tokio::sync::Notify>,
    kernel_pid: u32,
    socket_id: SocketId,
    capability: Option<agentos_runtime::capability::CapabilityLease>,
    armed: bool,
}

impl OwnedKernelFetchSocket {
    fn identity(&self) -> Option<(u64, u64)> {
        self.capability
            .as_ref()
            .map(|capability| (capability.id(), capability.generation()))
    }

    fn unregister_readiness(&self) {
        if let Some(identity) = self.identity() {
            self.kernel_readiness.unregister(self.socket_id, identity);
        }
    }

    fn close(&mut self) -> Result<(), SidecarError> {
        if !self.armed {
            return Ok(());
        }
        self.unregister_readiness();
        self.vm.try_command("close owned VM fetch socket", |vm| {
            close_kernel_socket_idempotent(&mut vm.kernel, self.kernel_pid, self.socket_id)
        })?;
        self.capability.take();
        self.armed = false;
        Ok(())
    }

    fn into_stream_state(
        mut self,
        target_process_id: String,
        raw_buffer: Vec<u8>,
        body_mode: VmFetchBodyMode,
        peer_closed: bool,
        max_response_bytes: usize,
    ) -> VmFetchStreamState {
        self.unregister_readiness();
        self.armed = false;
        VmFetchStreamState {
            target_process_id,
            kernel_pid: self.kernel_pid,
            socket_id: self.socket_id,
            _capability: self
                .capability
                .take()
                .expect("armed VM fetch socket must retain its capability"),
            raw_buffer,
            decoded_buffer: VecDeque::new(),
            body_mode,
            peer_closed,
            response_bytes: 0,
            max_response_bytes,
            last_progress_at: Instant::now(),
        }
    }
}

impl Drop for OwnedKernelFetchSocket {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = self.close() {
                eprintln!(
                    "ERR_AGENTOS_VM_FETCH_SOCKET_CLEANUP: failed to close cancelled VM fetch socket: {error}"
                );
            }
        }
    }
}

fn open_owned_kernel_fetch_socket(
    vm: &crate::state::VmHandle,
    target_process_id: &str,
    port: u16,
    path: &str,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
    body_bytes: Option<&[u8]>,
    enforce_stream_limit: bool,
) -> Result<OwnedKernelFetchSocket, SidecarError> {
    let handle = vm.clone();
    let readiness_notify = Arc::new(tokio::sync::Notify::new());
    let notify = Arc::clone(&readiness_notify);
    vm.try_command("open owned VM fetch socket", move |vm| {
        if enforce_stream_limit && vm.vm_fetch_streams.len() >= VM_FETCH_STREAM_COUNT_LIMIT {
            return Err(SidecarError::Execution(format!(
                "ERR_AGENTOS_VM_FETCH_STREAM_LIMIT: VM has {} open fetch streams; close or cancel a stream before opening another (limit {})",
                vm.vm_fetch_streams.len(),
                VM_FETCH_STREAM_COUNT_LIMIT
            )));
        }
        let pending_capability =
            reserve_capability(&vm.capabilities, CapabilityKind::TcpSocket)?;
        let kernel_pid = vm
            .active_processes
            .get(target_process_id)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "vm.fetch target process disappeared: {target_process_id}"
                ))
            })?
            .kernel_pid;
        let socket_id = vm
            .kernel
            .socket_create(EXECUTION_DRIVER_NAME, kernel_pid, SocketSpec::tcp())
            .map_err(kernel_error)?;
        let capability = match pending_capability.commit(CapabilityBackend::Kernel { socket_id }) {
            Ok(capability) => capability,
            Err(error) => {
                let _ = close_kernel_socket_idempotent(&mut vm.kernel, kernel_pid, socket_id);
                return Err(SidecarError::Execution(error.to_string()));
            }
        };
        let setup = (|| {
            vm.kernel
                .socket_bind_inet(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    socket_id,
                    InetSocketAddress::new("127.0.0.1", 0),
                )
                .map_err(kernel_error)?;
            vm.kernel
                .socket_connect_inet_loopback(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    socket_id,
                    InetSocketAddress::new("127.0.0.1", port),
                )
                .map_err(kernel_error)?;
            let request_bytes =
                serialize_kernel_http_fetch_request(port, path, options, headers, body_bytes);
            vm.kernel
                .socket_write(EXECUTION_DRIVER_NAME, kernel_pid, socket_id, &request_bytes)
                .map_err(kernel_error)
        })();
        if let Err(error) = setup {
            let close_result =
                close_kernel_socket_idempotent(&mut vm.kernel, kernel_pid, socket_id);
            if let Err(close_error) = close_result {
                eprintln!(
                    "ERR_AGENTOS_VM_FETCH_SOCKET_CLEANUP: setup rollback failed: {close_error}"
                );
            }
            return Err(error);
        }

        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let target = KernelSocketReadinessTarget {
            session: None,
            notify: Some(Arc::clone(&notify)),
            capability_id: capability.id(),
            capability_generation: capability.generation(),
            target_id: format!("vm-fetch:{target_process_id}:{socket_id}"),
            event: KernelSocketReadinessEvent::Data,
        };
        if let Err(error) = kernel_readiness.register(socket_id, target) {
            let close_result =
                close_kernel_socket_idempotent(&mut vm.kernel, kernel_pid, socket_id);
            if let Err(close_error) = close_result {
                eprintln!(
                    "ERR_AGENTOS_VM_FETCH_SOCKET_CLEANUP: readiness rollback failed: {close_error}"
                );
            }
            return Err(error);
        }
        // Registration is level-triggered. Force an initial zero-time probe so
        // readiness that preceded registration cannot be lost.
        notify.notify_one();
        Ok(OwnedKernelFetchSocket {
            vm: handle,
            kernel_readiness,
            readiness_notify: notify,
            kernel_pid,
            socket_id,
            capability: Some(capability),
            armed: true,
        })
    })
}

fn poll_owned_kernel_fetch_socket(
    socket: &OwnedKernelFetchSocket,
    response_buffer: &mut Vec<u8>,
    peer_closed: &mut bool,
    label: &str,
) -> Result<bool, SidecarError> {
    socket.vm.try_command("poll owned VM fetch socket", |vm| {
        let poll = vm
            .kernel
            .poll_targets(
                EXECUTION_DRIVER_NAME,
                socket.kernel_pid,
                vec![PollTargetEntry::socket(
                    socket.socket_id,
                    POLLIN | POLLHUP | POLLERR,
                )],
                0,
            )
            .map_err(kernel_error)?;
        let revents = poll
            .targets
            .first()
            .map(|entry| entry.revents)
            .unwrap_or_else(PollEvents::empty);
        if revents.intersects(POLLERR) {
            return Err(SidecarError::Execution(String::from(
                "ERR_AGENTOS_VM_FETCH_SOCKET: kernel TCP socket reported POLLERR",
            )));
        }
        let before = response_buffer.len();
        if revents.intersects(POLLIN) {
            loop {
                match vm.kernel.socket_read(
                    EXECUTION_DRIVER_NAME,
                    socket.kernel_pid,
                    socket.socket_id,
                    VM_FETCH_STREAM_CHUNK_MAX_BYTES,
                ) {
                    Ok(Some(bytes)) if !bytes.is_empty() => {
                        response_buffer.extend(bytes);
                        ensure_vm_fetch_raw_response_buffer_within_limit(
                            response_buffer.len(),
                            label,
                        )
                        .map_err(sidecar_core_execution_error)?;
                    }
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        *peer_closed = true;
                        break;
                    }
                    Err(error) if error.code() == "EAGAIN" => break,
                    Err(error) => return Err(kernel_error(error)),
                }
            }
        }
        if revents.intersects(POLLHUP) {
            *peer_closed = true;
        }
        Ok(response_buffer.len() != before || *peer_closed)
    })
}

pub(crate) async fn settle_owned_fetch_sync_rpc<B>(
    vm: &crate::state::VmHandle,
    process_id: &str,
    child_path: &[String],
    request: JavascriptSyncRpcRequest,
    response: Result<JavascriptSyncRpcServiceResponse, SidecarError>,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let response = match response {
        Ok(JavascriptSyncRpcServiceResponse::Deferred {
            receiver, timeout, ..
        }) => {
            let receive = async {
                receiver.await.unwrap_or_else(|_| {
                    Err(DeferredRpcError {
                        code: String::from("ERR_AGENTOS_DEFERRED_RPC_RESPONSE_CHANNEL_CLOSED"),
                        message: format!(
                            "deferred sync RPC response channel closed for {}",
                            request.method
                        ),
                    })
                })
            };
            let completion = match timeout {
                Some(timeout) => match tokio::time::timeout(timeout, receive).await {
                    Ok(result) => result,
                    Err(_) => Err(DeferredRpcError {
                        code: String::from("ERR_AGENTOS_DEFERRED_RPC_TIMEOUT"),
                        message: format!(
                            "{} deferred response timed out after {} ms",
                            request.method,
                            timeout.as_millis()
                        ),
                    }),
                },
                None => receive.await,
            };
            return vm.try_command("settle deferred owned JavaScript RPC", |vm| {
                let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                let Some(root) = vm.active_processes.get_mut(process_id) else {
                    return Ok(());
                };
                let path = child_path.iter().map(String::as_str).collect::<Vec<_>>();
                let Some(process) = NativeSidecar::<B>::active_process_by_path_mut(root, &path)
                else {
                    return Ok(());
                };
                settle_javascript_sync_rpc_completion(
                    process,
                    &kernel_readiness,
                    request.id,
                    completion,
                )
            });
        }
        other => other,
    };
    vm.try_command("settle owned VM fetch RPC", |vm| {
        let Some(root) = vm.active_processes.get_mut(process_id) else {
            return Ok(());
        };
        let path = child_path.iter().map(String::as_str).collect::<Vec<_>>();
        let Some(process) = NativeSidecar::<B>::active_process_by_path_mut(root, &path) else {
            return Ok(());
        };
        match response {
            Ok(result) => process
                .execution
                .respond_javascript_sync_rpc_response(request.id, result)
                .or_else(ignore_stale_javascript_sync_rpc_response),
            Err(error) => process
                .execution
                .respond_javascript_sync_rpc_error(
                    request.id,
                    javascript_sync_rpc_error_code(&error),
                    javascript_sync_rpc_error_message(&error),
                )
                .or_else(ignore_stale_javascript_sync_rpc_response),
        }
    })
}

/// Temporarily owns one UDP socket while `dgram.poll` waits outside VM state.
/// Cancellation restores the exact socket to its process. If the process was
/// reaped while the wait was in flight, the guard closes the detached handle
/// instead of leaking its kernel/readiness registration.
struct DetachedJavascriptUdpSocket {
    vm: VmHandle,
    process_id: String,
    child_path: Vec<String>,
    socket_id: String,
    kernel_pid: u32,
    readiness_identity: Option<(
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::CapabilityGeneration,
    )>,
    socket: Option<ActiveUdpSocket>,
}

impl DetachedJavascriptUdpSocket {
    fn socket(&self) -> &ActiveUdpSocket {
        self.socket
            .as_ref()
            .expect("detached JavaScript UDP socket remains owned until restoration")
    }

    fn restore(&mut self) -> Result<(), SidecarError> {
        if self.socket.is_none() {
            return Ok(());
        }
        let mut vm = self
            .vm
            .try_borrow_mut("restore detached JavaScript UDP socket")?;
        let VmState {
            kernel,
            active_processes,
            kernel_socket_readiness,
            ..
        } = &mut *vm;
        let process = active_processes.get_mut(&self.process_id).and_then(|root| {
            let mut process = root;
            for child_id in &self.child_path {
                process = process.child_processes.get_mut(child_id)?;
            }
            Some(process)
        });
        if let Some(process) = process {
            if !process.udp_sockets.contains_key(&self.socket_id) {
                process.udp_sockets.insert(
                    self.socket_id.clone(),
                    self.socket
                        .take()
                        .expect("checked detached JavaScript UDP socket"),
                );
                return Ok(());
            }
        }

        let mut socket = self
            .socket
            .take()
            .expect("checked detached JavaScript UDP socket");
        unregister_kernel_readiness_target(
            kernel_socket_readiness,
            socket.kernel_socket_id,
            self.readiness_identity,
        );
        socket.set_event_pusher(None, None);
        if socket.is_final_description_handle() {
            socket.close(kernel, self.kernel_pid);
        }
        Err(SidecarError::InvalidState(format!(
            "ERR_AGENTOS_UDP_POLL_RESTORE: UDP socket {} could not be restored because its process was reaped or the socket id was replaced",
            self.socket_id
        )))
    }
}

impl Drop for DetachedJavascriptUdpSocket {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            tracing::error!(
                %error,
                process_id = %self.process_id,
                socket_id = %self.socket_id,
                "failed to restore cancelled owned JavaScript UDP poll"
            );
        }
    }
}

async fn poll_detached_javascript_udp_socket_once(
    vm: &VmHandle,
    socket: &DetachedJavascriptUdpSocket,
) -> Result<Option<JavascriptUdpSocketEvent>, SidecarError> {
    let kernel_ready = vm.try_command("probe owned JavaScript UDP socket", |vm| {
        socket
            .socket()
            .poll_kernel_ready(&mut vm.kernel, socket.kernel_pid, Duration::ZERO)
    })?;
    if kernel_ready {
        let turn = socket.socket().acquire_poll_fair_turn().await?;
        let event = vm.try_command("consume owned JavaScript UDP datagram", |vm| {
            socket
                .socket()
                .consume_ready_kernel_datagram(&mut vm.kernel, socket.kernel_pid, turn)
        })?;
        if event.is_some() {
            return Ok(event);
        }
    }
    socket.socket().poll_native(Duration::ZERO).await
}

pub(in crate::execution) async fn service_owned_javascript_dgram_poll<B>(
    vm: &VmHandle,
    process_id: &str,
    child_path: &[String],
    request: &JavascriptSyncRpcRequest,
) -> Result<JavascriptSyncRpcServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let socket_id =
        javascript_sync_rpc_arg_str(&request.args, 0, "dgram.poll socket id")?.to_owned();
    let wait = Duration::from_millis(
        javascript_sync_rpc_arg_u64_optional(&request.args, 1, "dgram.poll wait ms")?
            .unwrap_or_default(),
    );
    let (socket_paths, kernel_pid, readiness_identity, active_socket) =
        vm.try_command("detach owned JavaScript UDP socket", |vm| {
            let socket_paths = build_javascript_socket_path_context(vm)?;
            let root = vm.active_processes.get_mut(process_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "JavaScript UDP poll target process disappeared: {process_id}"
                ))
            })?;
            let path = child_path.iter().map(String::as_str).collect::<Vec<_>>();
            let process =
                NativeSidecar::<B>::active_process_by_path_mut(root, &path).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "JavaScript UDP poll target descendant disappeared: {process_id}/{}",
                        child_path.join("/")
                    ))
                })?;
            let readiness_identity = process
                .capability_readiness_identity(&NativeCapabilityKey::UdpSocket(socket_id.clone()));
            let active_socket = process.udp_sockets.remove(&socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            Ok((
                socket_paths,
                process.kernel_pid,
                readiness_identity,
                active_socket,
            ))
        })?;
    let mut socket = DetachedJavascriptUdpSocket {
        vm: vm.clone(),
        process_id: process_id.to_owned(),
        child_path: child_path.to_vec(),
        socket_id,
        kernel_pid,
        readiness_identity,
        socket: Some(active_socket),
    };
    let wait = wait.min(socket.socket().reactor_limits.operation_deadline);
    let notified = Arc::clone(&socket.socket().read_event_notify).notified_owned();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let mut event = poll_detached_javascript_udp_socket_once(vm, &socket).await?;
    if event.is_none() && !wait.is_zero() {
        let _ = tokio::time::timeout(wait, notified.as_mut()).await;
        // Probe after both wake and timeout so a datagram arriving exactly at
        // the deadline is not deferred to an unrelated later poll.
        event = poll_detached_javascript_udp_socket_once(vm, &socket).await?;
    }
    socket.restore()?;
    javascript_dgram_poll_event_response(&socket_paths, event)
}

/// Service one target-runtime event without retaining VM state over a wait.
/// The only intrinsically asynchronous generic method (`dgram.poll`) is
/// detached into an owned operation first; every remaining generic dispatch
/// must complete during its short VM command.
pub(crate) async fn service_owned_javascript_sync_rpc_request<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &crate::state::VmHandle,
    process_id: &str,
    child_path: &[String],
    request: JavascriptSyncRpcRequest,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if request.method == "dgram.poll" {
        let response =
            service_owned_javascript_dgram_poll::<B>(vm, process_id, child_path, &request).await;
        return settle_owned_fetch_sync_rpc::<B>(vm, process_id, child_path, request, response)
            .await;
    }

    let response = vm.try_command("service owned JavaScript process event", |vm| {
        let socket_paths = build_javascript_socket_path_context(vm)?;
        let dns = vm.dns.clone();
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let capabilities = vm.capabilities.clone();
        let VmState {
            kernel,
            active_processes,
            ..
        } = vm;
        let root = active_processes.get_mut(process_id).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "JavaScript event target process disappeared: {process_id}"
            ))
        })?;
        let path = child_path.iter().map(String::as_str).collect::<Vec<_>>();
        let process =
            NativeSidecar::<B>::active_process_by_path_mut(root, &path).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "JavaScript event target descendant disappeared: {process_id}/{}",
                    child_path.join("/")
                ))
            })?;
        let mut future = Box::pin(service_javascript_sync_rpc(
            JavascriptSyncRpcServiceRequest {
                bridge,
                vm_id,
                dns: &dns,
                socket_paths: &socket_paths,
                kernel,
                kernel_readiness,
                process,
                sync_request: &request,
                capabilities,
            },
        ));
        let mut context = Context::from_waker(Waker::noop());
        let poll = future.as_mut().poll(&mut context);
        drop(future);
        match poll {
            Poll::Ready(response) => Ok(response),
            Poll::Pending => Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_JAVASCRIPT_RPC_OWNERSHIP: {} suspended inside the generic VM-state dispatcher; route it through an owned service",
                request.method
            ))),
        }
    })?;
    settle_owned_fetch_sync_rpc::<B>(vm, process_id, child_path, request, response).await
}

fn owned_fetch_process_notify(
    vm: &crate::state::VmHandle,
    process_id: &str,
) -> Result<Arc<tokio::sync::Notify>, SidecarError> {
    vm.try_read("clone VM fetch process notification", |vm| {
        vm.active_processes
            .get(process_id)
            .map(|process| Arc::clone(&process.process_event_notify))
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "vm.fetch target process disappeared: {process_id}"
                ))
            })
    })?
}

async fn wait_for_owned_fetch_progress(
    readiness_notify: &Arc<tokio::sync::Notify>,
    process_notify: &Arc<tokio::sync::Notify>,
    deadline: Instant,
) -> Result<(), SidecarError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(SidecarError::Execution(String::from(
            "ERR_AGENTOS_VM_FETCH_TIMEOUT: VM fetch progress deadline elapsed",
        )));
    }
    tokio::time::timeout(remaining, async {
        tokio::select! {
            _ = readiness_notify.notified() => {},
            _ = tokio::time::sleep(Duration::from_millis(1)) => {
                // The ordinary dispatcher exclusively owns guest execution
                // events. Wake it so runnable microtask or WASM work advances;
                // this fetch path only owns the kernel response socket.
                process_notify.notify_one();
                tokio::task::yield_now().await;
            },
        }
    })
    .await
    .map_err(|_| {
        SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_FETCH_TIMEOUT: timed out after {} ms waiting for VM fetch progress",
            http_loopback_request_timeout().as_millis()
        ))
    })?;
    Ok(())
}

async fn dispatch_owned_kernel_http_fetch(
    vm: &crate::state::VmHandle,
    target_process_id: &str,
    port: u16,
    path: &str,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
    body_bytes: Option<&[u8]>,
    max_fetch_response_bytes: usize,
) -> Result<String, SidecarError> {
    let mut socket = open_owned_kernel_fetch_socket(
        vm,
        target_process_id,
        port,
        path,
        options,
        headers,
        body_bytes,
        false,
    )?;
    let mut response_buffer = Vec::new();
    let mut peer_closed = false;
    let url = format!("http://127.0.0.1:{port}{path}");
    let deadline = Instant::now() + http_loopback_request_timeout();
    let process_notify = owned_fetch_process_notify(vm, target_process_id)?;
    loop {
        if let Some(response) =
            parse_kernel_http_fetch_response(&response_buffer, peer_closed, &url)
                .map_err(sidecar_core_execution_error)?
        {
            ensure_vm_fetch_response_within_limit(&response, "vm.fetch", max_fetch_response_bytes)
                .map_err(sidecar_core_execution_error)?;
            socket.close()?;
            return Ok(response);
        }
        if Instant::now() >= deadline {
            let preview = String::from_utf8_lossy(&response_buffer);
            return Err(SidecarError::Execution(format!(
                "vm.fetch timed out waiting for kernel TCP HTTP response ({} buffered bytes: {:?})",
                response_buffer.len(),
                preview.chars().take(200).collect::<String>()
            )));
        }
        let progressed = poll_owned_kernel_fetch_socket(
            &socket,
            &mut response_buffer,
            &mut peer_closed,
            "vm.fetch",
        )?;
        if !progressed {
            wait_for_owned_fetch_progress(&socket.readiness_notify, &process_notify, deadline)
                .await?;
        }
    }
}

async fn start_owned_kernel_http_fetch_stream(
    vm: &crate::state::VmHandle,
    target_process_id: &str,
    port: u16,
    path: &str,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
    body_bytes: Option<&[u8]>,
    max_response_bytes: usize,
) -> Result<String, SidecarError> {
    let socket = open_owned_kernel_fetch_socket(
        vm,
        target_process_id,
        port,
        path,
        options,
        headers,
        body_bytes,
        true,
    )?;
    let mut response_buffer = Vec::new();
    let mut peer_closed = false;
    let deadline = Instant::now() + http_loopback_request_timeout();
    let request_method = options.method.as_deref().unwrap_or("GET");
    let process_notify = owned_fetch_process_notify(vm, target_process_id)?;
    let (status, status_text, response_headers, body_mode) = loop {
        if let Some(header_end) = find_http_header_end(&response_buffer) {
            let parsed = parse_stream_response_head(
                &response_buffer[..header_end],
                request_method,
                max_response_bytes,
            )?;
            if (100..200).contains(&parsed.0) && parsed.0 != 101 {
                response_buffer.drain(..header_end + 4);
                continue;
            }
            response_buffer.drain(..header_end + 4);
            break parsed;
        }
        if peer_closed {
            return Err(SidecarError::Execution(String::from(
                "ERR_AGENTOS_VM_FETCH_TRUNCATED: peer closed before response headers completed",
            )));
        }
        if Instant::now() >= deadline {
            return Err(SidecarError::Execution(format!(
                "ERR_AGENTOS_VM_FETCH_TIMEOUT: timed out waiting for response headers after {} ms; raise AGENTOS_HTTP_LOOPBACK_REQUEST_TIMEOUT_MS",
                http_loopback_request_timeout().as_millis()
            )));
        }
        let progressed = poll_owned_kernel_fetch_socket(
            &socket,
            &mut response_buffer,
            &mut peer_closed,
            "vm.fetchStream",
        )?;
        if !progressed {
            wait_for_owned_fetch_progress(&socket.readiness_notify, &process_notify, deadline)
                .await?;
        }
    };

    let mut state = socket.into_stream_state(
        target_process_id.to_owned(),
        response_buffer,
        body_mode,
        peer_closed,
        max_response_bytes,
    );
    decode_stream_body(&mut state)?;
    let stream_id = vm.try_command("register owned VM fetch stream", |vm| {
        vm.next_vm_fetch_stream_id = vm.next_vm_fetch_stream_id.wrapping_add(1);
        let stream_id = format!("{}:{}", vm.generation, vm.next_vm_fetch_stream_id);
        vm.vm_fetch_streams.insert(stream_id.clone(), state);
        Ok(stream_id)
    })?;
    serde_json::to_string(&json!({
        "streamId": stream_id,
        "status": status,
        "statusText": status_text,
        "headers": response_headers,
    }))
    .map_err(|error| {
        SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_FETCH_SERIALIZE: failed to serialize response head: {error}"
        ))
    })
}

struct OwnedFetchStreamLease {
    vm: crate::state::VmHandle,
    stream_id: String,
    kernel_readiness: KernelSocketReadinessRegistry,
    readiness_notify: Arc<tokio::sync::Notify>,
    state: Option<VmFetchStreamState>,
}

impl OwnedFetchStreamLease {
    fn state(&self) -> &VmFetchStreamState {
        self.state
            .as_ref()
            .expect("owned VM fetch stream state must be present")
    }

    fn state_mut(&mut self) -> &mut VmFetchStreamState {
        self.state
            .as_mut()
            .expect("owned VM fetch stream state must be present")
    }

    fn identity(&self) -> (u64, u64) {
        let capability = &self.state()._capability;
        (capability.id(), capability.generation())
    }

    fn unregister_readiness(&self) {
        self.kernel_readiness
            .unregister(self.state().socket_id, self.identity());
    }

    fn reinsert(mut self) -> Result<(), SidecarError> {
        self.unregister_readiness();
        let state = self
            .state
            .take()
            .expect("owned VM fetch stream state must be present");
        self.vm.try_command("restore owned VM fetch stream", |vm| {
            vm.vm_fetch_streams.insert(self.stream_id.clone(), state);
            Ok(())
        })
    }

    fn close(mut self) -> Result<(), SidecarError> {
        self.unregister_readiness();
        let state = self
            .state
            .take()
            .expect("owned VM fetch stream state must be present");
        self.vm.try_command("close owned VM fetch stream", |vm| {
            close_kernel_socket_idempotent(&mut vm.kernel, state.kernel_pid, state.socket_id)
        })?;
        drop(state);
        Ok(())
    }
}

impl Drop for OwnedFetchStreamLease {
    fn drop(&mut self) {
        let Some(state) = self.state.take() else {
            return;
        };
        self.kernel_readiness.unregister(
            state.socket_id,
            (state._capability.id(), state._capability.generation()),
        );
        let result = self.vm.try_command("cancel owned VM fetch stream", |vm| {
            close_kernel_socket_idempotent(&mut vm.kernel, state.kernel_pid, state.socket_id)
        });
        if let Err(error) = result {
            eprintln!(
                "ERR_AGENTOS_VM_FETCH_STREAM_CLEANUP: failed to close cancelled stream {}: {error}",
                self.stream_id
            );
        }
        drop(state);
    }
}

fn lease_owned_fetch_stream(
    vm: &crate::state::VmHandle,
    stream_id: &str,
) -> Result<OwnedFetchStreamLease, SidecarError> {
    let readiness_notify = Arc::new(tokio::sync::Notify::new());
    let notify = Arc::clone(&readiness_notify);
    let handle = vm.clone();
    vm.try_command("lease owned VM fetch stream", |vm| {
        let state = vm.vm_fetch_streams.remove(stream_id).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "ERR_AGENTOS_VM_FETCH_STREAM_NOT_FOUND: stream {stream_id:?} is closed or unknown"
            ))
        })?;
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let target = KernelSocketReadinessTarget {
            session: None,
            notify: Some(Arc::clone(&notify)),
            capability_id: state._capability.id(),
            capability_generation: state._capability.generation(),
            target_id: format!("vm-fetch-stream:{stream_id}"),
            event: KernelSocketReadinessEvent::Data,
        };
        if let Err(error) = kernel_readiness.register(state.socket_id, target) {
            vm.vm_fetch_streams.insert(stream_id.to_owned(), state);
            return Err(error);
        }
        notify.notify_one();
        Ok(OwnedFetchStreamLease {
            vm: handle,
            stream_id: stream_id.to_owned(),
            kernel_readiness,
            readiness_notify: notify,
            state: Some(state),
        })
    })
}

async fn read_owned_kernel_http_fetch_stream(
    vm: &crate::state::VmHandle,
    stream_id: &str,
    requested_max_bytes: usize,
) -> Result<String, SidecarError> {
    let max_bytes = requested_max_bytes.clamp(1, VM_FETCH_STREAM_CHUNK_MAX_BYTES);
    let mut lease = lease_owned_fetch_stream(vm, stream_id)?;
    let target_process_id = lease.state().target_process_id.clone();
    let process_notify = owned_fetch_process_notify(vm, &target_process_id)?;
    loop {
        decode_stream_body(lease.state_mut())?;
        if !lease.state().decoded_buffer.is_empty()
            || matches!(lease.state().body_mode, VmFetchBodyMode::Empty)
        {
            break;
        }
        let deadline = lease.state().last_progress_at + http_loopback_request_timeout();
        if Instant::now() >= deadline {
            return Err(SidecarError::Execution(format!(
                "ERR_AGENTOS_VM_FETCH_TIMEOUT: stream produced no data for {} ms; raise AGENTOS_HTTP_LOOPBACK_REQUEST_TIMEOUT_MS",
                http_loopback_request_timeout().as_millis()
            )));
        }
        let (kernel_pid, socket_id) = (lease.state().kernel_pid, lease.state().socket_id);
        let mut raw_buffer = std::mem::take(&mut lease.state_mut().raw_buffer);
        let mut peer_closed = lease.state().peer_closed;
        let progressed = lease.vm.try_command("poll owned VM fetch stream", |vm| {
            let poll = vm
                .kernel
                .poll_targets(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    vec![PollTargetEntry::socket(
                        socket_id,
                        POLLIN | POLLHUP | POLLERR,
                    )],
                    0,
                )
                .map_err(kernel_error)?;
            let revents = poll
                .targets
                .first()
                .map(|entry| entry.revents)
                .unwrap_or_else(PollEvents::empty);
            if revents.intersects(POLLERR) {
                return Err(SidecarError::Execution(String::from(
                    "ERR_AGENTOS_VM_FETCH_SOCKET: kernel TCP stream reported POLLERR",
                )));
            }
            let before = raw_buffer.len();
            if revents.intersects(POLLIN) {
                loop {
                    match vm.kernel.socket_read(
                        EXECUTION_DRIVER_NAME,
                        kernel_pid,
                        socket_id,
                        VM_FETCH_STREAM_CHUNK_MAX_BYTES,
                    ) {
                        Ok(Some(bytes)) if !bytes.is_empty() => {
                            raw_buffer.extend(bytes);
                            ensure_vm_fetch_raw_response_buffer_within_limit(
                                raw_buffer.len(),
                                "vm.fetchStream",
                            )
                            .map_err(sidecar_core_execution_error)?;
                        }
                        Ok(Some(_)) => break,
                        Ok(None) => {
                            peer_closed = true;
                            break;
                        }
                        Err(error) if error.code() == "EAGAIN" => break,
                        Err(error) => return Err(kernel_error(error)),
                    }
                }
            }
            if revents.intersects(POLLHUP) {
                peer_closed = true;
            }
            Ok(raw_buffer.len() != before || peer_closed)
        })?;
        lease.state_mut().raw_buffer = raw_buffer;
        lease.state_mut().peer_closed = peer_closed;
        if progressed {
            lease.state_mut().last_progress_at = Instant::now();
        } else {
            wait_for_owned_fetch_progress(&lease.readiness_notify, &process_notify, deadline)
                .await?;
        }
    }

    let state = lease.state_mut();
    let take = max_bytes.min(state.decoded_buffer.len());
    let body: Vec<u8> = state.decoded_buffer.drain(..take).collect();
    let done = state.decoded_buffer.is_empty() && matches!(state.body_mode, VmFetchBodyMode::Empty);
    let response = serde_json::to_string(&json!({
        "body": base64::engine::general_purpose::STANDARD.encode(body),
        "done": done,
    }))
    .map_err(|error| {
        SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_FETCH_SERIALIZE: failed to serialize stream chunk: {error}"
        ))
    })?;
    if done {
        lease.close()?;
    } else {
        lease.reinsert()?;
    }
    Ok(response)
}

fn cancel_owned_kernel_http_fetch_stream(
    vm: &crate::state::VmHandle,
    stream_id: &str,
) -> Result<String, SidecarError> {
    let lease = lease_owned_fetch_stream(vm, stream_id)?;
    lease.close()?;
    Ok(String::from("{\"cancelled\":true}"))
}

struct OwnedLoopbackHttpRequest {
    vm: crate::state::VmHandle,
    process_id: String,
    request_key: (u64, u64),
    armed: bool,
}

impl OwnedLoopbackHttpRequest {
    fn complete(&mut self) {
        self.armed = false;
    }
}

impl Drop for OwnedLoopbackHttpRequest {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let result = self
            .vm
            .try_command("cancel owned loopback HTTP request", |vm| {
                if let Some(process) = vm.active_processes.get_mut(&self.process_id) {
                    process.pending_http_requests.remove(&self.request_key);
                }
                Ok(())
            });
        if let Err(error) = result {
            eprintln!(
                "ERR_AGENTOS_VM_FETCH_LOOPBACK_CLEANUP: failed to cancel pending request: {error}"
            );
        }
    }
}

async fn dispatch_owned_loopback_http_request(
    vm: &crate::state::VmHandle,
    process_id: &str,
    server_id: u64,
    request_json: &str,
) -> Result<String, SidecarError> {
    let (request_key, receiver) = vm.try_command("start owned loopback HTTP request", |vm| {
        let process = vm.active_processes.get_mut(process_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("vm.fetch target process disappeared: {process_id}"))
        })?;
        let (respond_to, receiver) = tokio::sync::oneshot::channel();
        let request_key = begin_loopback_http_request(process, server_id, request_json, || {
            PendingHttpRequest::Deferred(respond_to)
        })?;
        Ok((request_key, receiver))
    })?;
    let mut guard = OwnedLoopbackHttpRequest {
        vm: vm.clone(),
        process_id: process_id.to_owned(),
        request_key,
        armed: true,
    };
    let response = tokio::time::timeout(http_loopback_request_timeout(), receiver)
        .await
        .map_err(|_| {
            SidecarError::Execution(String::from(
                "HTTP loopback request timed out waiting for net.http_respond",
            ))
        })?
        .map_err(|_| {
            SidecarError::InvalidState(String::from(
                "HTTP loopback response waiter closed before net.http_respond",
            ))
        })?
        .map_err(|error| SidecarError::Execution(format!("{}: {}", error.code, error.message)))?;
    guard.complete();
    response.as_str().map(str::to_owned).ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "HTTP loopback response completed with a non-string value",
        ))
    })
}

/// Run `vm.fetch` without holding either the process coordinator or a VM state
/// borrow over readiness and adapter-response waits.
pub(in crate::execution) async fn dispatch_owned_vm_fetch(
    vm_id: &str,
    vm: crate::state::VmHandle,
    payload: VmFetchRequest,
) -> Result<String, SidecarError> {
    let stream_operation = payload.stream_operation.as_deref();
    if matches!(stream_operation, Some("read" | "cancel")) {
        let stream_id = payload.stream_id.as_deref().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "vm.fetch stream read/cancel requires stream_id",
            ))
        })?;
        return if stream_operation == Some("read") {
            read_owned_kernel_http_fetch_stream(
                &vm,
                stream_id,
                payload.max_bytes.unwrap_or(64 * 1024) as usize,
            )
            .await
        } else {
            cancel_owned_kernel_http_fetch_stream(&vm, stream_id)
        };
    }
    if let Some(operation) = stream_operation {
        if operation != "start" {
            return Err(SidecarError::InvalidState(format!(
                "unknown vm.fetch stream operation {operation:?}; expected start, read, or cancel"
            )));
        }
    }

    let target_path = format!("/{}", payload.path.trim_start_matches('/'));
    let request_url = Url::parse(&format!("http://127.0.0.1:{}{target_path}", payload.port))
        .map_err(|error| {
            SidecarError::InvalidState(format!("invalid vm.fetch target {target_path:?}: {error}"))
        })?;
    let header_values: BTreeMap<String, Value> = serde_json::from_str(&payload.headers_json)
        .map_err(|error| {
            SidecarError::InvalidState(format!("vm.fetch headers_json must be valid JSON: {error}"))
        })?;
    if payload.body.is_some() && payload.body_base64.is_some() {
        return Err(SidecarError::InvalidState(String::from(
            "vm.fetch accepts either body or body_base64, not both",
        )));
    }
    let body_bytes = payload
        .body_base64
        .as_deref()
        .map(|body| {
            base64::engine::general_purpose::STANDARD
                .decode(body)
                .map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "vm.fetch body_base64 must be valid base64: {error}"
                    ))
                })
        })
        .transpose()?;
    let options = JavascriptHttpRequestOptions {
        method: Some(payload.method),
        headers: header_values,
        body: payload.body,
        reject_unauthorized: None,
    };
    let headers = parse_http_header_collection(&options.headers, "vm.fetch headers")?;
    let (kernel_target, max_fetch_response_bytes) =
        vm.try_read("resolve VM fetch target", |vm| {
            (
                find_kernel_http_listener_process(vm, payload.port),
                vm.limits.http.max_fetch_response_bytes,
            )
        })?;
    if let Some(target_process_id) = kernel_target {
        return if stream_operation == Some("start") {
            start_owned_kernel_http_fetch_stream(
                &vm,
                &target_process_id,
                payload.port,
                &target_path,
                &options,
                &headers,
                body_bytes.as_deref(),
                max_fetch_response_bytes,
            )
            .await
        } else {
            dispatch_owned_kernel_http_fetch(
                &vm,
                &target_process_id,
                payload.port,
                &target_path,
                &options,
                &headers,
                body_bytes.as_deref(),
                max_fetch_response_bytes,
            )
            .await
        };
    }

    let target = vm.try_read("resolve loopback VM fetch target", |vm| {
        vm.active_processes
            .iter()
            .find_map(|(process_id, process)| {
                process
                    .http_servers
                    .iter()
                    .find(|(_, server)| server.guest_local_addr.port() == payload.port)
                    .map(|(server_id, _)| (process_id.clone(), *server_id))
            })
    })?;
    let Some((target_process_id, server_id)) = target else {
        return Err(SidecarError::Execution(format!(
            "vm.fetch could not find a guest HTTP listener on port {} in VM {vm_id}",
            payload.port
        )));
    };
    if stream_operation == Some("start") {
        return Err(SidecarError::InvalidState(String::from(
            "vm.fetch streaming requires a kernel-backed HTTP listener",
        )));
    }
    if body_bytes.is_some() {
        return Err(SidecarError::InvalidState(String::from(
            "binary vm.fetch bodies require a kernel-backed HTTP listener",
        )));
    }
    let request_json = serialize_http_loopback_request(&request_url, &options, &headers)?;
    dispatch_owned_loopback_http_request(&vm, &target_process_id, server_id, &request_json).await
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_kernel_http_fetch_with_socket<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
    target_process_id: &str,
    kernel_pid: u32,
    socket_id: SocketId,
    local_port: u16,
    port: u16,
    path: &str,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
    body_bytes: Option<&[u8]>,
    socket_paths: &JavascriptSocketPathContext,
    max_fetch_response_bytes: usize,
) -> Result<String, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    vm.kernel
        .socket_bind_inet(
            EXECUTION_DRIVER_NAME,
            kernel_pid,
            socket_id,
            InetSocketAddress::new("127.0.0.1", local_port),
        )
        .map_err(kernel_error)?;
    vm.kernel
        .socket_connect_inet_loopback(
            EXECUTION_DRIVER_NAME,
            kernel_pid,
            socket_id,
            InetSocketAddress::new("127.0.0.1", port),
        )
        .map_err(kernel_error)?;

    let request_bytes =
        serialize_kernel_http_fetch_request(port, path, options, headers, body_bytes);
    vm.kernel
        .socket_write(EXECUTION_DRIVER_NAME, kernel_pid, socket_id, &request_bytes)
        .map_err(kernel_error)?;

    let mut response_buffer = Vec::new();
    let mut peer_closed = false;
    let url = format!("http://127.0.0.1:{port}{path}");
    let deadline = Instant::now() + http_loopback_request_timeout();
    loop {
        if let Some(response) =
            parse_kernel_http_fetch_response(&response_buffer, peer_closed, &url)
                .map_err(sidecar_core_execution_error)?
        {
            ensure_vm_fetch_response_within_limit(&response, "vm.fetch", max_fetch_response_bytes)
                .map_err(sidecar_core_execution_error)?;
            return Ok(response);
        }
        if Instant::now() >= deadline {
            let preview = String::from_utf8_lossy(&response_buffer);
            return Err(SidecarError::Execution(format!(
                "vm.fetch timed out waiting for kernel TCP HTTP response ({} buffered bytes: {:?})",
                response_buffer.len(),
                preview.chars().take(200).collect::<String>()
            )));
        }

        {
            let dns = vm.dns.clone();
            let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
            let capabilities = vm.capabilities.clone();
            let process = vm
                .active_processes
                .get_mut(target_process_id)
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "vm.fetch target process disappeared: {target_process_id}"
                    ))
                })?;
            service_host_fetch_target_event(
                bridge,
                vm_id,
                &dns,
                socket_paths,
                &mut vm.kernel,
                &kernel_readiness,
                process,
                Duration::from_millis(5),
                &capabilities,
            )
            .await?;
        }

        let poll = vm
            .kernel
            .poll_targets(
                EXECUTION_DRIVER_NAME,
                kernel_pid,
                vec![PollTargetEntry::socket(
                    socket_id,
                    POLLIN | POLLHUP | POLLERR,
                )],
                5,
            )
            .map_err(kernel_error)?;
        let revents = poll
            .targets
            .first()
            .map(|entry| entry.revents)
            .unwrap_or_else(PollEvents::empty);
        if revents.intersects(POLLERR) {
            return Err(SidecarError::Execution(String::from(
                "vm.fetch kernel TCP socket reported POLLERR",
            )));
        }
        if revents.intersects(POLLIN) {
            loop {
                match vm
                    .kernel
                    .socket_read(EXECUTION_DRIVER_NAME, kernel_pid, socket_id, 64 * 1024)
                {
                    Ok(Some(bytes)) if !bytes.is_empty() => {
                        response_buffer.extend(bytes);
                        ensure_vm_fetch_raw_response_buffer_within_limit(
                            response_buffer.len(),
                            "vm.fetch",
                        )
                        .map_err(sidecar_core_execution_error)?;
                    }
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        peer_closed = true;
                        break;
                    }
                    Err(error) if error.code() == "EAGAIN" => break,
                    Err(error) => return Err(kernel_error(error)),
                }
            }
        }
        if revents.intersects(POLLHUP) {
            peer_closed = true;
        }
    }
}

fn outbound_http_response_json(url: &Url, response: ureq::Response) -> Result<Value, SidecarError> {
    let status = response.status();
    let status_text = response.status_text().to_owned();
    let mut header_pairs = Vec::new();
    let mut raw_headers = Vec::new();
    for raw_name in response.headers_names() {
        for value in response.all(&raw_name) {
            header_pairs.push(json!([raw_name.to_ascii_lowercase(), value]));
            raw_headers.push(Value::String(raw_name.clone()));
            raw_headers.push(Value::String(value.to_owned()));
        }
    }
    let mut reader = response.into_reader();
    let mut body = Vec::new();
    reader.read_to_end(&mut body).map_err(|error| {
        SidecarError::Execution(format!("failed to read HTTP response: {error}"))
    })?;
    serde_json::to_string(&json!({
        "status": status,
        "statusText": status_text,
        "headers": header_pairs,
        "rawHeaders": raw_headers,
        "body": base64::engine::general_purpose::STANDARD.encode(body),
        "bodyEncoding": "base64",
        "url": url.as_str(),
    }))
    .map(Value::String)
    .map_err(|error| SidecarError::Execution(format!("ERR_AGENTOS_NODE_SYNC_RPC: {error}")))
}

/// Split a ureq resolver `netloc` (`host:port`, with optional `[..]` IPv6
/// brackets) into its host and port components. Returns `None` if the port is
/// missing or unparseable.
fn split_netloc(netloc: &str) -> Option<(&str, u16)> {
    let (host, port) = netloc.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    Some((host, port))
}

pub(in crate::execution) fn issue_outbound_http_request(
    url: &Url,
    options: &JavascriptHttpRequestOptions,
    headers: &HttpHeaderCollection,
    pinned_addresses: &[IpAddr],
    default_ca_bundle: &[u8],
) -> Result<Value, SidecarError> {
    let method = options.method.as_deref().unwrap_or("GET");
    if pinned_addresses.is_empty() {
        return Err(SidecarError::Execution(String::from(
            "EACCES: no egress-vetted address available for outbound HTTP request",
        )));
    }
    // Pin the underlying resolver to the egress-vetted addresses. ureq performs
    // its own DNS resolution for the TCP/TLS connect; without this override an
    // https:// request would re-resolve the hostname through the host resolver
    // (a rebinding DNS server could then return a private/metadata IP that the
    // earlier range check would have rejected). The pinned resolver returns only
    // the vetted addresses and refuses any host it was not vetted for, while the
    // request URL keeps the original hostname so TLS SNI and the Host header stay
    // correct.
    let pinned_host = url.host_str().map(str::to_owned);
    let pinned: Vec<IpAddr> = pinned_addresses.to_vec();
    let resolver = move |netloc: &str| -> std::io::Result<Vec<SocketAddr>> {
        let (host, port) = split_netloc(netloc).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid network location: {netloc}"),
            )
        })?;
        let expected_host = pinned_host.as_deref();
        if expected_host != Some(host) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "EACCES: outbound HTTP resolver pinned to {expected_host:?}, refusing {host}"
                ),
            ));
        }
        if pinned.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "EACCES: no egress-vetted address available for outbound HTTP request",
            ));
        }
        Ok(pinned.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
    };
    let mut agent_builder = ureq::AgentBuilder::new()
        .resolver(resolver)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(15))
        .timeout_write(Duration::from_secs(15));
    if url.scheme() == "https" {
        let tls_options = JavascriptTlsBridgeOptions {
            is_server: false,
            servername: url.host_str().map(str::to_owned),
            alpn_protocols: Some(vec![String::from("http/1.1")]),
            reject_unauthorized: options.reject_unauthorized,
            ..JavascriptTlsBridgeOptions::default()
        };
        agent_builder = agent_builder.tls_config(Arc::new(build_client_tls_config(
            &tls_options,
            default_ca_bundle,
        )?));
    }
    let agent = agent_builder.build();
    let mut request = agent.request_url(method, url);
    for (name, values) in &headers.normalized {
        if name == "host" {
            continue;
        }
        let header_value = values.join(", ");
        request = request.set(name, &header_value);
    }
    let response = match options.body.as_deref() {
        Some(body) => request.send_string(body),
        None => request.call(),
    };

    match response {
        Ok(response) => outbound_http_response_json(url, response),
        Err(ureq::Error::Status(_, response)) => outbound_http_response_json(url, response),
        Err(ureq::Error::Transport(error)) => Err(SidecarError::Execution(format!(
            "ERR_HTTP_REQUEST_FAILED: {error}"
        ))),
    }
}

async fn wait_for_loopback_http_response<B>(
    request: LoopbackHttpResponseWaitRequest<'_, B>,
) -> Result<String, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let LoopbackHttpResponseWaitRequest {
        bridge,
        vm_id,
        dns,
        socket_paths,
        kernel,
        kernel_readiness,
        process,
        request_key,
        capabilities,
    } = request;
    let deadline = Instant::now() + http_loopback_request_timeout();
    loop {
        let response = match process.pending_http_requests.get(&request_key) {
            Some(PendingHttpRequest::Buffered(response)) => response.clone(),
            Some(PendingHttpRequest::Deferred(_)) | None => None,
        };
        if let Some(response) = response {
            process.pending_http_requests.remove(&request_key);
            return Ok(response);
        }

        if Instant::now() >= deadline {
            process.pending_http_requests.remove(&request_key);
            return Err(SidecarError::Execution(String::from(
                "HTTP loopback request timed out waiting for net.http_respond",
            )));
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        let Some(event) = process
            .execution
            .poll_event(remaining)
            .await
            .map_err(|error| SidecarError::Execution(error.to_string()))?
        else {
            continue;
        };

        match event {
            ActiveExecutionEvent::JavascriptSyncRpcRequest(request)
                if request.method == "net.http_wait" =>
            {
                process.queue_pending_execution_event(
                    ActiveExecutionEvent::JavascriptSyncRpcRequest(request),
                )?;
            }
            ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
                let response = service_javascript_sync_rpc(JavascriptSyncRpcServiceRequest {
                    bridge,
                    vm_id,
                    dns,
                    socket_paths,
                    kernel,
                    kernel_readiness: Arc::clone(&kernel_readiness),
                    process,
                    sync_request: &request,
                    capabilities: capabilities.clone(),
                })
                .await;
                settle_nested_javascript_sync_rpc(process, &request, response).await?;
            }
            ActiveExecutionEvent::Exited(code) => {
                process.pending_http_requests.remove(&request_key);
                return Err(SidecarError::Execution(format!(
                    "HTTP loopback server exited before responding (exit code {code})"
                )));
            }
            ActiveExecutionEvent::Stdout(_)
            | ActiveExecutionEvent::Stderr(_)
            | ActiveExecutionEvent::JavascriptSyncRpcCompletion(_)
            | ActiveExecutionEvent::PythonVfsRpcRequest(_)
            | ActiveExecutionEvent::PythonSocketConnectCompletion(_)
            | ActiveExecutionEvent::SignalState { .. } => {}
        }
    }
}

fn begin_loopback_http_request(
    process: &mut ActiveProcess,
    server_id: u64,
    request_json: &str,
    pending: impl FnOnce() -> PendingHttpRequest,
) -> Result<(u64, u64), SidecarError> {
    process.pending_http_requests.retain(
        |_, pending| !matches!(pending, PendingHttpRequest::Deferred(sender) if sender.is_closed()),
    );
    let request_id = {
        let server = process.http_servers.get_mut(&server_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("HTTP target server disappeared: {server_id}"))
        })?;
        server.next_request_id += 1;
        server.next_request_id
    };
    process
        .pending_http_requests
        .insert((server_id, request_id), pending());
    process.execution.send_javascript_stream_event(
        "http_request",
        json!({
            "serverId": server_id,
            "requestId": request_id,
            "request": request_json,
        }),
    )?;
    Ok((server_id, request_id))
}

pub(in crate::execution) fn complete_loopback_http_request(
    process: &mut ActiveProcess,
    request_key: (u64, u64),
    response_json: String,
) -> Result<(), SidecarError> {
    let pending = process
        .pending_http_requests
        .remove(&request_key)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "unknown pending HTTP request {} for server {}",
                request_key.1, request_key.0
            ))
        })?;
    match pending {
        PendingHttpRequest::Buffered(_) => {
            process.pending_http_requests.insert(
                request_key,
                PendingHttpRequest::Buffered(Some(response_json)),
            );
        }
        PendingHttpRequest::Deferred(respond_to) => {
            respond_to
                .send(Ok(Value::String(response_json)))
                .map_err(|_| {
                    SidecarError::InvalidState(String::from(
                        "HTTP loopback response waiter closed before net.http_respond",
                    ))
                })?;
        }
    }
    Ok(())
}

pub(crate) async fn dispatch_loopback_http_request<B>(
    request: LoopbackHttpDispatchRequest<'_, B>,
) -> Result<String, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let LoopbackHttpDispatchRequest {
        bridge,
        vm_id,
        dns,
        socket_paths,
        kernel,
        kernel_readiness,
        process,
        server_id,
        request_json,
        capabilities,
    } = request;
    let request_key = begin_loopback_http_request(process, server_id, request_json, || {
        PendingHttpRequest::Buffered(None)
    })?;
    wait_for_loopback_http_response(LoopbackHttpResponseWaitRequest {
        bridge,
        vm_id,
        dns,
        socket_paths,
        kernel,
        kernel_readiness,
        process,
        request_key,
        capabilities,
    })
    .await
}

pub(crate) fn dispatch_loopback_http_request_deferred<B>(
    request: LoopbackHttpDispatchRequest<'_, B>,
) -> Result<JavascriptSyncRpcServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let LoopbackHttpDispatchRequest {
        process,
        server_id,
        request_json,
        ..
    } = request;
    let (respond_to, receiver) = tokio::sync::oneshot::channel();
    begin_loopback_http_request(process, server_id, request_json, || {
        PendingHttpRequest::Deferred(respond_to)
    })?;
    Ok(JavascriptSyncRpcServiceResponse::Deferred {
        receiver,
        timeout: Some(http_loopback_request_timeout()),
        task_class: agentos_runtime::TaskClass::Listener,
    })
}

pub(in crate::execution) fn sidecar_core_execution_error(error: SidecarCoreError) -> SidecarError {
    SidecarError::Execution(error.to_string())
}

pub(crate) fn ensure_vm_fetch_response_frame_within_limit(
    response: &ResponseFrame,
    max_frame_bytes: usize,
) -> Result<(), SidecarError> {
    let max_frame_bytes = max_frame_bytes.min(VM_FETCH_BUFFER_LIMIT_BYTES);
    let frame = crate::protocol::to_generated_protocol_frame(
        &crate::protocol::ProtocolFrame::Response(response.clone()),
    )
    .map_err(|error| SidecarError::FrameTooLarge(error.to_string()))?;
    let WireProtocolFrame::ResponseFrame(_) = &frame else {
        return Err(SidecarError::FrameTooLarge(String::from(
            "vm fetch response converted to non-response wire frame",
        )));
    };
    WireFrameCodec::new(max_frame_bytes)
        .encode(&frame)
        .map(|_| ())
        .map_err(|error| SidecarError::FrameTooLarge(error.to_string()))
}

/// Adversarial coverage for the DNS-rebinding gap (VECTORS.md D.3) on the
/// Python/Pyodide `httpRequestSync` outbound HTTP path. The egress range guard
/// (`filter_dns_safe_ip_addrs`) runs at resolution time, but `ureq` performs its
/// own DNS resolution for the TCP/TLS connect, so a rebinding DNS server could
/// previously make the second lookup land on a private/link-local/metadata IP
/// the first check rejected. The fix pins `ureq`'s resolver to the vetted
/// address set; these tests prove the connect is pinned and refuses any other
/// host or an empty (fully-rejected) address set.
#[cfg(test)]
mod dns_rebinding_pin_tests {
    use super::{
        issue_outbound_http_request, serialize_kernel_http_fetch_request, split_netloc,
        JavascriptHttpRequestOptions,
    };
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, TcpListener};
    use std::thread;
    use url::Url;

    fn empty_headers() -> super::HttpHeaderCollection {
        super::parse_http_header_collection(&BTreeMap::new(), "test headers")
            .expect("empty header collection")
    }

    fn options() -> JavascriptHttpRequestOptions {
        JavascriptHttpRequestOptions {
            method: Some(String::from("GET")),
            headers: BTreeMap::new(),
            body: None,
            reject_unauthorized: None,
        }
    }

    #[test]
    fn split_netloc_handles_hostnames_and_bracketed_ipv6() {
        assert_eq!(
            split_netloc("attacker.example:80"),
            Some(("attacker.example", 80))
        );
        assert_eq!(split_netloc("[::1]:443"), Some(("::1", 443)));
        assert_eq!(split_netloc("10.0.0.1:8080"), Some(("10.0.0.1", 8080)));
        assert_eq!(split_netloc("no-port"), None);
        assert_eq!(split_netloc("host:notaport"), None);
    }

    #[test]
    fn vm_fetch_serializes_exactly_one_leading_path_slash() {
        for path in ["hello?q=1", "/hello?q=1", "//hello?q=1"] {
            let request =
                serialize_kernel_http_fetch_request(3080, path, &options(), &empty_headers(), None);
            assert!(
                request.starts_with(b"GET /hello?q=1 HTTP/1.1\r\n"),
                "unexpected request line for {path:?}: {}",
                String::from_utf8_lossy(&request)
            );
        }
    }

    /// A loopback HTTP server stands in for the egress-vetted target. The
    /// request URL uses a *different* hostname (`attacker.example`) whose real
    /// DNS would resolve elsewhere; pinning forces the connect onto the vetted
    /// IP only. If the resolver were unpinned, the request would fail to reach
    /// this server (and on a real host could land on a private/metadata IP).
    #[cfg(test)]
    #[test]
    fn outbound_http_connect_is_pinned_to_vetted_ip() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback server");
        let port = listener.local_addr().expect("local addr").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi")
                .expect("write response");
            let _ = stream.flush();
        });

        let url = Url::parse(&format!("http://attacker.example:{port}/")).expect("url");
        let pinned = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
        let result = issue_outbound_http_request(&url, &options(), &empty_headers(), &pinned, &[])
            .expect("pinned request should reach the vetted loopback target");
        let payload = result.as_str().expect("string payload");
        assert!(
            payload.contains("\"status\":200"),
            "expected 200 from pinned target, got: {payload}"
        );
        server.join().expect("server thread");
    }

    /// With no vetted address (every resolved IP was rejected by the range
    /// guard, or the literal IP was a blocked range), the pinned resolver must
    /// refuse rather than fall back to the host resolver.
    #[test]
    fn outbound_http_refuses_when_no_vetted_address() {
        let url = Url::parse("https://attacker.example/").expect("url");
        let error = issue_outbound_http_request(&url, &options(), &empty_headers(), &[], &[])
            .expect_err("empty pinned set must be refused");
        let message = error.to_string();
        assert!(
            message.contains("EACCES") || message.contains("ERR_HTTP_REQUEST_FAILED"),
            "expected an egress refusal, got: {message}"
        );
    }
}
