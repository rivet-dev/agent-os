use super::super::*;

trait Http2AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> Http2AsyncIo for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Http2ServerListenOptions {
    server_id: u64,
    secure: bool,
    port: Option<u16>,
    host: Option<String>,
    backlog: Option<u32>,
    timeout: Option<u64>,
    settings: BTreeMap<String, Value>,
    tls: Option<TlsBridgeOptions>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Http2SessionConnectOptions {
    authority: Option<String>,
    protocol: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    settings: BTreeMap<String, Value>,
    tls: Option<TlsBridgeOptions>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Http2RequestOptions {
    end_stream: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Http2FileResponseOptions {
    offset: Option<u64>,
    length: Option<i64>,
}

pub(in crate::execution) struct Http2ServiceRequest<'a, B> {
    pub(in crate::execution) bridge: &'a SharedBridge<B>,
    pub(in crate::execution) kernel: &'a mut SidecarKernel,
    pub(in crate::execution) vm_id: &'a str,
    pub(in crate::execution) dns: &'a VmDnsConfig,
    pub(in crate::execution) socket_paths: &'a SocketPathContext,
    pub(in crate::execution) process: &'a mut ActiveProcess,
    pub(in crate::execution) sync_request: &'a HostRpcRequest,
    pub(in crate::execution) capabilities: CapabilityRegistry,
}

#[derive(Debug)]
struct ClientHttp2StreamState {
    send_stream: Option<h2::SendStream<Bytes>>,
    pending_write: Option<PendingHttp2Write>,
    response: Option<client::ResponseFuture>,
    recv_stream: Option<h2::RecvStream>,
    pending_read: Option<PendingHttp2Read>,
    budget_wait: Option<Pin<Box<tokio::sync::futures::OwnedNotified>>>,
    resume_wait: Option<Pin<Box<tokio::sync::futures::OwnedNotified>>>,
}

#[derive(Debug)]
struct ServerHttp2StreamState {
    send_response: Option<ServerHttp2Responder>,
    send_stream: Option<h2::SendStream<Bytes>>,
    pending_write: Option<PendingHttp2Write>,
    recv_stream: Option<h2::RecvStream>,
    pending_read: Option<PendingHttp2Read>,
    budget_wait: Option<Pin<Box<tokio::sync::futures::OwnedNotified>>>,
    resume_wait: Option<Pin<Box<tokio::sync::futures::OwnedNotified>>>,
}

#[derive(Debug)]
enum ServerHttp2Responder {
    Regular(server::SendResponse<Bytes>),
    Pushed(server::SendPushedResponse<Bytes>),
}

#[derive(Debug)]
struct PendingHttp2Write {
    bytes: Bytes,
    offset: usize,
    end_stream: bool,
    respond_to: Http2ResponseSender,
    success_value: Value,
    _reservations: Vec<Reservation>,
}

#[derive(Debug)]
struct PendingHttp2Read {
    bytes: Bytes,
    reservations: Vec<Reservation>,
}

fn release_http2_read_prefix(pending: &mut PendingHttp2Read, amount: usize) {
    for reservation in &mut pending.reservations {
        match reservation.split(amount) {
            Some(consumed) => drop(consumed),
            None => {
                eprintln!(
                    "ERR_AGENTOS_HTTP2_ACCOUNTING_SPLIT: attempted to release {amount} bytes from a smaller reservation"
                );
                return;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Http2TurnUsage {
    operations: usize,
    bytes: usize,
    still_ready: bool,
}

fn reserve_http2_inbound_chunk(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    bytes: usize,
) -> Result<Vec<Reservation>, SidecarError> {
    let resources = shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?
        .resources
        .as_ref()
        .cloned()
        .ok_or_else(|| {
            SidecarError::host(
                "ERR_AGENTOS_RUNTIME_UNAVAILABLE",
                String::from("HTTP/2 read has no VM ResourceLedger"),
            )
        })?;
    reserve_http2_resources(
        &resources,
        &[
            (ResourceClass::Http2DataBytes, bytes),
            (ResourceClass::Http2BufferedBytes, bytes),
            (ResourceClass::BufferedBytes, bytes),
        ],
    )
}

fn poll_http2_write(
    cx: &mut Context<'_>,
    send_stream: &mut h2::SendStream<Bytes>,
    pending: &mut PendingHttp2Write,
    byte_budget: usize,
) -> Poll<Result<bool, String>> {
    let remaining = pending.bytes.len().saturating_sub(pending.offset);
    if remaining == 0 {
        return Poll::Ready(
            send_stream
                .send_data(Bytes::new(), pending.end_stream)
                .map(|()| true)
                .map_err(|error| error.to_string()),
        );
    }
    send_stream.reserve_capacity(remaining);
    let capacity = match send_stream.poll_capacity(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(Some(Ok(capacity))) => capacity,
        Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error.to_string())),
        Poll::Ready(None) => {
            return Poll::Ready(Err(String::from(
                "HTTP/2 send stream closed while waiting for flow-control capacity",
            )))
        }
    };
    let amount = remaining.min(capacity).min(byte_budget.max(1));
    if amount == 0 {
        return Poll::Pending;
    }
    let next_offset = pending.offset.saturating_add(amount);
    let finished = next_offset == pending.bytes.len();
    let frame = pending.bytes.slice(pending.offset..next_offset);
    send_stream
        .send_data(frame, pending.end_stream && finished)
        .map_err(|error| error.to_string())?;
    pending.offset = next_offset;
    Poll::Ready(Ok(finished))
}

const HTTP2_DEFAULT_WINDOW_SIZE: u32 = 65_535;

fn reserve_http2_resources(
    resources: &ResourceLedger,
    reservations: &[(ResourceClass, usize)],
) -> Result<Vec<Reservation>, SidecarError> {
    let mut admitted = Vec::with_capacity(reservations.len());
    for (resource, amount) in reservations {
        if *amount != 0 {
            admitted.push(
                resources
                    .reserve(*resource, *amount)
                    .map_err(|error| SidecarError::Execution(error.to_string()))?,
            );
        }
    }
    Ok(admitted)
}

fn http2_event_bytes(event: &Http2BridgeEvent) -> usize {
    64usize
        .saturating_add(event.kind.len())
        .saturating_add(event.data.as_ref().map_or(0, String::len))
        .saturating_add(event.extra.as_ref().map_or(0, String::len))
        .saturating_add(event.extra_headers.as_ref().map_or(0, String::len))
}

fn http2_event_header_bytes(event: &Http2BridgeEvent) -> usize {
    let explicit = event.extra_headers.as_ref().map_or(0, String::len);
    if event.kind.ends_with("Headers") {
        explicit.saturating_add(event.data.as_ref().map_or(0, String::len))
    } else {
        explicit
    }
}

fn http2_event_data_bytes(event: &Http2BridgeEvent) -> usize {
    if event.kind.ends_with("Data") {
        event.data.as_ref().map_or(0, String::len)
    } else {
        0
    }
}

fn reserve_http2_event(
    state: &crate::state::Http2SharedState,
    event: &Http2BridgeEvent,
) -> Result<Vec<Reservation>, SidecarError> {
    let resources = state.resources.as_ref().ok_or_else(|| {
        SidecarError::host(
            "ERR_AGENTOS_RUNTIME_UNAVAILABLE",
            String::from("HTTP/2 state has no VM ResourceLedger"),
        )
    })?;
    let event_bytes = http2_event_bytes(event);
    reserve_http2_resources(
        resources,
        &[
            (ResourceClass::Http2Events, 1),
            (ResourceClass::Http2EventBytes, event_bytes),
            (ResourceClass::Http2BufferedBytes, event_bytes),
            (ResourceClass::BufferedBytes, event_bytes),
            (
                ResourceClass::Http2HeaderBytes,
                http2_event_header_bytes(event),
            ),
            (ResourceClass::Http2DataBytes, http2_event_data_bytes(event)),
        ],
    )
}

fn http2_pause_state(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    stream_id: u64,
) -> Option<(Arc<AtomicBool>, Arc<tokio::sync::Notify>)> {
    shared.lock().ok()?.streams.get(&stream_id).map(|stream| {
        (
            Arc::clone(&stream.paused),
            Arc::clone(&stream.resume_notify),
        )
    })
}

fn http2_budget_wait(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
) -> Option<Pin<Box<tokio::sync::futures::OwnedNotified>>> {
    shared
        .lock()
        .ok()
        .map(|state| Box::pin(Arc::clone(&state.event_capacity_notify).notified_owned()))
}

fn poll_http2_budget_wait(
    cx: &mut Context<'_>,
    wait: &mut Option<Pin<Box<tokio::sync::futures::OwnedNotified>>>,
) -> bool {
    let Some(notified) = wait.as_mut() else {
        return true;
    };
    if notified.as_mut().poll(cx).is_pending() {
        return false;
    }
    *wait = None;
    true
}

fn poll_http2_resume(
    cx: &mut Context<'_>,
    paused: &AtomicBool,
    resume_notify: Arc<tokio::sync::Notify>,
    resume_wait: &mut Option<Pin<Box<tokio::sync::futures::OwnedNotified>>>,
) -> bool {
    if !paused.load(Ordering::Acquire) {
        *resume_wait = None;
        return true;
    }
    let notified = resume_wait.get_or_insert_with(|| Box::pin(resume_notify.notified_owned()));
    if notified.as_mut().poll(cx).is_ready() {
        *resume_wait = None;
    }
    !paused.load(Ordering::Acquire)
}

fn finish_http2_client_stream(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    snapshot: &Arc<Mutex<Http2SessionSnapshot>>,
    session_id: u64,
    stream_id: u64,
    reason: Reason,
) {
    if let Ok(mut snapshot) = snapshot.lock() {
        snapshot.state.next_stream_id = snapshot.state.next_stream_id.saturating_add(2);
    }
    push_http2_session_event(
        shared,
        session_id,
        Http2BridgeEvent {
            kind: String::from("clientClose"),
            id: stream_id,
            extra_number: Some(u32::from(reason) as u64),
            ..Http2BridgeEvent::default()
        },
    );
    if let Ok(mut state) = shared.lock() {
        remove_http2_stream_locked(&mut state, stream_id);
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_client_http2_streams(
    cx: &mut Context<'_>,
    streams: &mut BTreeMap<u64, ClientHttp2StreamState>,
    rotation: &mut VecDeque<u64>,
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    snapshot: &Arc<Mutex<Http2SessionSnapshot>>,
    session_id: u64,
    operation_quantum: usize,
    byte_quantum: usize,
) -> Poll<Http2TurnUsage> {
    let mut progressed = false;
    let mut operations_serviced = 0usize;
    let mut bytes_serviced = 0usize;
    let turns = rotation.len().min(operation_quantum.max(1));
    for _ in 0..turns {
        let Some(stream_id) = rotation.pop_front() else {
            break;
        };
        let Some(mut stream) = streams.remove(&stream_id) else {
            continue;
        };
        let mut stream_operations = 0usize;
        let mut keep = true;

        if let Some(mut pending) = stream.pending_write.take() {
            let before = pending.offset;
            let result = match stream.send_stream.as_mut() {
                Some(send_stream) => poll_http2_write(cx, send_stream, &mut pending, byte_quantum),
                None => Poll::Ready(Err(format!(
                    "HTTP/2 client stream {stream_id} is not writable"
                ))),
            };
            bytes_serviced = bytes_serviced.saturating_add(pending.offset.saturating_sub(before));
            match result {
                Poll::Pending => stream.pending_write = Some(pending),
                Poll::Ready(Ok(false)) => {
                    stream_operations = stream_operations.saturating_add(1);
                    stream.pending_write = Some(pending);
                }
                Poll::Ready(Ok(true)) => {
                    stream_operations = stream_operations.saturating_add(1);
                    if pending.end_stream {
                        stream.send_stream = None;
                    }
                    pending.respond_to.settle(Ok(pending.success_value));
                }
                Poll::Ready(Err(error)) => {
                    stream_operations = stream_operations.saturating_add(1);
                    pending.respond_to.settle(Err(error));
                    stream.send_stream = None;
                }
            }
        }

        if operations_serviced.saturating_add(stream_operations) < operation_quantum.max(1) {
            if let Some(mut response_future) = stream.response.take() {
                match Pin::new(&mut response_future).poll(cx) {
                    Poll::Pending => stream.response = Some(response_future),
                    Poll::Ready(Ok(response)) => {
                        stream_operations = stream_operations.saturating_add(1);
                        match serialize_http2_response_headers(&response) {
                            Ok(headers_json) => {
                                push_http2_session_event(
                                    shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("clientResponseHeaders"),
                                        id: stream_id,
                                        data: Some(headers_json),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                stream.recv_stream = Some(response.into_body());
                            }
                            Err(error) => {
                                push_http2_session_event(
                                    shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("clientError"),
                                        id: stream_id,
                                        data: Some(http2_error_payload(error.to_string())),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                finish_http2_client_stream(
                                    shared,
                                    snapshot,
                                    session_id,
                                    stream_id,
                                    Reason::INTERNAL_ERROR,
                                );
                                keep = false;
                            }
                        }
                    }
                    Poll::Ready(Err(error)) => {
                        stream_operations = stream_operations.saturating_add(1);
                        push_http2_session_event(
                            shared,
                            session_id,
                            Http2BridgeEvent {
                                kind: String::from("clientError"),
                                id: stream_id,
                                data: Some(http2_error_payload(error.to_string())),
                                ..Http2BridgeEvent::default()
                            },
                        );
                        finish_http2_client_stream(
                            shared,
                            snapshot,
                            session_id,
                            stream_id,
                            Reason::INTERNAL_ERROR,
                        );
                        keep = false;
                    }
                }
            } else if let Some(body) = stream.recv_stream.as_mut() {
                let can_read = bytes_serviced < byte_quantum.max(1)
                    && poll_http2_budget_wait(cx, &mut stream.budget_wait)
                    && match http2_pause_state(shared, stream_id) {
                        Some((paused, resume_notify)) => {
                            poll_http2_resume(cx, &paused, resume_notify, &mut stream.resume_wait)
                        }
                        None => false,
                    };
                if can_read {
                    if let Some(mut pending) = stream.pending_read.take() {
                        let remaining_budget = byte_quantum.max(1).saturating_sub(bytes_serviced);
                        let amount = pending.bytes.len().min(remaining_budget);
                        let chunk = pending.bytes.slice(..amount);
                        let delivered = push_http2_data_event(
                            shared,
                            session_id,
                            false,
                            "clientData",
                            stream_id,
                            &chunk,
                        );
                        if delivered {
                            stream_operations = stream_operations.saturating_add(1);
                            bytes_serviced = bytes_serviced.saturating_add(amount);
                            if let Err(error) = body.flow_control().release_capacity(amount) {
                                eprintln!(
                                "ERR_AGENTOS_HTTP2_FLOW_CONTROL: stream={stream_id} error={error}"
                            );
                            }
                            release_http2_read_prefix(&mut pending, amount);
                            if amount != pending.bytes.len() {
                                pending.bytes = pending.bytes.slice(amount..);
                                stream.pending_read = Some(pending);
                            }
                        } else {
                            stream.pending_read = Some(pending);
                            stream.budget_wait = http2_budget_wait(shared);
                        }
                    } else {
                        match body.poll_data(cx) {
                            Poll::Pending => {}
                            Poll::Ready(Some(Ok(chunk))) => {
                                stream_operations = stream_operations.saturating_add(1);
                                let source_reservations = match reserve_http2_inbound_chunk(
                                    shared,
                                    chunk.len(),
                                ) {
                                    Ok(reservations) => reservations,
                                    Err(error) => {
                                        eprintln!("ERR_AGENTOS_HTTP2_DATA_ADMISSION: stream={stream_id} error={error}");
                                        finish_http2_client_stream(
                                            shared,
                                            snapshot,
                                            session_id,
                                            stream_id,
                                            Reason::ENHANCE_YOUR_CALM,
                                        );
                                        operations_serviced = operations_serviced.saturating_add(1);
                                        continue;
                                    }
                                };
                                stream.pending_read = Some(PendingHttp2Read {
                                    bytes: chunk,
                                    reservations: source_reservations,
                                });
                            }
                            Poll::Ready(Some(Err(error))) => {
                                stream_operations = stream_operations.saturating_add(1);
                                push_http2_session_event(
                                    shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("clientError"),
                                        id: stream_id,
                                        data: Some(http2_error_payload(error.to_string())),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                finish_http2_client_stream(
                                    shared,
                                    snapshot,
                                    session_id,
                                    stream_id,
                                    Reason::INTERNAL_ERROR,
                                );
                                keep = false;
                            }
                            Poll::Ready(None) => {
                                stream_operations = stream_operations.saturating_add(1);
                                push_http2_session_event(
                                    shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("clientEnd"),
                                        id: stream_id,
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                finish_http2_client_stream(
                                    shared,
                                    snapshot,
                                    session_id,
                                    stream_id,
                                    Reason::NO_ERROR,
                                );
                                keep = false;
                            }
                        }
                    }
                }
            }
        }

        if keep {
            streams.insert(stream_id, stream);
            rotation.push_back(stream_id);
        }
        if stream_operations != 0 {
            progressed = true;
            operations_serviced = operations_serviced.saturating_add(stream_operations);
        }
        if bytes_serviced >= byte_quantum.max(1) {
            break;
        }
    }
    if progressed {
        Poll::Ready(Http2TurnUsage {
            operations: operations_serviced,
            bytes: bytes_serviced,
            still_ready: !rotation.is_empty()
                && (operations_serviced >= operation_quantum.max(1)
                    || bytes_serviced >= byte_quantum.max(1)),
        })
    } else {
        Poll::Pending
    }
}

fn poll_server_http2_streams(
    cx: &mut Context<'_>,
    streams: &mut BTreeMap<u64, ServerHttp2StreamState>,
    rotation: &mut VecDeque<u64>,
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    server_id: u64,
    operation_quantum: usize,
    byte_quantum: usize,
) -> Poll<Http2TurnUsage> {
    let mut progressed = false;
    let mut operations_serviced = 0usize;
    let mut bytes_serviced = 0usize;
    let turns = rotation.len().min(operation_quantum.max(1));
    for _ in 0..turns {
        let Some(stream_id) = rotation.pop_front() else {
            break;
        };
        let Some(mut stream) = streams.remove(&stream_id) else {
            continue;
        };
        let mut stream_operations = 0usize;
        let mut keep = true;
        if let Some(mut pending) = stream.pending_write.take() {
            let before = pending.offset;
            let result = match stream.send_stream.as_mut() {
                Some(send_stream) => poll_http2_write(cx, send_stream, &mut pending, byte_quantum),
                None => Poll::Ready(Err(format!(
                    "HTTP/2 server stream {stream_id} is not writable"
                ))),
            };
            bytes_serviced = bytes_serviced.saturating_add(pending.offset.saturating_sub(before));
            match result {
                Poll::Pending => stream.pending_write = Some(pending),
                Poll::Ready(Ok(false)) => {
                    stream_operations = stream_operations.saturating_add(1);
                    stream.pending_write = Some(pending);
                }
                Poll::Ready(Ok(true)) => {
                    stream_operations = stream_operations.saturating_add(1);
                    if pending.end_stream {
                        stream.send_stream = None;
                        push_http2_server_event(
                            shared,
                            server_id,
                            Http2BridgeEvent {
                                kind: String::from("serverStreamClose"),
                                id: stream_id,
                                extra_number: Some(0),
                                ..Http2BridgeEvent::default()
                            },
                        );
                    }
                    pending.respond_to.settle(Ok(pending.success_value));
                }
                Poll::Ready(Err(error)) => {
                    stream_operations = stream_operations.saturating_add(1);
                    pending.respond_to.settle(Err(error));
                    stream.send_stream = None;
                }
            }
        }
        if operations_serviced.saturating_add(stream_operations) < operation_quantum.max(1) {
            if let Some(body) = stream.recv_stream.as_mut() {
                let can_read = bytes_serviced < byte_quantum.max(1)
                    && poll_http2_budget_wait(cx, &mut stream.budget_wait)
                    && match http2_pause_state(shared, stream_id) {
                        Some((paused, resume_notify)) => {
                            poll_http2_resume(cx, &paused, resume_notify, &mut stream.resume_wait)
                        }
                        None => false,
                    };
                if can_read {
                    if let Some(mut pending) = stream.pending_read.take() {
                        let remaining_budget = byte_quantum.max(1).saturating_sub(bytes_serviced);
                        let amount = pending.bytes.len().min(remaining_budget);
                        let chunk = pending.bytes.slice(..amount);
                        let delivered = push_http2_data_event(
                            shared,
                            server_id,
                            true,
                            "serverStreamData",
                            stream_id,
                            &chunk,
                        );
                        if delivered {
                            stream_operations = stream_operations.saturating_add(1);
                            bytes_serviced = bytes_serviced.saturating_add(amount);
                            if let Err(error) = body.flow_control().release_capacity(amount) {
                                eprintln!(
                                "ERR_AGENTOS_HTTP2_FLOW_CONTROL: stream={stream_id} error={error}"
                            );
                            }
                            release_http2_read_prefix(&mut pending, amount);
                            if amount != pending.bytes.len() {
                                pending.bytes = pending.bytes.slice(amount..);
                                stream.pending_read = Some(pending);
                            }
                        } else {
                            stream.pending_read = Some(pending);
                            stream.budget_wait = http2_budget_wait(shared);
                        }
                    } else {
                        match body.poll_data(cx) {
                            Poll::Pending => {}
                            Poll::Ready(Some(Ok(chunk))) => {
                                stream_operations = stream_operations.saturating_add(1);
                                let source_reservations = match reserve_http2_inbound_chunk(
                                    shared,
                                    chunk.len(),
                                ) {
                                    Ok(reservations) => reservations,
                                    Err(error) => {
                                        eprintln!("ERR_AGENTOS_HTTP2_DATA_ADMISSION: stream={stream_id} error={error}");
                                        if let Ok(mut state) = shared.lock() {
                                            remove_http2_stream_locked(&mut state, stream_id);
                                        }
                                        operations_serviced = operations_serviced.saturating_add(1);
                                        continue;
                                    }
                                };
                                stream.pending_read = Some(PendingHttp2Read {
                                    bytes: chunk,
                                    reservations: source_reservations,
                                });
                            }
                            Poll::Ready(Some(Err(error))) => {
                                stream_operations = stream_operations.saturating_add(1);
                                push_http2_server_event(
                                    shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("serverStreamError"),
                                        id: stream_id,
                                        data: Some(http2_error_payload(error.to_string())),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                stream.recv_stream = None;
                            }
                            Poll::Ready(None) => {
                                stream_operations = stream_operations.saturating_add(1);
                                push_http2_server_event(
                                    shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("serverStreamEnd"),
                                        id: stream_id,
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                stream.recv_stream = None;
                            }
                        }
                    }
                }
            }
        }
        if stream.recv_stream.is_none()
            && stream.send_response.is_none()
            && stream.send_stream.is_none()
            && stream.pending_write.is_none()
            && stream.pending_read.is_none()
        {
            if let Ok(mut state) = shared.lock() {
                remove_http2_stream_locked(&mut state, stream_id);
            }
            keep = false;
        }
        if keep {
            streams.insert(stream_id, stream);
            rotation.push_back(stream_id);
        }
        if stream_operations != 0 {
            progressed = true;
            operations_serviced = operations_serviced.saturating_add(stream_operations);
        }
        if bytes_serviced >= byte_quantum.max(1) {
            break;
        }
    }
    if progressed {
        Poll::Ready(Http2TurnUsage {
            operations: operations_serviced,
            bytes: bytes_serviced,
            still_ready: !rotation.is_empty()
                && (operations_serviced >= operation_quantum.max(1)
                    || bytes_serviced >= byte_quantum.max(1)),
        })
    } else {
        Poll::Pending
    }
}

#[derive(Debug)]
struct Http2ReadyWake {
    notify: Arc<tokio::sync::Notify>,
}

impl Wake for Http2ReadyWake {
    fn wake(self: Arc<Self>) {
        self.notify.notify_one();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notify.notify_one();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_client_http2_fair_turn(
    runtime: &agentos_runtime::RuntimeContext,
    vm_generation: u64,
    capability_id: u64,
    streams: &mut BTreeMap<u64, ClientHttp2StreamState>,
    rotation: &mut VecDeque<u64>,
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    snapshot: &Arc<Mutex<Http2SessionSnapshot>>,
    session_id: u64,
    requested: FairBudget,
) -> Result<(), SidecarError> {
    let ready = Arc::new(tokio::sync::Notify::new());
    let waker = Waker::from(Arc::new(Http2ReadyWake {
        notify: Arc::clone(&ready),
    }));
    loop {
        let notified = ready.notified();
        let turn = runtime
            .fairness()
            .acquire(vm_generation, capability_id, requested)
            .await
            .map_err(|error| SidecarError::Execution(error.to_string()))?;
        let allowance = turn.allowance();
        let usage = {
            let mut cx = Context::from_waker(&waker);
            match poll_client_http2_streams(
                &mut cx,
                streams,
                rotation,
                shared,
                snapshot,
                session_id,
                allowance.operations,
                allowance.bytes,
            ) {
                Poll::Ready(usage) => usage,
                Poll::Pending => Http2TurnUsage::default(),
            }
        };
        turn.complete(
            FairBudget::new(usage.operations, usage.bytes),
            usage.still_ready,
        )
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
        if usage.operations != 0 || usage.bytes != 0 {
            return Ok(());
        }
        // The readiness waker was registered by the poll above. No fair turn
        // is retained while this task awaits transport/window progress.
        notified.await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_server_http2_fair_turn(
    runtime: &agentos_runtime::RuntimeContext,
    vm_generation: u64,
    capability_id: u64,
    streams: &mut BTreeMap<u64, ServerHttp2StreamState>,
    rotation: &mut VecDeque<u64>,
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    server_id: u64,
    requested: FairBudget,
) -> Result<(), SidecarError> {
    let ready = Arc::new(tokio::sync::Notify::new());
    let waker = Waker::from(Arc::new(Http2ReadyWake {
        notify: Arc::clone(&ready),
    }));
    loop {
        let notified = ready.notified();
        let turn = runtime
            .fairness()
            .acquire(vm_generation, capability_id, requested)
            .await
            .map_err(|error| SidecarError::Execution(error.to_string()))?;
        let allowance = turn.allowance();
        let usage = {
            let mut cx = Context::from_waker(&waker);
            match poll_server_http2_streams(
                &mut cx,
                streams,
                rotation,
                shared,
                server_id,
                allowance.operations,
                allowance.bytes,
            ) {
                Poll::Ready(usage) => usage,
                Poll::Pending => Http2TurnUsage::default(),
            }
        };
        turn.complete(
            FairBudget::new(usage.operations, usage.bytes),
            usage.still_ready,
        )
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
        if usage.operations != 0 || usage.bytes != 0 {
            return Ok(());
        }
        notified.await;
    }
}

fn http2_runtime_snapshot() -> Http2RuntimeSnapshot {
    Http2RuntimeSnapshot {
        effective_local_window_size: HTTP2_DEFAULT_WINDOW_SIZE,
        local_window_size: HTTP2_DEFAULT_WINDOW_SIZE,
        remote_window_size: HTTP2_DEFAULT_WINDOW_SIZE,
        next_stream_id: 1,
        outbound_queue_size: 1,
        deflate_dynamic_table_size: 0,
        inflate_dynamic_table_size: 0,
    }
}

fn http2_snapshot_json(snapshot: &Http2SessionSnapshot) -> Result<String, SidecarError> {
    serde_json::to_string(snapshot)
        .map_err(|error| SidecarError::host("ERR_AGENTOS_NODE_SYNC_RPC", format!("{error}")))
}

fn http2_event_value(event: &Http2BridgeEvent) -> Result<Value, SidecarError> {
    serde_json::to_string(event)
        .map(Value::String)
        .map_err(|error| SidecarError::host("ERR_AGENTOS_NODE_SYNC_RPC", format!("{error}")))
}

fn push_http2_server_event(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    server_id: u64,
    event: Http2BridgeEvent,
) -> bool {
    let (ready, wake_session, wake_identity, should_wake) = if let Ok(mut state) = shared.lock() {
        let reservations = match reserve_http2_event(&state, &event) {
            Ok(reservations) => reservations,
            Err(error) => {
                eprintln!(
                    "ERR_AGENTOS_HTTP2_EVENT_ADMISSION: server={server_id} kind={} error={error}",
                    event.kind
                );
                return false;
            }
        };
        let wake_identity = state
            .capability_leases
            .get(&NativeCapabilityKey::Http2Server(server_id))
            .map(|lease| (lease.id(), lease.generation()));
        let queue = state.server_events.entry(server_id).or_default();
        let should_wake = queue.is_empty();
        queue.push_back(QueuedHttp2Event {
            event,
            reservations,
        });
        (
            Some(Arc::clone(&state.ready)),
            state.event_session.clone(),
            wake_identity,
            should_wake,
        )
    } else {
        eprintln!("ERR_AGENTOS_HTTP2_STATE_POISONED: HTTP/2 server event state lock poisoned");
        (None, None, None, false)
    };
    let queued = ready.is_some();
    if let Some(ready) = ready {
        ready.notify_waiters();
    }
    if should_wake {
        push_http2_retain_wake(wake_session, wake_identity);
    }
    queued
}

fn push_http2_session_event(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    session_id: u64,
    event: Http2BridgeEvent,
) -> bool {
    let (ready, wake_session, wake_identity, should_wake) = if let Ok(mut state) = shared.lock() {
        let reservations = match reserve_http2_event(&state, &event) {
            Ok(reservations) => reservations,
            Err(error) => {
                eprintln!(
                    "ERR_AGENTOS_HTTP2_EVENT_ADMISSION: session={session_id} kind={} error={error}",
                    event.kind
                );
                return false;
            }
        };
        let wake_identity = state
            .capability_leases
            .get(&NativeCapabilityKey::Http2Session(session_id))
            .map(|lease| (lease.id(), lease.generation()));
        let queue = state.session_events.entry(session_id).or_default();
        let should_wake = queue.is_empty();
        queue.push_back(QueuedHttp2Event {
            event,
            reservations,
        });
        (
            Some(Arc::clone(&state.ready)),
            state.event_session.clone(),
            wake_identity,
            should_wake,
        )
    } else {
        eprintln!("ERR_AGENTOS_HTTP2_STATE_POISONED: HTTP/2 session event state lock poisoned");
        (None, None, None, false)
    };
    let queued = ready.is_some();
    if let Some(ready) = ready {
        ready.notify_waiters();
    }
    if should_wake {
        push_http2_retain_wake(wake_session, wake_identity);
    }
    queued
}

fn push_http2_data_event(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    queue_id: u64,
    is_server: bool,
    event_kind: &'static str,
    stream_id: u64,
    chunk: &Bytes,
) -> bool {
    let encoded_bytes = chunk.len().saturating_add(2) / 3;
    let encoded_bytes = encoded_bytes.saturating_mul(4);
    let event_bytes = 64usize
        .saturating_add(event_kind.len())
        .saturating_add(encoded_bytes);
    let (ready, wake_session, wake_identity, should_wake) = if let Ok(mut state) = shared.lock() {
        let Some(resources) = state.resources.as_ref() else {
            eprintln!(
                "ERR_AGENTOS_RUNTIME_UNAVAILABLE: HTTP/2 data event has no VM ResourceLedger"
            );
            return false;
        };
        let reservations = match reserve_http2_resources(
            resources,
            &[
                (ResourceClass::Http2Events, 1),
                (ResourceClass::Http2EventBytes, event_bytes),
                (ResourceClass::Http2BufferedBytes, event_bytes),
                (ResourceClass::BufferedBytes, event_bytes),
                (ResourceClass::Http2DataBytes, encoded_bytes),
            ],
        ) {
            Ok(reservations) => reservations,
            Err(error) => {
                eprintln!(
                    "ERR_AGENTOS_HTTP2_EVENT_ADMISSION: {}={queue_id} kind={event_kind} error={error}",
                    if is_server { "server" } else { "session" }
                );
                return false;
            }
        };
        // Admission happens before the base64/JSON-owned event allocation.
        let event = Http2BridgeEvent {
            kind: String::from(event_kind),
            id: stream_id,
            data: Some(base64::engine::general_purpose::STANDARD.encode(chunk)),
            ..Http2BridgeEvent::default()
        };
        let capability_key = if is_server {
            NativeCapabilityKey::Http2Server(queue_id)
        } else {
            NativeCapabilityKey::Http2Session(queue_id)
        };
        let wake_identity = state
            .capability_leases
            .get(&capability_key)
            .map(|lease| (lease.id(), lease.generation()));
        let queue = if is_server {
            state.server_events.entry(queue_id).or_default()
        } else {
            state.session_events.entry(queue_id).or_default()
        };
        let should_wake = queue.is_empty();
        queue.push_back(QueuedHttp2Event {
            event,
            reservations,
        });
        (
            Arc::clone(&state.ready),
            state.event_session.clone(),
            wake_identity,
            should_wake,
        )
    } else {
        eprintln!("ERR_AGENTOS_HTTP2_STATE_POISONED: HTTP/2 data event state lock poisoned");
        return false;
    };
    ready.notify_waiters();
    if should_wake {
        push_http2_retain_wake(wake_session, wake_identity);
    }
    true
}

fn push_http2_retain_wake(
    session: Option<ExecutionWakeHandle>,
    identity: Option<(
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::CapabilityGeneration,
    )>,
) {
    let (Some(session), Some((capability_id, capability_generation))) = (session, identity) else {
        return;
    };
    if let Err(error) = session.publish_readiness(
        capability_id,
        capability_generation,
        agentos_runtime::readiness::ReadyFlags::READABLE,
    ) {
        eprintln!("ERR_AGENTOS_HTTP2_WAKE: failed to queue HTTP/2 wake: {error}");
    }
}

fn pop_http2_event(
    queue: &mut BTreeMap<u64, VecDeque<QueuedHttp2Event>>,
    id: u64,
) -> Option<QueuedHttp2Event> {
    let (event, drained) = {
        let events = queue.get_mut(&id)?;
        let event = events.pop_front();
        (event, events.is_empty())
    };
    if drained {
        queue.remove(&id);
    }
    event
}

fn pop_http2_event_now(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    id: u64,
    is_server: bool,
) -> Option<Http2BridgeEvent> {
    let mut state = shared.lock().ok()?;
    let queue = if is_server {
        &mut state.server_events
    } else {
        &mut state.session_events
    };
    let queued = pop_http2_event(queue, id)?;
    state.event_capacity_notify.notify_one();
    let QueuedHttp2Event {
        event,
        reservations: _reservations,
    } = queued;
    let terminal = is_http2_terminal_event(&event, is_server, id);
    drop(state);
    if terminal {
        retire_http2_readiness(shared, id, is_server);
    }
    Some(event)
}

fn retire_http2_readiness(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    id: u64,
    is_server: bool,
) {
    let key = if is_server {
        NativeCapabilityKey::Http2Server(id)
    } else {
        NativeCapabilityKey::Http2Session(id)
    };
    let retired = shared.lock().ok().and_then(|mut state| {
        let session = state.event_session.clone()?;
        let lease = state.capability_leases.remove(&key)?;
        Some((session, lease.id(), lease.generation()))
    });
    if let Some((session, capability_id, capability_generation)) = retired {
        if let Err(error) = session.remove_readiness(capability_id, capability_generation) {
            eprintln!(
                "ERR_AGENTOS_READY_REMOVE: capability={capability_id} generation={capability_generation}: {error}"
            );
        }
    }
}

fn next_http2_session_id(shared: &mut crate::state::Http2SharedState) -> u64 {
    shared.next_session_id += 1;
    shared.next_session_id
}

fn next_http2_stream_id(shared: &mut crate::state::Http2SharedState) -> u64 {
    shared.next_stream_id += 1;
    shared.next_stream_id
}

fn http2_reason(code: Option<u32>) -> Reason {
    code.unwrap_or(Reason::NO_ERROR.into()).into()
}

fn http2_error_payload(message: impl Into<String>) -> String {
    serde_json::to_string(&json!({
        "name": "Error",
        "code": "ERR_HTTP2_ERROR",
        "message": message.into(),
    }))
    .unwrap_or_else(|_| {
        String::from(
            "{\"name\":\"Error\",\"code\":\"ERR_HTTP2_ERROR\",\"message\":\"HTTP/2 bridge error\"}",
        )
    })
}

fn http2_socket_snapshot(local_addr: SocketAddr, remote_addr: SocketAddr) -> Http2SocketSnapshot {
    Http2SocketSnapshot {
        encrypted: false,
        allow_half_open: false,
        local_address: Some(local_addr.ip().to_string()),
        local_port: Some(local_addr.port()),
        local_family: Some(socket_addr_family(&local_addr).to_string()),
        remote_address: Some(remote_addr.ip().to_string()),
        remote_port: Some(remote_addr.port()),
        remote_family: Some(socket_addr_family(&remote_addr).to_string()),
        servername: None,
        alpn_protocol: Some(String::from("h2c")),
    }
}

fn http2_wait_result(kind: &str, id: u64) -> Value {
    json!({
        "kind": kind,
        "id": id,
    })
}

fn is_http2_terminal_event(event: &Http2BridgeEvent, is_server: bool, id: u64) -> bool {
    if is_server {
        event.kind == "serverClose" && event.id == id
    } else {
        event.kind == "sessionClose" && event.id == id
    }
}

async fn await_http2_event(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    id: u64,
    is_server: bool,
) -> Result<Option<Http2BridgeEvent>, crate::state::DeferredRpcError> {
    loop {
        let (queued, exists, notified, event_capacity_notify) = {
            let mut state = shared.lock().map_err(|_| crate::state::DeferredRpcError {
                code: String::from("ERR_AGENTOS_HTTP2_STATE_POISONED"),
                message: String::from("HTTP/2 event state lock poisoned"),
                details: None,
            })?;
            let notified = Arc::clone(&state.ready).notified_owned();
            let queue = if is_server {
                &mut state.server_events
            } else {
                &mut state.session_events
            };
            let queued = pop_http2_event(queue, id);
            let exists = if is_server {
                state.servers.contains_key(&id)
            } else {
                state.sessions.contains_key(&id)
            };
            (
                queued,
                exists,
                notified,
                Arc::clone(&state.event_capacity_notify),
            )
        };
        if let Some(queued) = queued {
            let QueuedHttp2Event {
                event,
                reservations,
            } = queued;
            drop(reservations);
            event_capacity_notify.notify_one();
            if is_http2_terminal_event(&event, is_server, id) {
                retire_http2_readiness(shared, id, is_server);
            }
            return Ok(Some(event));
        }
        if !exists {
            return Ok(None);
        }
        notified.await;
    }
}

fn defer_http2_poll(
    process: &ActiveProcess,
    id: u64,
    is_server: bool,
    wait_ms: u64,
) -> Result<HostServiceResponse, SidecarError> {
    if let Some(event) = pop_http2_event_now(&process.http2.shared, id, is_server) {
        return http2_event_value(&event).map(Into::into);
    }
    if wait_ms == 0 {
        return Ok(Value::Null.into());
    }
    let operation_deadline = Duration::from_millis(process.limits.reactor.operation_deadline_ms);
    let requested_wait = Duration::from_millis(wait_ms);
    let wait = requested_wait.min(operation_deadline);
    let warn_operation_deadline = requested_wait >= operation_deadline;
    let shared = Arc::clone(&process.http2.shared);
    let (respond_to, receiver) = tokio::sync::oneshot::channel();
    process
        .runtime_context
        .spawn(agentos_runtime::TaskClass::Http2, async move {
            let wait_result = if warn_operation_deadline {
                crate::execution::operation_deadline_timeout(
                    "HTTP/2 event poll",
                    wait,
                    await_http2_event(&shared, id, is_server),
                )
                .await
            } else {
                tokio::time::timeout(wait, await_http2_event(&shared, id, is_server)).await
            };
            let result = match wait_result {
                Ok(Ok(Some(event))) => {
                    http2_event_value(&event).map_err(|error| crate::state::DeferredRpcError {
                        code: String::from("ERR_AGENTOS_HTTP2_EVENT_SERIALIZE"),
                        message: error.to_string(),
                        details: None,
                    })
                }
                Ok(Ok(None)) | Err(_) => Ok(Value::Null),
                Ok(Err(error)) => Err(error),
            };
            respond_to.settle(result);
        })
        .map_err(SidecarError::from)?;
    Ok(HostServiceResponse::Deferred {
        receiver,
        timeout: None,
        task_class: agentos_runtime::TaskClass::Http2,
    })
}

fn defer_http2_wait(
    process: &ActiveProcess,
    id: u64,
    is_server: bool,
) -> Result<HostServiceResponse, SidecarError> {
    let shared = Arc::clone(&process.http2.shared);
    let event_session = shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?
        .event_session
        .clone();
    let max_adapter_event_bytes = process.limits.js_runtime.event_payload_limit_bytes;
    let (respond_to, receiver) = tokio::sync::oneshot::channel();
    process
        .runtime_context
        .spawn(agentos_runtime::TaskClass::Http2, async move {
            let result = loop {
                let event = match await_http2_event(&shared, id, is_server).await {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        break Ok(if is_server {
                            http2_wait_result("serverClose", id)
                        } else {
                            http2_wait_result("sessionClose", id)
                        });
                    }
                    Err(error) => break Err(error),
                };
                let payload = match serde_json::to_value(&event) {
                    Ok(payload) => payload,
                    Err(error) => {
                        break Err(crate::state::DeferredRpcError {
                            code: String::from("ERR_AGENTOS_HTTP2_EVENT_SERIALIZE"),
                            message: error.to_string(),
                            details: None,
                        });
                    }
                };
                if let Some(session) = &event_session {
                    if let Err(error) = session.send_adapter_event(
                        "http2",
                        &payload,
                        "limits.jsRuntime.eventPayloadLimitBytes",
                        max_adapter_event_bytes,
                    ) {
                        break Err(crate::state::DeferredRpcError {
                            code: error.code().to_owned(),
                            message: error.message().to_owned(),
                            details: error.details().cloned(),
                        });
                    }
                }
                if is_http2_terminal_event(&event, is_server, id) {
                    break Ok(payload);
                }
            };
            respond_to.settle(result);
        })
        .map_err(SidecarError::from)?;
    Ok(HostServiceResponse::Deferred {
        receiver,
        timeout: None,
        task_class: agentos_runtime::TaskClass::Http2,
    })
}

fn http2_settings_from_value(settings: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    settings.clone()
}

fn parse_http2_headers_json(
    headers_json: &str,
    label: &str,
) -> Result<BTreeMap<String, Value>, SidecarError> {
    serde_json::from_str::<BTreeMap<String, Value>>(headers_json)
        .map_err(|error| SidecarError::InvalidState(format!("{label} must be valid JSON: {error}")))
}

fn apply_http2_header_values(
    header_map: &mut HeaderMap,
    name: &str,
    value: &Value,
) -> Result<(), SidecarError> {
    let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
        SidecarError::InvalidState(format!("invalid HTTP/2 header name {name:?}: {error}"))
    })?;
    match value {
        Value::Array(values) => {
            for value in values {
                apply_http2_header_values(header_map, name, value)?;
            }
        }
        Value::String(text) => {
            let value = HeaderValue::from_str(text).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid HTTP/2 header value for {name}: {error}"
                ))
            })?;
            header_map.append(header_name.clone(), value);
        }
        Value::Number(number) => {
            let value = HeaderValue::from_str(&number.to_string()).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid HTTP/2 numeric header value for {name}: {error}"
                ))
            })?;
            header_map.append(header_name.clone(), value);
        }
        Value::Bool(boolean) => {
            let value = HeaderValue::from_str(if *boolean { "true" } else { "false" }).map_err(
                |error| {
                    SidecarError::InvalidState(format!(
                        "invalid HTTP/2 boolean header value for {name}: {error}"
                    ))
                },
            )?;
            header_map.append(header_name.clone(), value);
        }
        Value::Null => {}
        Value::Object(_) => {
            return Err(SidecarError::InvalidState(format!(
                "unsupported HTTP/2 header object value for {name}"
            )));
        }
    }
    Ok(())
}

fn build_http2_request(headers_json: &str) -> Result<Request<()>, SidecarError> {
    let headers = parse_http2_headers_json(headers_json, "HTTP/2 request headers")?;
    let method = headers
        .get(":method")
        .and_then(Value::as_str)
        .unwrap_or("GET");
    let path = headers.get(":path").and_then(Value::as_str).unwrap_or("/");
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).map_err(|error| {
            SidecarError::InvalidState(format!("invalid HTTP/2 method {method:?}: {error}"))
        })?)
        .uri(path.parse::<Uri>().map_err(|error| {
            SidecarError::InvalidState(format!("invalid HTTP/2 path {path:?}: {error}"))
        })?);
    {
        let header_map = builder.headers_mut().expect("request header map");
        for (name, value) in &headers {
            if name.starts_with(':') {
                continue;
            }
            apply_http2_header_values(header_map, name, value)?;
        }
    }
    builder
        .body(())
        .map_err(|error| SidecarError::InvalidState(format!("invalid HTTP/2 request: {error}")))
}

fn build_http2_response(headers_json: &str) -> Result<Response<()>, SidecarError> {
    let headers = parse_http2_headers_json(headers_json, "HTTP/2 response headers")?;
    let status = headers
        .get(":status")
        .and_then(Value::as_u64)
        .or_else(|| {
            headers
                .get(":status")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<u16>().ok().map(u64::from))
        })
        .unwrap_or(200);
    let mut builder = Response::builder().status(status as u16);
    {
        let header_map = builder.headers_mut().expect("response header map");
        for (name, value) in &headers {
            if name.starts_with(':') {
                continue;
            }
            apply_http2_header_values(header_map, name, value)?;
        }
    }
    builder.body(()).map_err(|error| {
        SidecarError::InvalidState(format!("invalid HTTP/2 response headers: {error}"))
    })
}

fn serialize_http2_headers_map(
    pseudo: BTreeMap<String, Value>,
    headers: &HeaderMap,
) -> Result<String, SidecarError> {
    let mut serialized = pseudo;
    for (name, value) in headers {
        let name = name.as_str().to_string();
        let value = Value::String(
            value
                .to_str()
                .map_err(|error| {
                    SidecarError::Execution(format!("invalid HTTP/2 header value: {error}"))
                })?
                .to_owned(),
        );
        match serialized.get_mut(&name) {
            Some(Value::Array(values)) => values.push(value),
            Some(existing) => {
                let first = existing.clone();
                *existing = Value::Array(vec![first, value]);
            }
            None => {
                serialized.insert(name, value);
            }
        }
    }
    serde_json::to_string(&serialized)
        .map_err(|error| SidecarError::host("ERR_AGENTOS_NODE_SYNC_RPC", format!("{error}")))
}

fn serialize_http2_request_headers(
    request: &Request<h2::RecvStream>,
) -> Result<String, SidecarError> {
    let mut pseudo = BTreeMap::new();
    pseudo.insert(
        String::from(":method"),
        Value::String(request.method().as_str().to_string()),
    );
    pseudo.insert(
        String::from(":path"),
        Value::String(
            request
                .uri()
                .path_and_query()
                .map(|value| value.as_str().to_string())
                .unwrap_or_else(|| String::from("/")),
        ),
    );
    serialize_http2_headers_map(pseudo, request.headers())
}

fn serialize_http2_response_headers(
    response: &Response<h2::RecvStream>,
) -> Result<String, SidecarError> {
    let mut pseudo = BTreeMap::new();
    pseudo.insert(
        String::from(":status"),
        Value::Number(serde_json::Number::from(response.status().as_u16())),
    );
    serialize_http2_headers_map(pseudo, response.headers())
}

fn commit_http2_capability(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    pending: PendingCapability,
    key: NativeCapabilityKey,
    local_id: String,
) -> Result<
    (
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::CapabilityGeneration,
    ),
    SidecarError,
> {
    let lease = pending
        .commit(CapabilityBackend::Native { local_id })
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    let identity = (lease.id(), lease.generation());
    let mut state = shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?;
    track_http2_capability(&mut state, key, lease)?;
    Ok(identity)
}

fn track_http2_capability(
    state: &mut crate::state::Http2SharedState,
    key: NativeCapabilityKey,
    lease: agentos_runtime::capability::CapabilityLease,
) -> Result<(), SidecarError> {
    match state.capability_leases.entry(key.clone()) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(lease);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(_) => Err(SidecarError::host(
            "ERR_AGENTOS_CAPABILITY_DUPLICATE",
            format!("HTTP/2 state already owns {key:?}"),
        )),
    }
}

fn admit_http2_stream(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    pending: PendingCapability,
    session_id: u64,
    reservations: Vec<Reservation>,
) -> Result<u64, SidecarError> {
    let stream_id = {
        let mut state = shared
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?;
        next_http2_stream_id(&mut state)
    };
    let lease = pending
        .commit(CapabilityBackend::Native {
            local_id: format!("http2-stream-{stream_id}"),
        })
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    let mut state = shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?;
    track_http2_capability(
        &mut state,
        NativeCapabilityKey::Http2Stream(stream_id),
        lease,
    )?;
    state.streams.insert(
        stream_id,
        ActiveHttp2Stream {
            session_id,
            paused: Arc::new(AtomicBool::new(false)),
            resume_notify: Arc::new(tokio::sync::Notify::new()),
            _reservations: reservations
                .into_iter()
                .map(SharedReservation::new)
                .collect(),
        },
    );
    Ok(stream_id)
}

fn admit_http2_session(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    pending: PendingCapability,
    command_tx: TokioSender<QueuedHttp2Command>,
    fairness: agentos_runtime::fairness::FairWorkBroker,
    reservations: Vec<Reservation>,
) -> Result<
    (
        u64,
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::CapabilityGeneration,
    ),
    SidecarError,
> {
    let session_id = {
        let mut state = shared
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?;
        next_http2_session_id(&mut state)
    };
    let lease = pending
        .commit(CapabilityBackend::Native {
            local_id: format!("http2-session-{session_id}"),
        })
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    let capability_id = lease.id();
    let capability_generation = lease.generation();
    let mut state = shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?;
    track_http2_capability(
        &mut state,
        NativeCapabilityKey::Http2Session(session_id),
        lease,
    )?;
    let resources = state.resources.as_ref().cloned().ok_or_else(|| {
        SidecarError::host(
            "ERR_AGENTOS_RUNTIME_UNAVAILABLE",
            String::from("HTTP/2 session has no VM ResourceLedger"),
        )
    })?;
    let stream_resources = Arc::new(ResourceLedger::root(
        format!("http2-session={session_id}"),
        [(
            ResourceClass::Http2Streams,
            ResourceLimit::new(
                state.limits.http2.max_streams_per_connection,
                "limits.http2.maxStreamsPerConnection",
            ),
        )],
    ));
    let vm_generation = state.vm_generation;
    let command_timeout = Duration::from_millis(state.limits.reactor.operation_deadline_ms);
    state.sessions.insert(
        session_id,
        ActiveHttp2Session {
            command_tx,
            capability_id,
            vm_generation,
            fairness,
            command_timeout,
            close_requested: Arc::new(AtomicBool::new(false)),
            close_abrupt: Arc::new(AtomicBool::new(false)),
            close_notify: Arc::new(tokio::sync::Notify::new()),
            _reservations: reservations
                .into_iter()
                .map(SharedReservation::new)
                .collect(),
            resources,
            stream_resources,
        },
    );
    Ok((session_id, capability_id, capability_generation))
}

fn reserve_http2_connection(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
) -> Result<Vec<Reservation>, SidecarError> {
    let resources = shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?
        .resources
        .as_ref()
        .cloned()
        .ok_or_else(|| {
            SidecarError::host(
                "ERR_AGENTOS_RUNTIME_UNAVAILABLE",
                String::from("HTTP/2 connection has no VM ResourceLedger"),
            )
        })?;
    reserve_http2_resources(&resources, &[(ResourceClass::Http2Connections, 1)])
}

fn reserve_http2_stream(session: &ActiveHttp2Session) -> Result<Vec<Reservation>, SidecarError> {
    let per_connection = session
        .stream_resources
        .reserve(ResourceClass::Http2Streams, 1)
        .map_err(SidecarError::from)?;
    let aggregate = session
        .resources
        .reserve(ResourceClass::Http2Streams, 1)
        .map_err(SidecarError::from)?;
    Ok(vec![per_connection, aggregate])
}

async fn reserve_http2_stream_when_available(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    session_id: u64,
) -> Result<Vec<Reservation>, SidecarError> {
    let session = shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?
        .sessions
        .get(&session_id)
        .cloned()
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("unknown HTTP/2 session {session_id}"))
        })?;
    let per_connection = session
        .stream_resources
        .reserve_when_available(ResourceClass::Http2Streams, 1)
        .await
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    let aggregate = session
        .resources
        .reserve_when_available(ResourceClass::Http2Streams, 1)
        .await
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    Ok(vec![per_connection, aggregate])
}

