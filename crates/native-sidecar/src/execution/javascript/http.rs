use super::super::*;

const HTTP_LOOPBACK_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const VM_FETCH_STREAM_CHUNK_MAX_BYTES: usize = 64 * 1024;
const VM_FETCH_STREAM_COUNT_LIMIT: usize = 256;
type VmFetchResponseHead = (u16, String, Vec<(String, String)>, VmFetchBodyMode);

pub(in crate::execution) fn http_loopback_request_timeout() -> Duration {
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

pub(crate) struct LoopbackHttpDispatchRequest<'a> {
    pub(crate) process: &'a mut ActiveProcess,
    pub(crate) server_id: u64,
    pub(crate) request_json: &'a str,
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
    .map_err(|error| SidecarError::host("ERR_AGENTOS_NODE_SYNC_RPC", format!("{error}")))
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
    socket_paths: &SocketPathContext,
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
    let identity = process.kernel_handle.runtime_identity();
    let max_reply_bytes = process.limits.reactor.max_bridge_response_bytes;
    let event = if wait.is_zero() {
        process
            .execution
            .try_poll_event(identity, max_reply_bytes)?
    } else {
        process
            .execution
            .poll_event(identity, max_reply_bytes, wait)
            .await?
    };
    let Some(event) = event else { return Ok(false) };

    match event {
        ActiveExecutionEvent::HostRpcRequest(request) if request.method == "net.http_wait" => {
            // The listener wait intentionally remains pending until server
            // close. A nested vm.fetch pump must not steal it from the main
            // sidecar dispatcher or wait for it inline.
            process.queue_pending_execution_event(ActiveExecutionEvent::HostRpcRequest(request))?;
        }
        ActiveExecutionEvent::HostRpcRequest(request) => {
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
                managed_descriptions: None,
            })
            .await;
            settle_execution_host_call(&request.reply, response)?;
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

async fn drain_host_fetch_target_events<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
    target_process_id: &str,
    socket_paths: &SocketPathContext,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    for _ in 0..32 {
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
            // A just-closed client socket may need another bounded reactor
            // turn before the guest observes EOF and retires its accepted
            // socket. Keep probing within this fixed 32 ms cleanup budget
            // instead of returning after the first empty 1 ms poll.
            continue;
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
    let socket_paths = build_socket_path_context(vm)?;
    // Client source ports belong to the kernel socket table. The listen-port
    // allocator does not reserve active client sockets and can hand the same
    // source port to concurrent requests.
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
    match result {
        Ok(response) => {
            close_result?;
            cleanup_result?;
            Ok(response)
        }
        Err(error) => {
            if let Err(close_error) = close_result {
                eprintln!(
                    "ERR_AGENTOS_HTTP_FETCH_CLEANUP: failed to close kernel socket {socket_id} after fetch error: {close_error}"
                );
            }
            if let Err(cleanup_error) = cleanup_result {
                eprintln!(
                    "ERR_AGENTOS_HTTP_FETCH_CLEANUP: failed to drain target events after fetch error: {cleanup_error}"
                );
            }
            Err(error)
        }
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
    let socket_paths = build_socket_path_context(vm)?;
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
        .map_err(|error| {
            SidecarError::Execution(format!(
                "ERR_AGENTOS_VM_FETCH_SERIALIZE: failed to serialize response head: {error}"
            ))
        })
    }
    .await;

    if result.is_err() {
        if let Err(error) = vm
            .kernel
            .socket_close(EXECUTION_DRIVER_NAME, kernel_pid, socket_id)
        {
            tracing::error!(
                socket_id,
                error = %error,
                "failed to close kernel socket after VM fetch stream start error"
            );
        }
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
    let socket_paths = build_socket_path_context(vm)?;
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
            let socket_paths = build_socket_path_context(vm)?;
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
        .map_err(|error| {
            SidecarError::Execution(format!(
                "ERR_AGENTOS_VM_FETCH_SERIALIZE: failed to serialize stream chunk: {error}"
            ))
        })?;
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
    socket_paths: &SocketPathContext,
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

pub(in crate::execution) fn begin_loopback_http_request(
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

pub(in crate::execution) fn take_loopback_http_response(
    process: &mut ActiveProcess,
    request_key: (u64, u64),
) -> Option<String> {
    let response = match process.pending_http_requests.get(&request_key) {
        Some(PendingHttpRequest::Buffered(response)) => response.clone(),
        Some(PendingHttpRequest::Deferred(_)) | None => None,
    }?;
    process.pending_http_requests.remove(&request_key);
    Some(response)
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

pub(crate) fn dispatch_loopback_http_request_deferred(
    request: LoopbackHttpDispatchRequest<'_>,
) -> Result<HostServiceResponse, SidecarError> {
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
    Ok(HostServiceResponse::Deferred {
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