fn remove_http2_stream_locked(state: &mut crate::state::Http2SharedState, stream_id: u64) {
    state.streams.remove(&stream_id);
    state
        .capability_leases
        .remove(&NativeCapabilityKey::Http2Stream(stream_id));
}

fn remove_http2_session_resources(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
    session_id: u64,
) {
    let removed_session = if let Ok(mut state) = shared.lock() {
        let removed_session = state.sessions.remove(&session_id);
        // Keep terminal/error events until the guest drains them. Removing the
        // queue here loses `sessionClose` after its wake crosses the VM
        // boundary but before the guest performs its nonblocking poll.
        let stream_ids = state
            .streams
            .iter()
            .filter_map(|(stream_id, stream)| {
                (stream.session_id == session_id).then_some(*stream_id)
            })
            .collect::<Vec<_>>();
        for stream_id in stream_ids {
            remove_http2_stream_locked(&mut state, stream_id);
        }
        removed_session
    } else {
        None
    };
    if let Some(session) = removed_session {
        if let Err(error) = session
            .fairness
            .retire_capability(session.vm_generation, session.capability_id)
        {
            eprintln!("ERR_AGENTOS_HTTP2_FAIRNESS_RETIRE: session={session_id} error={error}");
        }
    }
}

pub(in crate::execution) fn terminate_http2_process_state(
    shared: &Arc<Mutex<crate::state::Http2SharedState>>,
) {
    let sessions = if let Ok(mut state) = shared.lock() {
        let sessions = std::mem::take(&mut state.sessions);
        state.server_events.clear();
        state.session_events.clear();
        state.streams.clear();
        state.servers.clear();
        state.capability_leases.clear();
        state.event_session = None;
        state.ready.notify_waiters();
        sessions
    } else {
        eprintln!("ERR_AGENTOS_HTTP2_STATE: process teardown state lock poisoned");
        return;
    };

    // Session tasks may still be blocked on transport readiness or a fair-work
    // turn. Retire their identities while teardown still owns the metadata so
    // a late task cannot recreate scheduler membership after the process dies.
    for (session_id, session) in sessions {
        session.close_abrupt.store(true, Ordering::Release);
        session.close_requested.store(true, Ordering::Release);
        session.close_notify.notify_one();
        if let Err(error) = session
            .fairness
            .retire_capability(session.vm_generation, session.capability_id)
        {
            eprintln!("ERR_AGENTOS_HTTP2_FAIRNESS_RETIRE: session={session_id} error={error}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_http2_client_session(
    runtime: agentos_runtime::RuntimeContext,
    shared: Arc<Mutex<crate::state::Http2SharedState>>,
    session_id: u64,
    remote_addr: SocketAddr,
    tls: Option<TlsBridgeOptions>,
    default_ca_bundle: Vec<u8>,
    snapshot: Arc<Mutex<Http2SessionSnapshot>>,
    mut command_rx: TokioReceiver<QueuedHttp2Command>,
) {
    let task_error_shared = Arc::clone(&shared);
    let Some(session_control) = shared
        .lock()
        .ok()
        .and_then(|state| state.sessions.get(&session_id).cloned())
    else {
        eprintln!("ERR_AGENTOS_HTTP2_STATE: missing client session {session_id}");
        return;
    };
    let vm_generation = shared.lock().map(|state| state.vm_generation).unwrap_or(0);
    let fair_runtime = runtime.clone();
    if let Err(error) = runtime.spawn(agentos_runtime::TaskClass::Http2, async move {
            let stream = match tokio::net::TcpStream::connect(remote_addr).await {
                Ok(stream) => stream,
                Err(error) => {
                    push_http2_session_event(
                        &shared,
                        session_id,
                        Http2BridgeEvent {
                            kind: String::from("sessionError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };

            let local_addr = match stream.local_addr() {
                Ok(addr) => addr,
                Err(error) => {
                    push_http2_session_event(
                        &shared,
                        session_id,
                        Http2BridgeEvent {
                            kind: String::from("sessionError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };

            {
                let mut snapshot_guard = snapshot.lock().expect("http2 snapshot lock");
                snapshot_guard.socket = http2_socket_snapshot(local_addr, remote_addr);
                if let Some(options) = tls.as_ref() {
                    snapshot_guard.encrypted = true;
                    snapshot_guard.alpn_protocol = Some(String::from("h2"));
                    snapshot_guard.socket.encrypted = true;
                    snapshot_guard.socket.servername = options.servername.clone();
                    snapshot_guard.socket.alpn_protocol = Some(String::from("h2"));
                }
                snapshot_guard.state = http2_runtime_snapshot();
            }
            if let Ok(snapshot_json) =
                http2_snapshot_json(&snapshot.lock().expect("http2 snapshot lock").clone())
            {
                push_http2_session_event(
                    &shared,
                    session_id,
                    Http2BridgeEvent {
                        kind: String::from("sessionConnect"),
                        id: session_id,
                        data: Some(snapshot_json),
                        ..Http2BridgeEvent::default()
                    },
                );
            }

            let io: Pin<Box<dyn Http2AsyncIo>> = if let Some(options) = tls.as_ref() {
                let server_name = match ServerName::try_from(
                    options
                        .servername
                        .clone()
                        .unwrap_or_else(|| String::from("localhost")),
                ) {
                    Ok(server_name) => server_name,
                    Err(_) => {
                        push_http2_session_event(
                            &shared,
                            session_id,
                            Http2BridgeEvent {
                                kind: String::from("sessionError"),
                                id: session_id,
                                data: Some(http2_error_payload("invalid TLS servername")),
                                ..Http2BridgeEvent::default()
                            },
                        );
                        remove_http2_session_resources(&shared, session_id);
                        return;
                    }
                };
                let connector = match build_client_tls_config(options, &default_ca_bundle) {
                    Ok(config) => TlsConnector::from(Arc::new(config)),
                    Err(error) => {
                        push_http2_session_event(
                            &shared,
                            session_id,
                            Http2BridgeEvent {
                                kind: String::from("sessionError"),
                                id: session_id,
                                data: Some(http2_error_payload(error.to_string())),
                                ..Http2BridgeEvent::default()
                            },
                        );
                        remove_http2_session_resources(&shared, session_id);
                        return;
                    }
                };
                match connector.connect(server_name, stream).await {
                    Ok(tls_stream) => Box::pin(tls_stream),
                    Err(error) => {
                        push_http2_session_event(
                            &shared,
                            session_id,
                            Http2BridgeEvent {
                                kind: String::from("sessionError"),
                                id: session_id,
                                data: Some(http2_error_payload(error.to_string())),
                                ..Http2BridgeEvent::default()
                            },
                        );
                        remove_http2_session_resources(&shared, session_id);
                        return;
                    }
                }
            } else {
                Box::pin(stream)
            };

            let (max_header_bytes, max_streams_per_connection, max_buffered_bytes) = shared
                .lock()
                .map(|state| {
                    (
                        state.limits.http2.max_header_bytes,
                        state.limits.http2.max_streams_per_connection,
                        state.limits.http2.max_buffered_bytes,
                    )
                })
                .unwrap_or((1, 1, 1));
            let mut builder = client::Builder::new();
            builder
                .max_header_list_size(u32::try_from(max_header_bytes).unwrap_or(u32::MAX))
                .max_concurrent_streams(
                    u32::try_from(max_streams_per_connection).unwrap_or(u32::MAX),
                )
                .initial_max_send_streams(max_streams_per_connection)
                .max_send_buffer_size(max_buffered_bytes);
            let (mut sender, connection) = match builder.handshake(io).await {
                Ok(parts) => parts,
                Err(error) => {
                    push_http2_session_event(
                        &shared,
                        session_id,
                        Http2BridgeEvent {
                            kind: String::from("sessionError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };

            let mut connection = connection;
            let mut streams = BTreeMap::<u64, ClientHttp2StreamState>::new();
            let mut stream_rotation = VecDeque::new();
            let (operation_quantum, byte_quantum) = shared
                .lock()
                .map(|state| {
                    (
                        state.limits.reactor.completion_quantum,
                        state.limits.reactor.byte_quantum,
                    )
                })
                .unwrap_or((1, 1));

            loop {
                if session_control.close_requested.load(Ordering::Acquire) {
                    push_http2_session_event(
                        &shared,
                        session_id,
                        Http2BridgeEvent {
                            kind: String::from("sessionClose"),
                            id: session_id,
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    break;
                }
                tokio::select! {
                    _ = session_control.close_notify.notified() => continue,
                    result = &mut connection => {
                        if let Err(message) = result {
                            push_http2_session_event(
                                &shared,
                                session_id,
                                Http2BridgeEvent {
                                    kind: String::from("sessionError"),
                                    id: session_id,
                                    data: Some(http2_error_payload(message.to_string())),
                                    ..Http2BridgeEvent::default()
                                },
                            );
                        }
                        push_http2_session_event(
                            &shared,
                            session_id,
                            Http2BridgeEvent {
                                kind: String::from("sessionClose"),
                                id: session_id,
                                ..Http2BridgeEvent::default()
                            },
                        );
                        remove_http2_session_resources(&shared, session_id);
                        break;
                    }
                    fair_result = run_client_http2_fair_turn(
                        &fair_runtime,
                        vm_generation,
                        session_control.capability_id,
                        &mut streams,
                        &mut stream_rotation,
                        &shared,
                        &snapshot,
                        session_id,
                        FairBudget::new(operation_quantum, byte_quantum),
                    ), if !stream_rotation.is_empty() => {
                        if let Err(error) = fair_result {
                            eprintln!("ERR_AGENTOS_HTTP2_FAIRNESS: session={session_id} error={error}");
                            break;
                        }
                    }
                    Some(queued_command) = command_rx.recv() => {
                        let QueuedHttp2Command {
                            command,
                            reservations: command_reservations,
                        } = queued_command;
                        match command {
                            Http2SessionCommand::Request { headers_json, options_json, pending_capability, stream_reservations, respond_to } => {
                                let request = match build_http2_request(&headers_json) {
                                    Ok(request) => request,
                                    Err(error) => {
                                        respond_to.settle(Err(error.to_string()));
                                        continue;
                                    }
                                };
                                let options: Http2RequestOptions = match serde_json::from_str(&options_json) {
                                    Ok(options) => options,
                                    Err(error) => {
                                        respond_to.settle(Err(format!(
                                            "ERR_INVALID_ARG_VALUE: invalid HTTP/2 request options: {error}"
                                        )));
                                        continue;
                                    }
                                };
                                let stream_id = match admit_http2_stream(
                                    &shared,
                                    pending_capability,
                                    session_id,
                                    stream_reservations,
                                ) {
                                    Ok(stream_id) => stream_id,
                                    Err(error) => {
                                        respond_to.settle(Err(error.to_string()));
                                        continue;
                                    }
                                };
                                match sender.send_request(request, options.end_stream) {
                                    Ok((response_future, send_stream)) => {
                                        streams.insert(
                                            stream_id,
                                            ClientHttp2StreamState {
                                                send_stream: (!options.end_stream)
                                                    .then_some(send_stream),
                                                pending_write: None,
                                                response: Some(response_future),
                                                recv_stream: None,
                                                pending_read: None,
                                                budget_wait: None,
                                                resume_wait: None,
                                            },
                                        );
                                        stream_rotation.push_back(stream_id);
                                        respond_to.settle(Ok(json!(stream_id)));
                                    }
                                    Err(error) => {
                                        if let Ok(mut state) = shared.lock() {
                                            remove_http2_stream_locked(&mut state, stream_id);
                                        }
                                        respond_to.settle(Err(error.to_string()));
                                    }
                                }
                            }
                            Http2SessionCommand::Settings { settings_json, respond_to } => {
                                let settings = match serde_json::from_str::<BTreeMap<String, Value>>(&settings_json) {
                                    Ok(settings) => settings,
                                    Err(error) => {
                                        respond_to.settle(Err(format!(
                                            "ERR_INVALID_ARG_VALUE: invalid HTTP/2 settings: {error}"
                                        )));
                                        continue;
                                    }
                                };
                                {
                                    let mut snapshot = snapshot.lock().expect("http2 snapshot lock");
                                    snapshot.local_settings = http2_settings_from_value(&settings);
                                }
                                let headers_json = match serde_json::to_string(&settings) {
                                    Ok(headers_json) => headers_json,
                                    Err(error) => {
                                        respond_to.settle(Err(format!(
                                            "ERR_AGENTOS_SERIALIZATION: failed to encode HTTP/2 settings: {error}"
                                        )));
                                        continue;
                                    }
                                };
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionLocalSettings"),
                                        id: session_id,
                                        data: Some(headers_json.clone()),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionSettingsAck"),
                                        id: session_id,
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                respond_to.settle(Ok(Value::Null));
                            }
                            Http2SessionCommand::SetLocalWindowSize { size, respond_to } => {
                                {
                                    let mut snapshot = snapshot.lock().expect("http2 snapshot lock");
                                    snapshot.state.local_window_size = size;
                                    snapshot.state.effective_local_window_size = size;
                                }
                                let value = snapshot
                                    .lock()
                                    .ok()
                                    .and_then(|snapshot| http2_snapshot_json(&snapshot.clone()).ok())
                                    .map(Value::String)
                                    .unwrap_or(Value::Null);
                                respond_to.settle(Ok(value));
                            }
                            Http2SessionCommand::Goaway { error_code, last_stream_id, opaque_data, respond_to } => {
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionGoaway"),
                                        id: session_id,
                                        data: opaque_data.map(|value| {
                                            base64::engine::general_purpose::STANDARD.encode(value)
                                        }),
                                        extra_number: Some(error_code as u64),
                                        flags: Some(last_stream_id as u64),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                respond_to.settle(Ok(Value::Null));
                            }
                            Http2SessionCommand::StreamWrite { stream_id, chunk, end_stream, respond_to } => {
                                let Some(stream) = streams.get_mut(&stream_id) else {
                                    respond_to.settle(Err(format!("unknown HTTP/2 client stream {stream_id}")));
                                    continue;
                                };
                                if stream.send_stream.is_none() {
                                    respond_to.settle(Err(format!("HTTP/2 client stream {stream_id} is not writable")));
                                    continue;
                                }
                                if stream.pending_write.is_some() {
                                    respond_to.settle(Err(format!("ERR_AGENTOS_HTTP2_STREAM_WRITE_PENDING: stream {stream_id}")));
                                    continue;
                                }
                                stream.pending_write = Some(PendingHttp2Write {
                                    bytes: Bytes::from(chunk),
                                    offset: 0,
                                    end_stream,
                                    respond_to,
                                    success_value: Value::Bool(true),
                                    _reservations: command_reservations,
                                });
                            }
                            Http2SessionCommand::StreamClose { stream_id, error_code, respond_to } => {
                                let Some(mut state) = streams.remove(&stream_id) else {
                                    respond_to.settle(Err(format!("unknown HTTP/2 client stream {stream_id}")));
                                    continue;
                                };
                                if let Some(stream) = state.send_stream.as_mut() {
                                    stream.send_reset(http2_reason(error_code));
                                }
                                stream_rotation.retain(|queued| *queued != stream_id);
                                if let Ok(mut state) = shared.lock() {
                                    remove_http2_stream_locked(&mut state, stream_id);
                                }
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("clientClose"),
                                        id: stream_id,
                                        extra_number: Some(u32::from(http2_reason(error_code)) as u64),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                respond_to.settle(Ok(Value::Null));
                            }
                            Http2SessionCommand::StreamRespond { respond_to, .. }
                            | Http2SessionCommand::StreamPush { respond_to, .. }
                            | Http2SessionCommand::StreamRespondWithFile { respond_to, .. } => {
                                respond_to.settle(Err(String::from("HTTP/2 client streams cannot send server responses")));
                            }
                        }
                    }
                    else => break,
                }
            }
    }) {
        eprintln!("ERR_AGENTOS_HTTP2_TASK_ADMISSION: client session {session_id}: {error}");
        remove_http2_session_resources(&task_error_shared, session_id);
    }
}

#[allow(clippy::too_many_arguments)] // one admitted HTTP/2 session's owned reactor state
fn spawn_http2_server_session(
    runtime: agentos_runtime::RuntimeContext,
    shared: Arc<Mutex<crate::state::Http2SharedState>>,
    server_id: u64,
    session_id: u64,
    stream: tokio::net::TcpStream,
    tls: Option<TlsBridgeOptions>,
    snapshot: Arc<Mutex<Http2SessionSnapshot>>,
    mut command_rx: TokioReceiver<QueuedHttp2Command>,
    capabilities: CapabilityRegistry,
) {
    let task_error_shared = Arc::clone(&shared);
    let Some(session_control) = shared
        .lock()
        .ok()
        .and_then(|state| state.sessions.get(&session_id).cloned())
    else {
        eprintln!("ERR_AGENTOS_HTTP2_STATE: missing server session {session_id}");
        return;
    };
    let vm_generation = shared.lock().map(|state| state.vm_generation).unwrap_or(0);
    let fair_runtime = runtime.clone();
    if let Err(error) = runtime.spawn(agentos_runtime::TaskClass::Http2, async move {
            let local_addr = match stream.local_addr() {
                Ok(addr) => addr,
                Err(error) => {
                    push_http2_server_event(
                        &shared,
                        server_id,
                        Http2BridgeEvent {
                            kind: String::from("serverStreamError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };
            let remote_addr = match stream.peer_addr() {
                Ok(addr) => addr,
                Err(error) => {
                    push_http2_server_event(
                        &shared,
                        server_id,
                        Http2BridgeEvent {
                            kind: String::from("serverStreamError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };
            {
                let mut snapshot_guard = snapshot.lock().expect("http2 snapshot lock");
                snapshot_guard.socket = http2_socket_snapshot(local_addr, remote_addr);
                if tls.is_some() {
                    snapshot_guard.encrypted = true;
                    snapshot_guard.alpn_protocol = Some(String::from("h2"));
                    snapshot_guard.socket.encrypted = true;
                    snapshot_guard.socket.alpn_protocol = Some(String::from("h2"));
                }
                snapshot_guard.state = http2_runtime_snapshot();
            }
            if let Ok(snapshot_json) =
                http2_snapshot_json(&snapshot.lock().expect("http2 snapshot lock").clone())
            {
                push_http2_server_event(
                    &shared,
                    server_id,
                    Http2BridgeEvent {
                        kind: String::from(if tls.is_some() {
                            "serverSecureConnection"
                        } else {
                            "serverConnection"
                        }),
                        id: server_id,
                        data: Some(serde_json::to_string(&http2_socket_snapshot(local_addr, remote_addr)).unwrap_or_else(|error| {
                            eprintln!("ERR_AGENTOS_HTTP2_SERIALIZATION: failed to encode server socket snapshot: {error}");
                            String::from("{}")
                        })),
                        ..Http2BridgeEvent::default()
                    },
                );
                push_http2_server_event(
                    &shared,
                    server_id,
                    Http2BridgeEvent {
                        kind: String::from("serverSession"),
                        id: server_id,
                        data: Some(snapshot_json),
                        extra_number: Some(session_id),
                        ..Http2BridgeEvent::default()
                    },
                );
            }

            let io: Pin<Box<dyn Http2AsyncIo>> = if let Some(options) = tls.as_ref() {
                let acceptor = match build_server_tls_config(options) {
                    Ok(config) => TlsAcceptor::from(Arc::new(config)),
                    Err(error) => {
                        push_http2_server_event(
                            &shared,
                            server_id,
                            Http2BridgeEvent {
                                kind: String::from("serverStreamError"),
                                id: session_id,
                                data: Some(http2_error_payload(error.to_string())),
                                ..Http2BridgeEvent::default()
                            },
                        );
                        remove_http2_session_resources(&shared, session_id);
                        return;
                    }
                };
                match acceptor.accept(stream).await {
                    Ok(tls_stream) => Box::pin(tls_stream),
                    Err(error) => {
                        push_http2_server_event(
                            &shared,
                            server_id,
                            Http2BridgeEvent {
                                kind: String::from("serverStreamError"),
                                id: session_id,
                                data: Some(http2_error_payload(error.to_string())),
                                ..Http2BridgeEvent::default()
                            },
                        );
                        remove_http2_session_resources(&shared, session_id);
                        return;
                    }
                }
            } else {
                Box::pin(stream)
            };

            let (max_header_bytes, max_streams_per_connection, max_buffered_bytes) = shared
                .lock()
                .map(|state| {
                    (
                        state.limits.http2.max_header_bytes,
                        state.limits.http2.max_streams_per_connection,
                        state.limits.http2.max_buffered_bytes,
                    )
                })
                .unwrap_or((1, 1, 1));
            let mut builder = server::Builder::new();
            builder
                .max_header_list_size(u32::try_from(max_header_bytes).unwrap_or(u32::MAX))
                .max_concurrent_streams(
                    u32::try_from(max_streams_per_connection).unwrap_or(u32::MAX),
                )
                .max_send_buffer_size(max_buffered_bytes);
            let mut connection = match builder.handshake(io).await {
                Ok(connection) => connection,
                Err(error) => {
                    push_http2_server_event(
                        &shared,
                        server_id,
                        Http2BridgeEvent {
                            kind: String::from("serverStreamError"),
                            id: session_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    return;
                }
            };

            let mut streams = BTreeMap::<u64, ServerHttp2StreamState>::new();
            let mut stream_rotation = VecDeque::new();
            let (operation_quantum, byte_quantum) = shared
                .lock()
                .map(|state| {
                    (
                        state.limits.reactor.completion_quantum,
                        state.limits.reactor.byte_quantum,
                    )
                })
                .unwrap_or((1, 1));
            let mut pending_inbound = None;

            loop {
                if session_control.close_requested.load(Ordering::Acquire) {
                    if session_control.close_abrupt.load(Ordering::Acquire) {
                        connection.abrupt_shutdown(Reason::CANCEL);
                    } else {
                        connection.graceful_shutdown();
                    }
                    push_http2_session_event(
                        &shared,
                        session_id,
                        Http2BridgeEvent {
                            kind: String::from("sessionClose"),
                            id: session_id,
                            ..Http2BridgeEvent::default()
                        },
                    );
                    remove_http2_session_resources(&shared, session_id);
                    break;
                }
                tokio::select! {
                    _ = session_control.close_notify.notified() => continue,
                    admission = async {
                        let reservations = reserve_http2_stream_when_available(&shared, session_id).await?;
                        let capability = capabilities
                            .reserve_when_available(CapabilityKind::Http2Stream)
                            .await
                            .map_err(|error| SidecarError::Execution(error.to_string()))?;
                        Ok::<_, SidecarError>((capability, reservations))
                    }, if pending_inbound.is_none() => {
                        match admission {
                            Ok(pending) => pending_inbound = Some(pending),
                            Err(error) => {
                                push_http2_server_event(
                                    &shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("serverStreamError"),
                                        id: session_id,
                                        data: Some(http2_error_payload(error.to_string())),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                break;
                            }
                        }
                    }
                    incoming = connection.accept(), if pending_inbound.is_some() => {
                        let (pending_capability, stream_reservations) = pending_inbound
                            .take()
                            .expect("HTTP/2 accept is gated by pending stream admission");
                        match incoming {
                            Some(Ok((request, respond))) => {
                                let headers_json = match serialize_http2_request_headers(&request) {
                                    Ok(headers) => headers,
                                    Err(error) => {
                                        push_http2_server_event(
                                            &shared,
                                            server_id,
                                            Http2BridgeEvent {
                                                kind: String::from("serverStreamError"),
                                                id: server_id,
                                                data: Some(http2_error_payload(error.to_string())),
                                                ..Http2BridgeEvent::default()
                                            },
                                        );
                                        continue;
                                    }
                                };
                                let stream_id = match admit_http2_stream(
                                    &shared,
                                    pending_capability,
                                    session_id,
                                    stream_reservations,
                                ) {
                                    Ok(stream_id) => stream_id,
                                    Err(error) => {
                                        push_http2_server_event(
                                            &shared,
                                            server_id,
                                            Http2BridgeEvent {
                                                kind: String::from("serverStreamError"),
                                                id: session_id,
                                                data: Some(http2_error_payload(error.to_string())),
                                                ..Http2BridgeEvent::default()
                                            },
                                        );
                                        continue;
                                    }
                                };
                                let recv_stream = request.into_body();
                                streams.insert(
                                    stream_id,
                                    ServerHttp2StreamState {
                                        send_response: Some(ServerHttp2Responder::Regular(respond)),
                                        send_stream: None,
                                        pending_write: None,
                                        recv_stream: Some(recv_stream),
                                        pending_read: None,
                                        budget_wait: None,
                                        resume_wait: None,
                                    },
                                );
                                stream_rotation.push_back(stream_id);
                                let snapshot_json = snapshot
                                    .lock()
                                    .ok()
                                    .and_then(|snapshot| http2_snapshot_json(&snapshot.clone()).ok());
                                push_http2_server_event(
                                    &shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("serverStream"),
                                        id: server_id,
                                        data: Some(stream_id.to_string()),
                                        extra: snapshot_json,
                                        extra_number: Some(session_id),
                                        extra_headers: Some(headers_json),
                                        flags: Some(0),
                                    },
                                );
                            }
                            Some(Err(error)) => {
                                push_http2_server_event(
                                    &shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("serverStreamError"),
                                        id: server_id,
                                        data: Some(http2_error_payload(error.to_string())),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                break;
                            }
                            None => {
                                push_http2_server_event(
                                    &shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionClose"),
                                        id: session_id,
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                remove_http2_session_resources(&shared, session_id);
                                break;
                            }
                        }
                    }
                    fair_result = run_server_http2_fair_turn(
                        &fair_runtime,
                        vm_generation,
                        session_control.capability_id,
                        &mut streams,
                        &mut stream_rotation,
                        &shared,
                        server_id,
                        FairBudget::new(operation_quantum, byte_quantum),
                    ), if !stream_rotation.is_empty() => {
                        if let Err(error) = fair_result {
                            eprintln!("ERR_AGENTOS_HTTP2_FAIRNESS: session={session_id} error={error}");
                            break;
                        }
                    }
                    Some(queued_command) = command_rx.recv() => {
                        let QueuedHttp2Command {
                            command,
                            reservations: command_reservations,
                        } = queued_command;
                        match command {
                            Http2SessionCommand::Settings { settings_json, respond_to } => {
                                let settings = match serde_json::from_str::<BTreeMap<String, Value>>(&settings_json) {
                                    Ok(settings) => settings,
                                    Err(error) => {
                                        respond_to.settle(Err(format!(
                                            "ERR_INVALID_ARG_VALUE: invalid HTTP/2 settings: {error}"
                                        )));
                                        continue;
                                    }
                                };
                                if let Some(initial_window_size) = settings
                                    .get("initialWindowSize")
                                    .and_then(Value::as_u64)
                                {
                                    let initial_window_size = match u32::try_from(initial_window_size) {
                                        Ok(initial_window_size) => initial_window_size,
                                        Err(error) => {
                                            respond_to.settle(Err(format!(
                                                "ERR_OUT_OF_RANGE: HTTP/2 initialWindowSize does not fit u32: {error}"
                                            )));
                                            continue;
                                        }
                                    };
                                    if let Err(error) = connection.set_initial_window_size(initial_window_size) {
                                        respond_to.settle(Err(format!(
                                            "ERR_HTTP2_INVALID_SETTING_VALUE: failed to apply initialWindowSize: {error}"
                                        )));
                                        continue;
                                    }
                                }
                                {
                                    let mut snapshot = snapshot.lock().expect("http2 snapshot lock");
                                    snapshot.local_settings = http2_settings_from_value(&settings);
                                }
                                let headers_json = match serde_json::to_string(&settings) {
                                    Ok(headers_json) => headers_json,
                                    Err(error) => {
                                        respond_to.settle(Err(format!(
                                            "ERR_AGENTOS_SERIALIZATION: failed to encode HTTP/2 settings: {error}"
                                        )));
                                        continue;
                                    }
                                };
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionLocalSettings"),
                                        id: session_id,
                                        data: Some(headers_json),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                respond_to.settle(Ok(Value::Null));
                            }
                            Http2SessionCommand::SetLocalWindowSize { size, respond_to } => {
                                connection.set_target_window_size(size);
                                {
                                    let mut snapshot = snapshot.lock().expect("http2 snapshot lock");
                                    snapshot.state.local_window_size = size;
                                    snapshot.state.effective_local_window_size = size;
                                }
                                let value = snapshot
                                    .lock()
                                    .ok()
                                    .and_then(|snapshot| http2_snapshot_json(&snapshot.clone()).ok())
                                    .map(Value::String)
                                    .unwrap_or(Value::Null);
                                respond_to.settle(Ok(value));
                            }
                            Http2SessionCommand::Goaway { error_code, last_stream_id, opaque_data, respond_to } => {
                                connection.abrupt_shutdown(http2_reason(Some(error_code)));
                                push_http2_session_event(
                                    &shared,
                                    session_id,
                                    Http2BridgeEvent {
                                        kind: String::from("sessionGoaway"),
                                        id: session_id,
                                        data: opaque_data.map(|value| {
                                            base64::engine::general_purpose::STANDARD.encode(value)
                                        }),
                                        extra_number: Some(error_code as u64),
                                        flags: Some(last_stream_id as u64),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                respond_to.settle(Ok(Value::Null));
                            }
                            Http2SessionCommand::StreamRespond { stream_id, headers_json, respond_to } => {
                                let response = match build_http2_response(&headers_json) {
                                    Ok(response) => response,
                                    Err(error) => {
                                        respond_to.settle(Err(error.to_string()));
                                        continue;
                                    }
                                };
                                let Some(state) = streams.get_mut(&stream_id) else {
                                    respond_to.settle(Err(format!("unknown HTTP/2 server stream {stream_id}")));
                                    continue;
                                };
                                let Some(send_response) = state.send_response.as_mut() else {
                                    respond_to.settle(Err(format!("HTTP/2 server stream {stream_id} already responded")));
                                    continue;
                                };
                                match match send_response {
                                    ServerHttp2Responder::Regular(send_response) => {
                                        send_response.send_response(response, false)
                                    }
                                    ServerHttp2Responder::Pushed(send_response) => {
                                        send_response.send_response(response, false)
                                    }
                                } {
                                    Ok(send_stream) => {
                                        state.send_stream = Some(send_stream);
                                        state.send_response = None;
                                        respond_to.settle(Ok(Value::Null));
                                    }
                                    Err(error) => {
                                        respond_to.settle(Err(error.to_string()));
                                    }
                                }
                            }
                            Http2SessionCommand::StreamPush { stream_id, headers_json, pending_capability, stream_reservations, respond_to } => {
                                let request = match build_http2_request(&headers_json) {
                                    Ok(request) => request,
                                    Err(error) => {
                                        respond_to.settle(Err(error.to_string()));
                                        continue;
                                    }
                                };
                                let Some(state) = streams.get_mut(&stream_id) else {
                                    respond_to.settle(Err(format!("unknown HTTP/2 server stream {stream_id}")));
                                    continue;
                                };
                                let Some(send_response) = state.send_response.as_mut() else {
                                    respond_to.settle(Err(format!("HTTP/2 server stream {stream_id} cannot push after responding")));
                                    continue;
                                };
                                let ServerHttp2Responder::Regular(send_response) = send_response else {
                                    respond_to.settle(Err(format!("HTTP/2 pushed stream {stream_id} cannot create nested push promises")));
                                    continue;
                                };
                                match send_response.push_request(request) {
                                    Ok(pushed) => {
                                        let pushed_stream_id = match admit_http2_stream(
                                            &shared,
                                            pending_capability,
                                            session_id,
                                            stream_reservations,
                                        ) {
                                            Ok(stream_id) => stream_id,
                                            Err(error) => {
                                                respond_to.settle(Err(error.to_string()));
                                                continue;
                                            }
                                        };
                                        streams.insert(
                                            pushed_stream_id,
                                            ServerHttp2StreamState {
                                                send_response: Some(ServerHttp2Responder::Pushed(pushed)),
                                                send_stream: None,
                                                pending_write: None,
                                                recv_stream: None,
                                                pending_read: None,
                                                budget_wait: None,
                                                resume_wait: None,
                                            },
                                        );
                                        stream_rotation.push_back(pushed_stream_id);
                                        respond_to.settle(Ok(json!({
                                            "streamId": pushed_stream_id,
                                            "headers": headers_json,
                                        }).to_string().into()));
                                    }
                                    Err(error) => {
                                        respond_to.settle(Err(error.to_string()));
                                    }
                                }
                            }
                            Http2SessionCommand::StreamWrite { stream_id, chunk, end_stream, respond_to } => {
                                let Some(state) = streams.get_mut(&stream_id) else {
                                    respond_to.settle(Err(format!("unknown HTTP/2 server stream {stream_id}")));
                                    continue;
                                };
                                if state.send_stream.is_none() {
                                    respond_to.settle(Err(format!("HTTP/2 server stream {stream_id} has not sent response headers")));
                                    continue;
                                }
                                if state.pending_write.is_some() {
                                    respond_to.settle(Err(format!("ERR_AGENTOS_HTTP2_STREAM_WRITE_PENDING: stream {stream_id}")));
                                    continue;
                                }
                                state.pending_write = Some(PendingHttp2Write {
                                    bytes: Bytes::from(chunk),
                                    offset: 0,
                                    end_stream,
                                    respond_to,
                                    success_value: Value::Bool(true),
                                    _reservations: command_reservations,
                                });
                            }
                            Http2SessionCommand::StreamClose { stream_id, error_code, respond_to } => {
                                let Some(mut state) = streams.remove(&stream_id) else {
                                    respond_to.settle(Err(format!("unknown HTTP/2 server stream {stream_id}")));
                                    continue;
                                };
                                let reason = http2_reason(error_code);
                                if let Some(send_stream) = state.send_stream.as_mut() {
                                    send_stream.send_reset(reason);
                                }
                                if let Some(send_response) = state.send_response.as_mut() {
                                    match send_response {
                                        ServerHttp2Responder::Regular(send_response) => {
                                            send_response.send_reset(reason)
                                        }
                                        ServerHttp2Responder::Pushed(send_response) => {
                                            send_response.send_reset(reason)
                                        }
                                    }
                                }
                                if let Ok(mut shared_guard) = shared.lock() {
                                    remove_http2_stream_locked(&mut shared_guard, stream_id);
                                }
                                push_http2_server_event(
                                    &shared,
                                    server_id,
                                    Http2BridgeEvent {
                                        kind: String::from("serverStreamClose"),
                                        id: stream_id,
                                        extra_number: Some(u32::from(reason) as u64),
                                        ..Http2BridgeEvent::default()
                                    },
                                );
                                respond_to.settle(Ok(Value::Null));
                            }
                            Http2SessionCommand::StreamRespondWithFile { stream_id, body, headers_json, options_json, respond_to } => {
                                let options: Http2FileResponseOptions = match serde_json::from_str(&options_json) {
                                    Ok(options) => options,
                                    Err(error) => {
                                        respond_to.settle(Err(format!(
                                            "ERR_INVALID_ARG_VALUE: invalid HTTP/2 file response options: {error}"
                                        )));
                                        continue;
                                    }
                                };
                                let response = match build_http2_response(&headers_json) {
                                    Ok(response) => response,
                                    Err(error) => {
                                        respond_to.settle(Err(error.to_string()));
                                        continue;
                                    }
                                };
                                let offset = match usize::try_from(options.offset.unwrap_or_default()) {
                                    Ok(offset) => offset.min(body.len()),
                                    Err(error) => {
                                        respond_to.settle(Err(format!(
                                            "ERR_OUT_OF_RANGE: HTTP/2 file response offset does not fit usize: {error}"
                                        )));
                                        continue;
                                    }
                                };
                                let available = body.len().saturating_sub(offset);
                                let length = match options.length {
                                    Some(length) if length >= 0 => match usize::try_from(length) {
                                        Ok(length) => available.min(length),
                                        Err(error) => {
                                            respond_to.settle(Err(format!(
                                                "ERR_OUT_OF_RANGE: HTTP/2 file response length does not fit usize: {error}"
                                            )));
                                            continue;
                                        }
                                    },
                                    _ => available,
                                };
                                let body = Bytes::from(body).slice(offset..offset.saturating_add(length));
                                let Some(state) = streams.get_mut(&stream_id) else {
                                    respond_to.settle(Err(format!("unknown HTTP/2 server stream {stream_id}")));
                                    continue;
                                };
                                let Some(send_response) = state.send_response.as_mut() else {
                                    respond_to.settle(Err(format!("HTTP/2 server stream {stream_id} already responded")));
                                    continue;
                                };
                                match match send_response {
                                    ServerHttp2Responder::Regular(send_response) => {
                                        send_response.send_response(response, body.is_empty())
                                    }
                                    ServerHttp2Responder::Pushed(send_response) => {
                                        send_response.send_response(response, body.is_empty())
                                    }
                                } {
                                    Ok(send_stream) => {
                                        state.send_response = None;
                                        if body.is_empty() {
                                            state.send_stream = None;
                                            push_http2_server_event(
                                                &shared,
                                                server_id,
                                                Http2BridgeEvent {
                                                    kind: String::from("serverStreamClose"),
                                                    id: stream_id,
                                                    extra_number: Some(0),
                                                    ..Http2BridgeEvent::default()
                                                },
                                            );
                                            respond_to.settle(Ok(Value::Null));
                                        } else {
                                            state.send_stream = Some(send_stream);
                                            state.pending_write = Some(PendingHttp2Write {
                                                bytes: body,
                                                offset: 0,
                                                end_stream: true,
                                                respond_to,
                                                success_value: Value::Null,
                                                _reservations: command_reservations,
                                            });
                                        }
                                    }
                                    Err(error) => {
                                        respond_to.settle(Err(error.to_string()));
                                    }
                                }
                            }
                            Http2SessionCommand::Request { respond_to, .. } => {
                                respond_to.settle(Err(String::from("HTTP/2 server sessions cannot initiate client requests")));
                            }
                        }
                    }
                    else => break,
                }
            }
    }) {
        eprintln!("ERR_AGENTOS_HTTP2_TASK_ADMISSION: server session {session_id}: {error}");
        remove_http2_session_resources(&task_error_shared, session_id);
    }
}

fn spawn_http2_server_accept_loop(
    runtime: agentos_runtime::RuntimeContext,
    shared: Arc<Mutex<crate::state::Http2SharedState>>,
    server_id: u64,
    listener: TcpListener,
    close_notify: Arc<tokio::sync::Notify>,
    capabilities: CapabilityRegistry,
) {
    if let Err(error) = listener.set_nonblocking(true) {
        push_http2_server_event(
            &shared,
            server_id,
            Http2BridgeEvent {
                kind: String::from("serverStreamError"),
                id: server_id,
                data: Some(http2_error_payload(error.to_string())),
                ..Http2BridgeEvent::default()
            },
        );
        return;
    }
    let resources = match shared
        .lock()
        .ok()
        .and_then(|state| state.resources.as_ref().cloned())
    {
        Some(resources) => resources,
        None => {
            eprintln!(
                "ERR_AGENTOS_RUNTIME_UNAVAILABLE: HTTP/2 accept task has no VM ResourceLedger"
            );
            return;
        }
    };
    let task_error_shared = Arc::clone(&shared);
    let child_runtime = runtime.clone();
    if let Err(error) = runtime.spawn(agentos_runtime::TaskClass::Listener, async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                push_http2_server_event(
                    &shared,
                    server_id,
                    Http2BridgeEvent {
                        kind: String::from("serverStreamError"),
                        id: server_id,
                        data: Some(http2_error_payload(error.to_string())),
                        ..Http2BridgeEvent::default()
                    },
                );
                return;
            }
        };
        loop {
            let connection_reservation = tokio::select! {
                biased;
                _ = close_notify.notified() => break,
                admission = resources.reserve_when_available(ResourceClass::Http2Connections, 1) => {
                    match admission {
                        Ok(reservation) => reservation,
                        Err(error) => {
                            push_http2_server_event(
                                &shared,
                                server_id,
                                Http2BridgeEvent {
                                    kind: String::from("serverStreamError"),
                                    id: server_id,
                                    data: Some(http2_error_payload(error.to_string())),
                                    ..Http2BridgeEvent::default()
                                },
                            );
                            break;
                        }
                    }
                }
            };
            let pending_capability = tokio::select! {
                biased;
                _ = close_notify.notified() => break,
                admission = capabilities.reserve_when_available(CapabilityKind::Http2Connection) => {
                    match admission {
                        Ok(pending) => pending,
                        Err(error) => {
                            push_http2_server_event(
                                &shared,
                                server_id,
                                Http2BridgeEvent {
                                    kind: String::from("serverStreamError"),
                                    id: server_id,
                                    data: Some(http2_error_payload(error.to_string())),
                                    ..Http2BridgeEvent::default()
                                },
                            );
                            break;
                        }
                    }
                }
            };
            let accepted = tokio::select! {
                biased;
                _ = close_notify.notified() => break,
                accepted = listener.accept() => accepted,
            };
            match accepted {
                Ok((stream, _)) => {
                    let (guest_local_addr, secure, tls, command_limit) = {
                        let state = shared.lock().expect("http2 shared state");
                        let server = state.servers.get(&server_id).expect("http2 server state");
                        (
                            server.guest_local_addr,
                            server.secure,
                            server.tls.clone(),
                            state.limits.http2.max_pending_commands,
                        )
                    };
                    let (command_tx, command_rx) = tokio_channel(command_limit);
                    let (local_addr, remote_addr) = match (stream.local_addr(), stream.peer_addr())
                    {
                        (Ok(local_addr), Ok(remote_addr)) => (local_addr, remote_addr),
                        _ => continue,
                    };
                    let session_snapshot = Arc::new(Mutex::new(Http2SessionSnapshot {
                        encrypted: secure,
                        alpn_protocol: Some(if secure {
                            String::from("h2")
                        } else {
                            String::from("h2c")
                        }),
                        local_settings: BTreeMap::new(),
                        remote_settings: BTreeMap::new(),
                        state: http2_runtime_snapshot(),
                        socket: Http2SocketSnapshot {
                            local_address: Some(guest_local_addr.ip().to_string()),
                            local_port: Some(guest_local_addr.port()),
                            local_family: Some(socket_addr_family(&guest_local_addr).to_string()),
                            remote_address: Some(remote_addr.ip().to_string()),
                            remote_port: Some(remote_addr.port()),
                            remote_family: Some(socket_addr_family(&remote_addr).to_string()),
                            ..http2_socket_snapshot(local_addr, remote_addr)
                        },
                        ..Http2SessionSnapshot::default()
                    }));
                    let (session_id, capability_id, capability_generation) = match admit_http2_session(
                        &shared,
                        pending_capability,
                        command_tx,
                        child_runtime.fairness().clone(),
                        vec![connection_reservation],
                    ) {
                        Ok(identity) => identity,
                        Err(error) => {
                            push_http2_server_event(
                                &shared,
                                server_id,
                                Http2BridgeEvent {
                                    kind: String::from("serverStreamError"),
                                    id: server_id,
                                    data: Some(http2_error_payload(error.to_string())),
                                    ..Http2BridgeEvent::default()
                                },
                            );
                            continue;
                        }
                    };
                    {
                        let mut state = session_snapshot.lock().expect("http2 snapshot lock");
                        state.capability_id = Some(capability_id);
                        state.capability_generation = Some(capability_generation);
                    }
                    spawn_http2_server_session(
                        child_runtime.clone(),
                        Arc::clone(&shared),
                        server_id,
                        session_id,
                        stream,
                        tls,
                        session_snapshot,
                        command_rx,
                        capabilities.clone(),
                    );
                }
                Err(error) => {
                    push_http2_server_event(
                        &shared,
                        server_id,
                        Http2BridgeEvent {
                            kind: String::from("serverStreamError"),
                            id: server_id,
                            data: Some(http2_error_payload(error.to_string())),
                            ..Http2BridgeEvent::default()
                        },
                    );
                    // Tokio readiness is level-triggered; retrying a permanent
                    // accept error would spin. Fail the listener task and let
                    // the guest explicitly create a replacement server.
                    break;
                }
            }
        }
    }) {
        eprintln!("ERR_AGENTOS_HTTP2_TASK_ADMISSION: server accept {server_id}: {error}");
        push_http2_server_event(
            &task_error_shared,
            server_id,
            Http2BridgeEvent {
                kind: String::from("serverStreamError"),
                id: server_id,
                data: Some(http2_error_payload(error.to_string())),
                ..Http2BridgeEvent::default()
            },
        );
    }
}

fn send_http2_command(
    session: &ActiveHttp2Session,
    command_bytes: usize,
    header_bytes: usize,
    data_bytes: usize,
    command: impl FnOnce(Http2ResponseSender) -> Result<Http2SessionCommand, SidecarError>,
) -> Result<HostServiceResponse, SidecarError> {
    let (respond_to, response_rx) = tokio::sync::oneshot::channel();
    let respond_to = Http2ResponseSender::new(respond_to);
    let reservations = reserve_http2_resources(
        &session.resources,
        &[
            (ResourceClass::Http2Commands, 1),
            (ResourceClass::Http2CommandBytes, command_bytes),
            (ResourceClass::Http2BufferedBytes, command_bytes),
            (ResourceClass::BufferedBytes, command_bytes),
            (ResourceClass::Http2HeaderBytes, header_bytes),
            (ResourceClass::Http2DataBytes, data_bytes),
        ],
    )?;
    // Construct/clone the command payload only after every relevant count and
    // byte domain has admitted it.
    let command = command(respond_to)?;
    session
        .command_tx
        .try_send(QueuedHttp2Command {
            command,
            reservations,
        })
        .map_err(|error| match error {
            tokio::sync::mpsc::error::TrySendError::Full(_) => SidecarError::host(
                "ERR_AGENTOS_HTTP2_COMMAND_LIMIT",
                String::from(
                    "HTTP/2 session command queue is full; raise limits.http2.maxPendingCommands",
                ),
            ),
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                SidecarError::InvalidState(String::from("HTTP/2 session command channel closed"))
            }
        })?;
    Ok(HostServiceResponse::Deferred {
        receiver: response_rx,
        timeout: Some(session.command_timeout),
        task_class: agentos_runtime::TaskClass::Http2,
    })
}

fn read_http2_response_file_after_admission(
    kernel: &mut SidecarKernel,
    requester_pid: u32,
    guest_path: &str,
    admitted_bytes: usize,
) -> Result<Vec<u8>, SidecarError> {
    let body = kernel
        .read_file_for_process(EXECUTION_DRIVER_NAME, requester_pid, guest_path)
        .map_err(kernel_error)?;
    if body.len() != admitted_bytes {
        return Err(SidecarError::host(
            "ERR_AGENTOS_HTTP2_FILE_CHANGED",
            format!(
                "response file size changed after admission: {guest_path} (admitted {admitted_bytes} bytes, read {} bytes)",
                body.len()
            ),
        ));
    }
    Ok(body)
}

fn parse_http2_server_listen_payload(
    request: &HostRpcRequest,
) -> Result<Http2ServerListenOptions, SidecarError> {
    let payload_json =
        javascript_sync_rpc_arg_str(&request.args, 0, "net.http2_server_listen payload")?;
    serde_json::from_str(payload_json).map_err(|error| {
        SidecarError::InvalidState(format!(
            "net.http2_server_listen payload must be valid JSON: {error}"
        ))
    })
}

fn parse_http2_connect_payload(
    request: &HostRpcRequest,
) -> Result<Http2SessionConnectOptions, SidecarError> {
    let payload_json =
        javascript_sync_rpc_arg_str(&request.args, 0, "net.http2_session_connect payload")?;
    serde_json::from_str(payload_json).map_err(|error| {
        SidecarError::InvalidState(format!(
            "net.http2_session_connect payload must be valid JSON: {error}"
        ))
    })
}

fn http2_session_for_id(
    process: &ActiveProcess,
    session_id: u64,
) -> Result<ActiveHttp2Session, SidecarError> {
    let shared = process
        .http2
        .shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?;
    shared
        .sessions
        .get(&session_id)
        .cloned()
        .ok_or_else(|| SidecarError::InvalidState(format!("unknown HTTP/2 session {session_id}")))
}

fn http2_stream_for_id(
    process: &ActiveProcess,
    stream_id: u64,
) -> Result<ActiveHttp2Stream, SidecarError> {
    let shared = process
        .http2
        .shared
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned")))?;
    shared
        .streams
        .get(&stream_id)
        .cloned()
        .ok_or_else(|| SidecarError::InvalidState(format!("unknown HTTP/2 stream {stream_id}")))
}

pub(in crate::execution) fn service_javascript_http2_sync_rpc<B>(
    request: Http2ServiceRequest<'_, B>,
) -> Result<HostServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let Http2ServiceRequest {
        bridge,
        kernel,
        vm_id,
        dns,
        socket_paths,
        process,
        sync_request: request,
        capabilities,
    } = request;
    {
        let mut state =
            process.http2.shared.lock().map_err(|_| {
                SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned"))
            })?;
        state.resources = Some(Arc::clone(process.runtime_context.resources()));
        state.limits = process.limits.clone();
        state.vm_generation = capabilities.session_generation();
    }
    let response = match request.method.as_str() {
        "net.http2_server_listen" => {
            let payload = parse_http2_server_listen_payload(request)?;
            let (family, bind_host, guest_host) =
                normalize_tcp_listen_host(payload.host.as_deref())?;
            let requested_port = payload.port.unwrap_or(0);
            bridge.require_network_access(
                vm_id,
                NetworkOperation::Listen,
                format_tcp_resource(bind_host, requested_port),
            )?;
            let pending = reserve_capability(&capabilities, CapabilityKind::TcpListener)?;
            let port = allocate_guest_listen_port(
                requested_port,
                family,
                &socket_paths.used_tcp_guest_ports,
                socket_paths.listen_policy,
            )?;
            let mut listener =
                ActiveTcpListener::bind(bind_host, guest_host, port, payload.backlog)?;
            let guest_local_addr = listener.guest_local_addr();
            let closed = Arc::new(AtomicBool::new(false));
            let close_notify = Arc::new(tokio::sync::Notify::new());
            let identity = commit_http2_capability(
                &process.http2.shared,
                pending,
                NativeCapabilityKey::Http2Server(payload.server_id),
                format!("http2-server-{}", payload.server_id),
            )?;
            {
                let mut state = process.http2.shared.lock().map_err(|_| {
                    SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned"))
                })?;
                state.servers.insert(
                    payload.server_id,
                    ActiveHttp2Server {
                        actual_local_addr: listener.local_addr(),
                        guest_local_addr,
                        secure: payload.secure,
                        tls: payload.tls.clone().map(|mut tls| {
                            tls.is_server = payload.secure;
                            if payload.secure && tls.alpn_protocols.is_none() {
                                tls.alpn_protocols = Some(vec![String::from("h2")]);
                            }
                            tls
                        }),
                        closed: Arc::clone(&closed),
                        close_notify: Arc::clone(&close_notify),
                    },
                );
                if state.event_session.is_none() {
                    state.event_session = process
                        .execution
                        .execution_wake_handle(process.kernel_handle.runtime_identity());
                }
                state.server_events.entry(payload.server_id).or_default();
            }
            spawn_http2_server_accept_loop(
                process.runtime_context.clone(),
                Arc::clone(&process.http2.shared),
                payload.server_id,
                listener.listener.take().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "HTTP/2 listener missing host TCP socket",
                    ))
                })?,
                close_notify,
                capabilities.clone(),
            );
            encode_net_json_string(
                json!({
                    "address": socket_address_value(&guest_local_addr),
                    "capabilityId": identity.0,
                    "capabilityGeneration": identity.1,
                }),
                "net.http2_server_listen",
            )
        }
        "net.http2_server_poll" => {
            let server_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_server_poll server id")?;
            let wait_ms = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "net.http2_server_poll wait ms",
            )?
            .unwrap_or_default();
            return defer_http2_poll(process, server_id, true, wait_ms);
        }
        "net.http2_server_wait" => {
            let server_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_server_wait server id")?;
            return defer_http2_wait(process, server_id, true);
        }
        "net.http2_server_close" => {
            let server_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_server_close server id")?;
            let server = {
                let mut state = process.http2.shared.lock().map_err(|_| {
                    SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned"))
                })?;
                state.servers.remove(&server_id)
            }
            .ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown HTTP/2 server {server_id}"))
            })?;
            server.closed.store(true, Ordering::SeqCst);
            server.close_notify.notify_waiters();
            push_http2_server_event(
                &process.http2.shared,
                server_id,
                Http2BridgeEvent {
                    kind: String::from("serverClose"),
                    id: server_id,
                    ..Http2BridgeEvent::default()
                },
            );
            Ok(Value::Null)
        }
        "net.http2_server_respond" => {
            let server_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_server_respond server id",
            )?;
            let request_id = javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "net.http2_server_respond request id",
            )?;
            let response_json =
                javascript_sync_rpc_arg_str(&request.args, 2, "net.http2_server_respond payload")?;
            ensure_vm_fetch_response_within_limit(
                response_json,
                "net.http2_server_respond",
                VM_FETCH_BUFFER_LIMIT_BYTES,
            )
            .map_err(sidecar_core_execution_error)?;
            serde_json::from_str::<Value>(response_json).map_err(|error| {
                SidecarError::Execution(format!(
                    "net.http2_server_respond payload must be valid JSON: {error}"
                ))
            })?;
            complete_loopback_http_request(
                process,
                (server_id, request_id),
                response_json.to_owned(),
            )?;
            Ok(Value::Bool(true))
        }
        "net.http2_session_connect" => {
            let payload = parse_http2_connect_payload(request)?;
            let authority = payload.authority.clone().unwrap_or_else(|| {
                format!(
                    "{}://{}:{}",
                    payload.protocol.as_deref().unwrap_or("http"),
                    payload.host.as_deref().unwrap_or("localhost"),
                    payload.port.unwrap_or(80)
                )
            });
            let url = Url::parse(&authority).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid HTTP/2 authority {authority:?}: {error}"
                ))
            })?;
            let secure = url.scheme() == "https" || payload.protocol.as_deref() == Some("https:");
            let host = payload
                .host
                .as_deref()
                .or_else(|| url.host_str())
                .unwrap_or("localhost");
            let port = payload.port.or_else(|| url.port()).unwrap_or(80);
            bridge.require_network_access(
                vm_id,
                NetworkOperation::Http,
                format_tcp_resource(host, port),
            )?;
            let pending = reserve_capability(&capabilities, CapabilityKind::Http2Connection)?;
            let connection_reservations = reserve_http2_connection(&process.http2.shared)?;
            let resolved = {
                let shared = process.http2.shared.lock().map_err(|_| {
                    SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned"))
                })?;
                shared
                    .servers
                    .values()
                    .find(|server| {
                        is_loopback_request_host(host) && server.guest_local_addr.port() == port
                    })
                    .map(|server| ResolvedTcpConnectAddr {
                        actual_addr: server.actual_local_addr,
                        guest_remote_addr: server.guest_local_addr,
                        use_kernel_loopback: false,
                    })
            };
            let resolved = match resolved {
                Some(resolved) => resolved,
                None => resolve_tcp_connect_addr(
                    bridge,
                    kernel,
                    vm_id,
                    dns,
                    host,
                    port,
                    None,
                    socket_paths,
                )?,
            };
            let (command_tx, command_rx) = tokio_channel(process.limits.http2.max_pending_commands);
            let snapshot = Arc::new(Mutex::new(Http2SessionSnapshot {
                encrypted: secure,
                alpn_protocol: Some(String::from(if secure { "h2" } else { "h2c" })),
                local_settings: http2_settings_from_value(&payload.settings),
                remote_settings: BTreeMap::new(),
                state: http2_runtime_snapshot(),
                socket: Http2SocketSnapshot {
                    encrypted: secure,
                    remote_address: Some(resolved.guest_remote_addr.ip().to_string()),
                    remote_port: Some(resolved.guest_remote_addr.port()),
                    remote_family: Some(
                        socket_addr_family(&resolved.guest_remote_addr).to_string(),
                    ),
                    servername: if secure {
                        payload
                            .tls
                            .as_ref()
                            .and_then(|tls| tls.servername.clone())
                            .or_else(|| Some(host.to_string()))
                    } else {
                        None
                    },
                    alpn_protocol: Some(String::from(if secure { "h2" } else { "h2c" })),
                    ..Http2SocketSnapshot::default()
                },
                ..Http2SessionSnapshot::default()
            }));
            let (session_id, capability_id, capability_generation) = admit_http2_session(
                &process.http2.shared,
                pending,
                command_tx,
                process.runtime_context.fairness().clone(),
                connection_reservations,
            )?;
            {
                let mut state = process.http2.shared.lock().map_err(|_| {
                    SidecarError::InvalidState(String::from("HTTP/2 state lock poisoned"))
                })?;
                if state.event_session.is_none() {
                    state.event_session = process
                        .execution
                        .execution_wake_handle(process.kernel_handle.runtime_identity());
                }
                state.session_events.entry(session_id).or_default();
            }
            let tls = if secure {
                Some(payload.tls.unwrap_or(TlsBridgeOptions {
                    is_server: false,
                    servername: Some(host.to_string()),
                    alpn_protocols: Some(vec![String::from("h2")]),
                    ..TlsBridgeOptions::default()
                }))
            } else {
                None
            };
            let default_ca_bundle = match tls.as_ref() {
                Some(options) => {
                    vm_default_ca_bundle_for_tls_options(kernel, process.kernel_pid, options)?
                }
                None => Vec::new(),
            };
            spawn_http2_client_session(
                process.runtime_context.clone(),
                Arc::clone(&process.http2.shared),
                session_id,
                resolved.actual_addr,
                tls,
                default_ca_bundle,
                Arc::clone(&snapshot),
                command_rx,
            );
            let snapshot_json =
                http2_snapshot_json(&snapshot.lock().expect("http2 snapshot lock").clone())?;
            encode_net_json_string(
                json!({
                    "sessionId": session_id,
                    "capabilityId": capability_id,
                    "capabilityGeneration": capability_generation,
                    "state": snapshot_json,
                }),
                "net.http2_session_connect",
            )
        }
        "net.http2_session_request" => {
            let session_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_session_request session id",
            )?;
            let headers_json =
                javascript_sync_rpc_arg_str(&request.args, 1, "net.http2_session_request headers")?;
            let options_json =
                javascript_sync_rpc_arg_str(&request.args, 2, "net.http2_session_request options")?;
            let session = http2_session_for_id(process, session_id)?;
            let stream_reservations = reserve_http2_stream(&session)?;
            let pending_capability =
                reserve_capability(&capabilities, CapabilityKind::Http2Stream)?;
            return send_http2_command(
                &session,
                headers_json
                    .len()
                    .saturating_add(options_json.len())
                    .saturating_add(64),
                headers_json.len(),
                0,
                |respond_to| {
                    Ok(Http2SessionCommand::Request {
                        headers_json: headers_json.to_owned(),
                        options_json: options_json.to_owned(),
                        pending_capability,
                        stream_reservations,
                        respond_to,
                    })
                },
            );
        }
        "net.http2_session_settings" => {
            let session_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_session_settings session id",
            )?;
            let settings_json = javascript_sync_rpc_arg_str(
                &request.args,
                1,
                "net.http2_session_settings settings",
            )?;
            let session = http2_session_for_id(process, session_id)?;
            return send_http2_command(
                &session,
                settings_json.len().saturating_add(32),
                0,
                0,
                |respond_to| {
                    Ok(Http2SessionCommand::Settings {
                        settings_json: settings_json.to_owned(),
                        respond_to,
                    })
                },
            );
        }
        "net.http2_session_set_local_window_size" => {
            let session_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_session_set_local_window_size session id",
            )?;
            let window_size = javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "net.http2_session_set_local_window_size window size",
            )?;
            let session = http2_session_for_id(process, session_id)?;
            return send_http2_command(&session, 32, 0, 0, |respond_to| {
                Ok(Http2SessionCommand::SetLocalWindowSize {
                    size: window_size as u32,
                    respond_to,
                })
            });
        }
        "net.http2_session_goaway" => {
            let session_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_session_goaway session id",
            )?;
            let error_code = javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "net.http2_session_goaway error code",
            )?;
            let last_stream_id = javascript_sync_rpc_arg_u64(
                &request.args,
                2,
                "net.http2_session_goaway last stream id",
            )?;
            let opaque_data = request
                .args
                .get(3)
                .and_then(Value::as_str)
                .map(|value| {
                    base64::engine::general_purpose::STANDARD
                        .decode(value)
                        .map_err(|error| {
                            SidecarError::InvalidState(format!("invalid GOAWAY payload: {error}"))
                        })
                })
                .transpose()?;
            let session = http2_session_for_id(process, session_id)?;
            let command_bytes = opaque_data
                .as_ref()
                .map_or(64, |data| data.len().saturating_add(64));
            return send_http2_command(&session, command_bytes, 0, 0, |respond_to| {
                Ok(Http2SessionCommand::Goaway {
                    error_code: error_code as u32,
                    last_stream_id: last_stream_id as u32,
                    opaque_data,
                    respond_to,
                })
            });
        }
        "net.http2_session_close" | "net.http2_session_destroy" => {
            let session_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_session_close session id",
            )?;
            let session = http2_session_for_id(process, session_id)?;
            session.close_abrupt.store(
                request.method == "net.http2_session_destroy",
                Ordering::Release,
            );
            session.close_requested.store(true, Ordering::Release);
            session.close_notify.notify_one();
            Ok(Value::Null)
        }
        "net.http2_session_poll" => {
            let session_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_session_poll session id")?;
            let wait_ms = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "net.http2_session_poll wait ms",
            )?
            .unwrap_or_default();
            return defer_http2_poll(process, session_id, false, wait_ms);
        }
        "net.http2_session_wait" => {
            let session_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_session_wait session id")?;
            return defer_http2_wait(process, session_id, false);
        }
        "net.http2_stream_respond" => {
            let stream_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_stream_respond stream id",
            )?;
            let headers_json =
                javascript_sync_rpc_arg_str(&request.args, 1, "net.http2_stream_respond headers")?;
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            return send_http2_command(
                &session,
                headers_json.len().saturating_add(32),
                headers_json.len(),
                0,
                |respond_to| {
                    Ok(Http2SessionCommand::StreamRespond {
                        stream_id,
                        headers_json: headers_json.to_owned(),
                        respond_to,
                    })
                },
            );
        }
        "net.http2_stream_push_stream" => {
            let stream_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_stream_push_stream stream id",
            )?;
            let headers_json = javascript_sync_rpc_arg_str(
                &request.args,
                1,
                "net.http2_stream_push_stream headers",
            )?;
            let _options_json = javascript_sync_rpc_arg_str(
                &request.args,
                2,
                "net.http2_stream_push_stream options",
            )?;
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            let stream_reservations = reserve_http2_stream(&session)?;
            let pending_capability =
                reserve_capability(&capabilities, CapabilityKind::Http2Stream)?;
            return send_http2_command(
                &session,
                headers_json.len().saturating_add(32),
                headers_json.len(),
                0,
                |respond_to| {
                    Ok(Http2SessionCommand::StreamPush {
                        stream_id,
                        headers_json: headers_json.to_owned(),
                        pending_capability,
                        stream_reservations,
                        respond_to,
                    })
                },
            );
        }
        "net.http2_stream_write" => {
            let stream_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_stream_write stream id")?;
            let chunk_base64 =
                javascript_sync_rpc_arg_str(&request.args, 1, "net.http2_stream_write data")?;
            let decoded_bytes = base64::decoded_len_estimate(chunk_base64.len());
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            let command_bytes = chunk_base64
                .len()
                .saturating_add(decoded_bytes)
                .saturating_add(32);
            return send_http2_command(&session, command_bytes, 0, decoded_bytes, |respond_to| {
                let chunk = base64::engine::general_purpose::STANDARD
                    .decode(chunk_base64)
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "invalid HTTP/2 stream payload: {error}"
                        ))
                    })?;
                Ok(Http2SessionCommand::StreamWrite {
                    stream_id,
                    chunk,
                    end_stream: false,
                    respond_to,
                })
            });
        }
        "net.http2_stream_end" => {
            let stream_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_stream_end stream id")?;
            let chunk_base64 = request
                .args
                .get(1)
                .and_then(Value::as_str)
                .unwrap_or_default();
            let decoded_bytes = base64::decoded_len_estimate(chunk_base64.len());
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            let command_bytes = chunk_base64
                .len()
                .saturating_add(decoded_bytes)
                .saturating_add(32);
            return send_http2_command(&session, command_bytes, 0, decoded_bytes, |respond_to| {
                let chunk = base64::engine::general_purpose::STANDARD
                    .decode(chunk_base64)
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "invalid HTTP/2 stream payload: {error}"
                        ))
                    })?;
                Ok(Http2SessionCommand::StreamWrite {
                    stream_id,
                    chunk,
                    end_stream: true,
                    respond_to,
                })
            });
        }
        "net.http2_stream_close" => {
            let stream_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_stream_close stream id")?;
            let code = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "net.http2_stream_close error code",
            )?
            .map(|value| value as u32);
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            return send_http2_command(&session, 32, 0, 0, |respond_to| {
                Ok(Http2SessionCommand::StreamClose {
                    stream_id,
                    error_code: code,
                    respond_to,
                })
            });
        }
        "net.http2_stream_pause" => {
            let stream_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_stream_pause stream id")?;
            let stream = http2_stream_for_id(process, stream_id)?;
            stream.paused.store(true, Ordering::SeqCst);
            Ok(Value::Null)
        }
        "net.http2_stream_resume" => {
            let stream_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http2_stream_resume stream id")?;
            let stream = http2_stream_for_id(process, stream_id)?;
            stream.paused.store(false, Ordering::SeqCst);
            stream.resume_notify.notify_waiters();
            Ok(Value::Null)
        }
        "net.http2_stream_respond_with_file" => {
            let stream_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "net.http2_stream_respond_with_file stream id",
            )?;
            let path = javascript_sync_rpc_arg_str(
                &request.args,
                1,
                "net.http2_stream_respond_with_file path",
            )?;
            let headers_json = javascript_sync_rpc_arg_str(
                &request.args,
                2,
                "net.http2_stream_respond_with_file headers",
            )?;
            let options_json = javascript_sync_rpc_arg_str(
                &request.args,
                3,
                "net.http2_stream_respond_with_file options",
            )?;
            serde_json::from_str::<Http2FileResponseOptions>(options_json).map_err(|error| {
                SidecarError::host(
                    "ERR_INVALID_ARG_VALUE",
                    format!("invalid HTTP/2 file response options: {error}"),
                )
            })?;
            let stream = http2_stream_for_id(process, stream_id)?;
            let session = http2_session_for_id(process, stream.session_id)?;
            let guest_path = resolve_http2_file_response_guest_path(process, path);
            let requester_pid = process.kernel_pid;
            let file_bytes = kernel
                .preflight_regular_file_read_for_process(
                    EXECUTION_DRIVER_NAME,
                    requester_pid,
                    &guest_path,
                )
                .map_err(kernel_error)?;
            let command_bytes = file_bytes
                .saturating_add(headers_json.len())
                .saturating_add(options_json.len())
                .saturating_add(64);
            return send_http2_command(
                &session,
                command_bytes,
                headers_json.len(),
                file_bytes,
                |respond_to| {
                    let body = read_http2_response_file_after_admission(
                        kernel,
                        requester_pid,
                        &guest_path,
                        file_bytes,
                    )?;
                    Ok(Http2SessionCommand::StreamRespondWithFile {
                        stream_id,
                        body,
                        headers_json: headers_json.to_owned(),
                        options_json: options_json.to_owned(),
                        respond_to,
                    })
                },
            );
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript HTTP/2 sync RPC method {other}"
        ))),
    };
    response.map(Into::into)
}

#[cfg(test)]
mod http2_reactor_tests {
    use super::*;
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::KernelVmConfig;
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::resource_accounting::ResourceLimits;
    use agentos_kernel::vfs::MemoryFileSystem;

    fn http2_ledger(event_limit: usize) -> Arc<ResourceLedger> {
        Arc::new(ResourceLedger::root(
            "vm=test",
            [
                (
                    ResourceClass::BufferedBytes,
                    ResourceLimit::new(16 * 1024, "runtime.resources.maxSocketBufferedBytes"),
                ),
                (
                    ResourceClass::Http2BufferedBytes,
                    ResourceLimit::new(16 * 1024, "limits.http2.maxBufferedBytes"),
                ),
                (
                    ResourceClass::Http2EventBytes,
                    ResourceLimit::new(16 * 1024, "limits.http2.maxPendingEventBytes"),
                ),
                (
                    ResourceClass::Http2Events,
                    ResourceLimit::new(event_limit, "limits.http2.maxPendingEvents"),
                ),
                (
                    ResourceClass::Http2DataBytes,
                    ResourceLimit::new(16 * 1024, "limits.http2.maxDataBytes"),
                ),
                (
                    ResourceClass::Http2HeaderBytes,
                    ResourceLimit::new(16 * 1024, "limits.http2.maxHeaderBytes"),
                ),
                (
                    ResourceClass::Http2Streams,
                    ResourceLimit::new(2, "limits.http2.maxStreams"),
                ),
                (
                    ResourceClass::Http2Commands,
                    ResourceLimit::new(4, "limits.http2.maxPendingCommands"),
                ),
                (
                    ResourceClass::Http2CommandBytes,
                    ResourceLimit::new(16 * 1024, "limits.http2.maxPendingCommandBytes"),
                ),
            ],
        ))
    }

    fn http2_capability_registry(maximum: usize) -> (Arc<ResourceLedger>, CapabilityRegistry) {
        let resources = Arc::new(ResourceLedger::root(
            "vm=http2-capabilities",
            [
                (
                    ResourceClass::Capabilities,
                    ResourceLimit::new(maximum, "limits.reactor.maxCapabilities"),
                ),
                (
                    ResourceClass::ReadyHandles,
                    ResourceLimit::new(maximum, "limits.reactor.maxReadyHandles"),
                ),
                (
                    ResourceClass::Sockets,
                    ResourceLimit::new(maximum, "limits.resources.maxSockets"),
                ),
                (
                    ResourceClass::Connections,
                    ResourceLimit::new(maximum, "limits.resources.maxConnections"),
                ),
            ],
        ));
        let registry = CapabilityRegistry::new(41, Arc::clone(&resources));
        (resources, registry)
    }

    fn http2_file_test_kernel(max_pread_bytes: usize) -> (SidecarKernel, u32) {
        let mut config = KernelVmConfig::new("vm-http2-file-response");
        config.permissions = Permissions::allow_all();
        config.resources = ResourceLimits {
            max_pread_bytes: Some(max_pread_bytes),
            ..ResourceLimits::default()
        };
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register HTTP/2 file test driver");
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn HTTP/2 file test process");
        (kernel, process.pid())
    }

    #[test]
    fn http2_multi_resource_admission_rolls_back_exactly() {
        let ledger = ResourceLedger::root(
            "vm=test",
            [
                (
                    ResourceClass::Http2Events,
                    ResourceLimit::new(1, "limits.http2.maxPendingEvents"),
                ),
                (
                    ResourceClass::Http2EventBytes,
                    ResourceLimit::new(4, "limits.http2.maxPendingEventBytes"),
                ),
            ],
        );
        let error = reserve_http2_resources(
            &ledger,
            &[
                (ResourceClass::Http2Events, 1),
                (ResourceClass::Http2EventBytes, 5),
            ],
        )
        .expect_err("second reservation must fail");
        assert!(error
            .to_string()
            .contains("limits.http2.maxPendingEventBytes"));
        assert!(
            ledger.is_zero(),
            "failed admission must roll back earlier resources"
        );
    }

    #[test]
    fn http2_data_event_limit_backpressures_and_releases_on_pop() {
        let ledger = http2_ledger(1);
        let state = crate::state::Http2SharedState {
            resources: Some(Arc::clone(&ledger)),
            ..crate::state::Http2SharedState::default()
        };
        let shared = Arc::new(Mutex::new(state));
        let chunk = Bytes::from_static(b"data");

        assert!(push_http2_data_event(
            &shared,
            7,
            false,
            "clientData",
            11,
            &chunk,
        ));
        assert!(!push_http2_data_event(
            &shared,
            7,
            false,
            "clientData",
            11,
            &chunk,
        ));
        assert_eq!(ledger.usage(ResourceClass::Http2Events).used, 1);

        let event = pop_http2_event_now(&shared, 7, false).expect("queued data event");
        assert_eq!(event.kind, "clientData");
        assert!(
            ledger.is_zero(),
            "popping the event must release every byte/count charge"
        );
    }

    #[test]
    fn http2_per_connection_stream_limit_is_independent_of_vm_limit() {
        let resources = http2_ledger(4);
        let stream_resources = Arc::new(ResourceLedger::root(
            "http2-session=1",
            [(
                ResourceClass::Http2Streams,
                ResourceLimit::new(1, "limits.http2.maxStreamsPerConnection"),
            )],
        ));
        let (command_tx, _command_rx) = tokio_channel(1);
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("runtime")
                .context();
        let session = ActiveHttp2Session {
            command_tx,
            capability_id: 1,
            vm_generation: 1,
            fairness: runtime.fairness().clone(),
            command_timeout: Duration::from_secs(1),
            close_requested: Arc::new(AtomicBool::new(false)),
            close_abrupt: Arc::new(AtomicBool::new(false)),
            close_notify: Arc::new(tokio::sync::Notify::new()),
            _reservations: Vec::new(),
            resources: Arc::clone(&resources),
            stream_resources: Arc::clone(&stream_resources),
        };
        let first = reserve_http2_stream(&session).expect("first stream");
        let error = reserve_http2_stream(&session).expect_err("per-connection limit");
        assert!(error
            .to_string()
            .contains("limits.http2.maxStreamsPerConnection"));
        assert_eq!(resources.usage(ResourceClass::Http2Streams).used, 1);
        drop(first);
        assert!(resources.is_zero());
        assert!(stream_resources.is_zero());
    }

    #[test]
    fn http2_command_returns_deferred_before_connection_task_responds() {
        let resources = http2_ledger(4);
        let stream_resources = Arc::new(ResourceLedger::root(
            "http2-session=1",
            [(
                ResourceClass::Http2Streams,
                ResourceLimit::new(1, "limits.http2.maxStreamsPerConnection"),
            )],
        ));
        let (command_tx, mut command_rx) = tokio_channel(1);
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("runtime")
                .context();
        let session = ActiveHttp2Session {
            command_tx,
            capability_id: 1,
            vm_generation: 1,
            fairness: runtime.fairness().clone(),
            command_timeout: Duration::from_secs(1),
            close_requested: Arc::new(AtomicBool::new(false)),
            close_abrupt: Arc::new(AtomicBool::new(false)),
            close_notify: Arc::new(tokio::sync::Notify::new()),
            _reservations: Vec::new(),
            resources: Arc::clone(&resources),
            stream_resources,
        };

        let response = send_http2_command(&session, 32, 0, 0, |respond_to| {
            Ok(Http2SessionCommand::Settings {
                settings_json: String::from("{}"),
                respond_to,
            })
        })
        .expect("command admission");
        let HostServiceResponse::Deferred { receiver, .. } = response else {
            panic!("HTTP/2 command must return a deferred response");
        };
        let queued = command_rx.try_recv().expect("queued command");
        let QueuedHttp2Command {
            command,
            reservations,
        } = queued;
        let Http2SessionCommand::Settings { respond_to, .. } = command else {
            panic!("expected settings command");
        };
        respond_to.settle(Ok(Value::Null));
        let result = runtime
            .handle()
            .block_on(receiver)
            .expect("response sender")
            .expect("command success");
        assert_eq!(result, Value::Null);
        drop(reservations);
        assert!(resources.is_zero());
    }

    #[test]
    fn http2_command_payload_failure_releases_every_reservation() {
        let resources = http2_ledger(4);
        let stream_resources = Arc::new(ResourceLedger::root(
            "http2-session=payload-failure",
            [(
                ResourceClass::Http2Streams,
                ResourceLimit::new(1, "limits.http2.maxStreamsPerConnection"),
            )],
        ));
        let (command_tx, mut command_rx) = tokio_channel(1);
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("runtime")
                .context();
        let session = ActiveHttp2Session {
            command_tx,
            capability_id: 1,
            vm_generation: 1,
            fairness: runtime.fairness().clone(),
            command_timeout: Duration::from_secs(1),
            close_requested: Arc::new(AtomicBool::new(false)),
            close_abrupt: Arc::new(AtomicBool::new(false)),
            close_notify: Arc::new(tokio::sync::Notify::new()),
            _reservations: Vec::new(),
            resources: Arc::clone(&resources),
            stream_resources,
        };

        let error = send_http2_command(&session, 32, 4, 8, |_respond_to| {
            Err(SidecarError::host(
                "ERR_AGENTOS_HTTP2_FILE_CHANGED",
                String::from("injected file race"),
            ))
        })
        .err()
        .expect("payload construction failure must propagate");
        assert_eq!(error.code(), Some("ERR_AGENTOS_HTTP2_FILE_CHANGED"));
        assert!(command_rx.try_recv().is_err());
        assert!(
            resources.is_zero(),
            "payload failure must release command/header/data reservations"
        );
    }

    #[test]
    fn http2_full_command_queue_releases_rejected_reservations() {
        let resources = http2_ledger(4);
        let stream_resources = Arc::new(ResourceLedger::root(
            "http2-session=queue-full",
            [(
                ResourceClass::Http2Streams,
                ResourceLimit::new(1, "limits.http2.maxStreamsPerConnection"),
            )],
        ));
        let (command_tx, mut command_rx) = tokio_channel(1);
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("runtime")
                .context();
        let session = ActiveHttp2Session {
            command_tx,
            capability_id: 1,
            vm_generation: 1,
            fairness: runtime.fairness().clone(),
            command_timeout: Duration::from_secs(1),
            close_requested: Arc::new(AtomicBool::new(false)),
            close_abrupt: Arc::new(AtomicBool::new(false)),
            close_notify: Arc::new(tokio::sync::Notify::new()),
            _reservations: Vec::new(),
            resources: Arc::clone(&resources),
            stream_resources,
        };

        let first_response = send_http2_command(&session, 32, 4, 8, |respond_to| {
            Ok(Http2SessionCommand::Settings {
                settings_json: String::from("{}"),
                respond_to,
            })
        })
        .expect("fill command queue");
        let charged_classes = [
            ResourceClass::Http2Commands,
            ResourceClass::Http2CommandBytes,
            ResourceClass::Http2HeaderBytes,
            ResourceClass::Http2DataBytes,
            ResourceClass::Http2BufferedBytes,
            ResourceClass::BufferedBytes,
        ];
        let usage_before = charged_classes.map(|class| resources.usage(class).used);

        let error = send_http2_command(&session, 32, 4, 8, |respond_to| {
            Ok(Http2SessionCommand::Settings {
                settings_json: String::from("{}"),
                respond_to,
            })
        })
        .err()
        .expect("full queue must reject the second command");
        assert_eq!(error.code(), Some("ERR_AGENTOS_HTTP2_COMMAND_LIMIT"));
        assert_eq!(
            charged_classes.map(|class| resources.usage(class).used),
            usage_before,
            "queue rejection must release only the rejected command reservations"
        );

        drop(command_rx.try_recv().expect("drain admitted command"));
        drop(first_response);
        assert!(resources.is_zero());
    }

    #[test]
    fn http2_file_read_rejects_growth_after_admission() {
        let (mut kernel, pid) = http2_file_test_kernel(16);
        kernel
            .write_file("/response", b"body".to_vec())
            .expect("write initial response");
        let admitted = kernel
            .preflight_regular_file_read_for_process(EXECUTION_DRIVER_NAME, pid, "/response")
            .expect("preflight response");
        kernel
            .write_file("/response", b"grown".to_vec())
            .expect("grow response after admission");

        let error =
            read_http2_response_file_after_admission(&mut kernel, pid, "/response", admitted)
                .expect_err("growth after admission must fail");
        assert_eq!(error.code(), Some("ERR_AGENTOS_HTTP2_FILE_CHANGED"));
    }

    #[test]
    fn http2_fair_turn_reports_actual_usage_before_next_capability_runs() {
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("runtime")
                .context();
        let first_generation = runtime
            .allocate_vm_generation()
            .expect("allocate first HTTP/2 fairness generation");
        let second_generation = runtime
            .allocate_vm_generation()
            .expect("allocate second HTTP/2 fairness generation");
        runtime.handle().block_on(async {
            let first = runtime
                .fairness()
                .acquire(first_generation, 1, FairBudget::new(4, 1_024))
                .await
                .expect("first H2 fair turn");
            first
                .complete(FairBudget::new(1, 64), false)
                .expect("report actual H2 usage");
            let second = tokio::time::timeout(
                Duration::from_secs(1),
                runtime
                    .fairness()
                    .acquire(second_generation, 2, FairBudget::new(4, 1_024)),
            )
            .await
            .expect("second VM must not be blocked by a retained H2 turn")
            .expect("second H2 fair turn");
            second
                .complete(FairBudget::new(0, 0), false)
                .expect("complete second H2 turn");
            runtime
                .fairness()
                .retire_capability(first_generation, 1)
                .expect("retire first capability");
            runtime
                .fairness()
                .retire_capability(second_generation, 2)
                .expect("retire second capability");
        });
    }

    #[test]
    fn duplicate_http2_capability_preserves_the_live_lease() {
        let (resources, capabilities) = http2_capability_registry(2);
        let shared = Arc::new(Mutex::new(crate::state::Http2SharedState::default()));
        let key = NativeCapabilityKey::Http2Server(7);
        let first_identity = commit_http2_capability(
            &shared,
            capabilities
                .reserve(CapabilityKind::TcpListener)
                .expect("reserve first HTTP/2 capability"),
            key.clone(),
            String::from("http2-first"),
        )
        .expect("commit first HTTP/2 capability");
        let error = commit_http2_capability(
            &shared,
            capabilities
                .reserve(CapabilityKind::TcpListener)
                .expect("reserve duplicate HTTP/2 capability"),
            key.clone(),
            String::from("http2-duplicate"),
        )
        .expect_err("duplicate HTTP/2 key must be rejected");

        assert!(error
            .to_string()
            .contains("ERR_AGENTOS_CAPABILITY_DUPLICATE"));
        assert_eq!(
            shared
                .lock()
                .expect("HTTP/2 state")
                .capability_leases
                .get(&key)
                .expect("original HTTP/2 lease remains")
                .id(),
            first_identity.0
        );
        assert_eq!(capabilities.outstanding_len(), 1);

        shared
            .lock()
            .expect("HTTP/2 state")
            .capability_leases
            .clear();
        assert!(resources.is_zero());
    }

    #[test]
    fn repeated_process_teardown_retires_http2_fairness_within_one_vm() {
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("runtime")
                .context();
        let vm_generation = runtime
            .allocate_vm_generation()
            .expect("allocate repeated teardown HTTP/2 generation");

        for capability_id in 1..=2 {
            runtime.handle().block_on(async {
                let turn = runtime
                    .fairness()
                    .acquire(vm_generation, capability_id, FairBudget::new(1, 128))
                    .await
                    .expect("enroll process HTTP/2 capability");
                turn.complete(FairBudget::new(1, 64), false)
                    .expect("complete process HTTP/2 turn");
            });

            let resources = http2_ledger(4);
            let stream_resources = Arc::new(ResourceLedger::root(
                format!("http2-session={capability_id}"),
                [(
                    ResourceClass::Http2Streams,
                    ResourceLimit::new(1, "limits.http2.maxStreamsPerConnection"),
                )],
            ));
            let (command_tx, _command_rx) = tokio_channel(1);
            let shared = Arc::new(Mutex::new(crate::state::Http2SharedState::default()));
            shared.lock().expect("HTTP/2 state").sessions.insert(
                1,
                ActiveHttp2Session {
                    command_tx,
                    capability_id,
                    vm_generation,
                    fairness: runtime.fairness().clone(),
                    command_timeout: Duration::from_secs(1),
                    close_requested: Arc::new(AtomicBool::new(false)),
                    close_abrupt: Arc::new(AtomicBool::new(false)),
                    close_notify: Arc::new(tokio::sync::Notify::new()),
                    _reservations: Vec::new(),
                    resources,
                    stream_resources,
                },
            );

            terminate_http2_process_state(&shared);
            assert!(shared.lock().expect("HTTP/2 state").sessions.is_empty());
            runtime.handle().block_on(async {
                assert!(matches!(
                    runtime
                        .fairness()
                        .acquire(vm_generation, capability_id, FairBudget::new(1, 128))
                        .await,
                    Err(agentos_runtime::fairness::FairnessError::CapabilityRetired {
                        vm_generation: retired_generation,
                        capability_id: retired,
                    }) if retired_generation == vm_generation && retired == capability_id
                ));
            });
        }
    }
}
