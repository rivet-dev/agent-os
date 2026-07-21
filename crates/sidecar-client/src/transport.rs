//! `SidecarTransport`: spawns a native sidecar binary and speaks the existing framed
//! BARE protocol over its stdio.
//!
//! This mirrors the TypeScript `Sidecar`. Generated wire payloads are the native
//! transport path.
//!
//! Request-id direction is load-bearing: host-initiated `Request`/`Response` frames use positive ids
//! allocated by this transport, while sidecar-initiated `SidecarRequest`/`SidecarResponse` callbacks
//! echo the id allocated by the sidecar.

use std::process::Stdio;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

#[cfg(unix)]
use command_fds::{CommandFdExt, FdMapping};
#[cfg(unix)]
use std::os::unix::net::UnixStream as StdUnixStream;

use scc::HashMap as SccHashMap;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::wire::{self, WireFrameCodec};
use crate::TransportError;

/// Broadcast capacity for the structured/lifecycle/process event fan-out.
const EVENT_CHANNEL_CAPACITY: usize = 4096;

/// Maximum outbound frames buffered while the writer task drains to sidecar stdin.
const REQUEST_FRAME_QUEUE_CAPACITY: usize = 4096;

/// Maximum callback/control response frames buffered ahead of regular host requests.
const CONTROL_FRAME_QUEUE_CAPACITY: usize = 1024;

/// Maximum in-flight host-initiated sidecar requests per transport.
const PENDING_REQUEST_LIMIT: usize = 4096;

/// Env var that overrides the sidecar binary path. Defaults to `agentos-native-sidecar` on `PATH`.
/// Product clients can pass an explicit binary path to [`SidecarTransport::spawn`].
const SIDECAR_BIN_ENV: &str = "AGENTOS_SIDECAR_BIN";

/// Fixed inherited descriptor carrying the full-duplex response/control lane.
#[cfg(unix)]
const CONTROL_FD: std::os::fd::RawFd = 3;

/// How long the host tolerates TOTAL inbound silence (no responses, events, sidecar requests, or
/// heartbeats) before declaring the sidecar dead. The sidecar heartbeats every 10s from a dedicated
/// thread even while busy, so this allows two missed beats plus margin; it bounds "sidecar is dead
/// or wedged", never "this request is slow" — individual requests have no deadline of their own.
/// Fixed protocol constant paired with the sidecar heartbeat cadence; mirrors the TS client.
const SIDECAR_SILENCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A registered callback that answers a sidecar-initiated request using generated wire types.
pub type WireSidecarCallback = Arc<
    dyn Fn(
            wire::SidecarRequestPayload,
            wire::OwnershipScope,
        ) -> futures::future::BoxFuture<
            'static,
            Result<wire::SidecarResponsePayload, TransportError>,
        > + Send
        + Sync,
>;

/// Owns the spawned sidecar child, the framed BARE stdio I/O tasks, the pending-response map, the
/// event fan-out, and the callback dispatch table.
pub struct SidecarTransport {
    /// The spawned sidecar process (stdout/stdin taken by the I/O tasks; kept for kill on drop).
    child: parking_lot::Mutex<Option<Child>>,
    /// Pending host-initiated requests, keyed by positive `RequestId`.
    pending: SccHashMap<wire::RequestId, oneshot::Sender<wire::ResponsePayload>>,
    pending_request_lock: parking_lot::Mutex<()>,
    /// Host request-id counter (positive, starts at 1).
    request_counter: AtomicI64,
    /// Negotiated max frame size.
    max_frame_bytes: AtomicUsize,
    /// Structured-event fan-out for `Event` frames.
    event_tx: broadcast::Sender<(wire::OwnershipScope, wire::EventPayload)>,
    /// Registered host callbacks for `SidecarRequest` frames.
    callbacks: SccHashMap<&'static str, WireSidecarCallback>,
    /// Outbound host request frames drained by the writer task into the child's stdin.
    request_writer_tx: mpsc::Sender<Vec<u8>>,
    /// Outbound callback/control response frames. The writer drains this before regular requests.
    control_writer_tx: mpsc::Sender<Vec<u8>>,
    /// When the reader last received any inbound frame; the silence watchdog reads it.
    last_inbound_at: parking_lot::Mutex<std::time::Instant>,
}

impl SidecarTransport {
    /// Spawn the native sidecar binary and start the stdio I/O tasks.
    ///
    /// Does NOT run the handshake. Product clients drive Authenticate and any follow-up setup using
    /// [`request_wire`](Self::request_wire) once the transport is live.
    pub async fn spawn(binary_path: Option<String>) -> Result<Arc<Self>, TransportError> {
        #[cfg(not(unix))]
        {
            let _ = binary_path;
            return Err(TransportError::Sidecar(
                "the native sidecar response/control transport is unsupported on this platform"
                    .to_string(),
            ));
        }

        #[cfg(unix)]
        {
            Self::spawn_unix(binary_path).await
        }
    }

    #[cfg(unix)]
    async fn spawn_unix(binary_path: Option<String>) -> Result<Arc<Self>, TransportError> {
        let bin = resolve_sidecar_binary_path(binary_path);
        let (control_parent, control_child) = StdUnixStream::pair().map_err(|error| {
            TransportError::Sidecar(format!(
                "failed to create sidecar control socketpair: {error}"
            ))
        })?;
        control_parent.set_nonblocking(true).map_err(|error| {
            TransportError::Sidecar(format!(
                "failed to configure sidecar control socket: {error}"
            ))
        })?;
        let mut command = Command::new(&bin);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        // `command-fds` performs the child-only dup after fork, avoiding the
        // CLOEXEC race caused by making an arbitrary descriptor inheritable in
        // this multithreaded process.
        map_control_fd(&mut command, control_child)?;
        let mut child = command.spawn().map_err(|error| {
            TransportError::Sidecar(format!("failed to spawn sidecar '{bin}': {error}"))
        })?;
        drop(command);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| TransportError::Sidecar("sidecar stdin was not piped".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TransportError::Sidecar("sidecar stdout was not piped".to_string()))?;
        let control = tokio::net::UnixStream::from_std(control_parent).map_err(|error| {
            TransportError::Sidecar(format!("failed to adopt sidecar control socket: {error}"))
        })?;
        let (control_reader, control_writer) = control.into_split();

        let (request_writer_tx, request_writer_rx) = mpsc::channel(REQUEST_FRAME_QUEUE_CAPACITY);
        let (control_writer_tx, control_writer_rx) = mpsc::channel(CONTROL_FRAME_QUEUE_CAPACITY);
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        let transport = Arc::new(Self {
            child: parking_lot::Mutex::new(Some(child)),
            pending: SccHashMap::new(),
            pending_request_lock: parking_lot::Mutex::new(()),
            request_counter: AtomicI64::new(1),
            max_frame_bytes: AtomicUsize::new(wire::DEFAULT_MAX_FRAME_BYTES),
            event_tx,
            callbacks: SccHashMap::new(),
            request_writer_tx,
            control_writer_tx,
            last_inbound_at: parking_lot::Mutex::new(std::time::Instant::now()),
        });

        tokio::spawn(run_writer(
            Arc::downgrade(&transport),
            "ordinary request",
            stdin,
            request_writer_rx,
        ));
        tokio::spawn(run_writer(
            Arc::downgrade(&transport),
            "response/control",
            control_writer,
            control_writer_rx,
        ));
        tokio::spawn(run_reader(
            Arc::downgrade(&transport),
            stdout,
            InboundLane::Event,
        ));
        tokio::spawn(run_reader(
            Arc::downgrade(&transport),
            control_reader,
            InboundLane::Control,
        ));
        tokio::spawn(run_silence_watchdog(
            Arc::downgrade(&transport),
            SIDECAR_SILENCE_TIMEOUT,
        ));

        Ok(transport)
    }

    /// Allocate the next positive host request id.
    pub fn next_request_id(&self) -> wire::RequestId {
        self.request_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Issue a host request using generated wire protocol types and await a generated response.
    pub async fn request_wire(
        &self,
        ownership: wire::OwnershipScope,
        payload: wire::RequestPayload,
    ) -> Result<wire::ResponsePayload, TransportError> {
        self.request_wire_with_frame_limit(ownership, payload, None)
            .await
    }

    /// Issue a host request using generated wire protocol types with a caller-specific frame limit.
    pub async fn request_wire_bounded(
        &self,
        ownership: wire::OwnershipScope,
        payload: wire::RequestPayload,
        max_frame_bytes: usize,
    ) -> Result<wire::ResponsePayload, TransportError> {
        self.request_wire_with_frame_limit(ownership, payload, Some(max_frame_bytes))
            .await
    }

    async fn request_wire_with_frame_limit(
        &self,
        ownership: wire::OwnershipScope,
        payload: wire::RequestPayload,
        max_frame_bytes: Option<usize>,
    ) -> Result<wire::ResponsePayload, TransportError> {
        let request_id = self.next_request_id();
        let frame = wire::ProtocolFrame::RequestFrame(wire::RequestFrame {
            schema: wire::protocol_schema(),
            request_id,
            ownership,
            payload,
        });
        let bytes = self.encode_wire_frame(&frame, max_frame_bytes)?;

        let (tx, rx) = oneshot::channel();
        self.register_pending_request(request_id, tx)?;
        let _pending_guard = PendingRequestGuard::new(self, request_id);

        if self.request_writer_tx.send(bytes).await.is_err() {
            self.pending.remove(&request_id);
            return Err(TransportError::Sidecar(
                "sidecar transport closed".to_string(),
            ));
        }

        rx.await
            .map_err(|_| TransportError::Sidecar("sidecar transport disconnected".to_string()))
    }

    /// Subscribe to structured/lifecycle/process events using generated wire protocol types.
    pub fn subscribe_wire_events(
        &self,
    ) -> broadcast::Receiver<(wire::OwnershipScope, wire::EventPayload)> {
        self.event_tx.subscribe()
    }

    /// Register a callback that answers a class of sidecar-initiated requests using generated wire
    /// protocol types.
    pub fn register_wire_callback(&self, key: &'static str, callback: WireSidecarCallback) {
        let _ = self.callbacks.insert(key, callback);
    }

    /// Return the currently negotiated max frame size.
    pub fn max_frame_bytes(&self) -> usize {
        self.max_frame_bytes.load(Ordering::Relaxed)
    }

    /// Update the negotiated max frame size after authentication.
    pub fn set_max_frame_bytes(&self, max_frame_bytes: usize) {
        self.max_frame_bytes
            .store(max_frame_bytes, Ordering::SeqCst);
    }

    /// Request graceful process termination through the physically independent
    /// response/control lane. This does not reinterpret an ordinary request as
    /// control traffic.
    pub async fn shutdown(&self, reason: impl Into<String>) -> Result<(), TransportError> {
        let frame = wire::ProtocolFrame::ControlFrame(wire::ControlFrame {
            schema: wire::protocol_schema(),
            payload: wire::ControlPayload::ShutdownControl(wire::ShutdownControl {
                reason: reason.into(),
            }),
        });
        let bytes = self.encode_wire_frame(&frame, None)?;
        self.control_writer_tx.send(bytes).await.map_err(|_| {
            TransportError::Sidecar("sidecar response/control transport closed".to_string())
        })
    }

    /// Kill the child sidecar process if this transport still owns one.
    pub fn kill_child(&self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.start_kill();
        }
    }

    fn encode_wire_frame(
        &self,
        frame: &wire::ProtocolFrame,
        max_frame_bytes: Option<usize>,
    ) -> Result<Vec<u8>, TransportError> {
        let transport_limit = self.max_frame_bytes.load(Ordering::Relaxed);
        let max_frame_bytes = max_frame_bytes
            .map(|limit| limit.min(transport_limit))
            .unwrap_or(transport_limit);
        let codec = WireFrameCodec::new(max_frame_bytes);
        Ok(codec.encode(frame)?)
    }

    /// Route a decoded inbound frame. Host transports only legitimately receive `Response`, `Event`,
    /// and `SidecarRequest` frames.
    async fn handle_wire_frame(self: &Arc<Self>, frame: wire::ProtocolFrame) {
        match frame {
            wire::ProtocolFrame::ResponseFrame(response) => {
                match self.pending.remove(&response.request_id) {
                    Some((_, tx)) => {
                        let _ = tx.send(response.payload);
                    }
                    None => {
                        tracing::warn!(
                            request_id = response.request_id,
                            "response for unknown request id"
                        )
                    }
                }
            }
            wire::ProtocolFrame::EventFrame(event) => {
                // Transport-level liveness beats from the sidecar. Their arrival
                // already reset the silence watchdog in the reader; they carry no
                // meaning for event subscribers, so drop them here (mirrors the
                // TS client's heartbeat swallow).
                if matches!(
                    &event.payload,
                    wire::EventPayload::StructuredEvent(structured)
                        if structured.name == "heartbeat"
                ) {
                    return;
                }
                let _ = self.event_tx.send((event.ownership, event.payload));
            }
            wire::ProtocolFrame::SidecarRequestFrame(request) => {
                self.dispatch_sidecar_request(request).await
            }
            wire::ProtocolFrame::SidecarResponseFrame(_)
            | wire::ProtocolFrame::RequestFrame(_)
            | wire::ProtocolFrame::ControlFrame(_) => {
                tracing::warn!("unexpected inbound frame on host transport")
            }
        }
    }

    /// Dispatch a sidecar-initiated request to its registered callback. The callback runs in a
    /// spawned task so long-running host callbacks (binding execution, permission prompts) cannot stall
    /// the reader loop, which must keep draining responses for any requests the callback itself
    /// issues through this transport.
    async fn dispatch_sidecar_request(self: &Arc<Self>, frame: wire::SidecarRequestFrame) {
        let key = sidecar_request_key(&frame.payload);
        let callback = self.callbacks.read(&key, |_, value| value.clone());
        match callback {
            Some(callback) => {
                let transport = Arc::downgrade(self);
                tokio::spawn(async move {
                    match callback(frame.payload, frame.ownership.clone()).await {
                        Ok(payload) => {
                            let response = wire::ProtocolFrame::SidecarResponseFrame(
                                wire::SidecarResponseFrame {
                                    schema: wire::protocol_schema(),
                                    request_id: frame.request_id,
                                    ownership: frame.ownership,
                                    payload,
                                },
                            );
                            // If the transport is gone, the child is being killed; drop the reply.
                            let Some(transport) = transport.upgrade() else {
                                return;
                            };
                            if let Ok(bytes) = transport.encode_wire_frame(&response, None) {
                                let _ = transport.control_writer_tx.send(bytes).await;
                            }
                        }
                        Err(error) => tracing::warn!(?error, key, "sidecar callback failed"),
                    }
                });
            }
            None => tracing::warn!(key, "no callback registered for sidecar request"),
        }
    }

    /// Reject every in-flight request after the transport disconnects.
    fn fail_all_pending(&self) {
        self.pending.clear();
    }

    fn disconnect(&self) {
        self.kill_child();
        self.fail_all_pending();
    }

    fn register_pending_request(
        &self,
        request_id: wire::RequestId,
        tx: oneshot::Sender<wire::ResponsePayload>,
    ) -> Result<(), TransportError> {
        let _guard = self.pending_request_lock.lock();
        if pending_request_count(self) >= PENDING_REQUEST_LIMIT {
            return Err(TransportError::Sidecar(format!(
                "sidecar pending request limit exceeded: at most {PENDING_REQUEST_LIMIT} requests can be in flight"
            )));
        }
        let _ = self.pending.insert(request_id, tx);
        Ok(())
    }
}

#[cfg(unix)]
fn map_control_fd(
    command: &mut Command,
    control_child: StdUnixStream,
) -> Result<(), TransportError> {
    command
        .fd_mappings(vec![FdMapping {
            parent_fd: control_child.into(),
            child_fd: CONTROL_FD,
        }])
        .map_err(|error| {
            TransportError::Sidecar(format!(
                "failed to map sidecar response/control fd: {error}"
            ))
        })?;
    Ok(())
}

struct PendingRequestGuard<'a> {
    transport: &'a SidecarTransport,
    request_id: wire::RequestId,
}

impl<'a> PendingRequestGuard<'a> {
    fn new(transport: &'a SidecarTransport, request_id: wire::RequestId) -> Self {
        Self {
            transport,
            request_id,
        }
    }
}

impl Drop for PendingRequestGuard<'_> {
    fn drop(&mut self) {
        let _ = self.transport.pending.remove(&self.request_id);
    }
}

fn pending_request_count(transport: &SidecarTransport) -> usize {
    let mut count = 0;
    transport.pending.scan(|_, _| {
        count += 1;
    });
    count
}

/// Map a sidecar-request payload to the callback registry key.
fn sidecar_request_key(payload: &wire::SidecarRequestPayload) -> &'static str {
    match payload {
        wire::SidecarRequestPayload::HostCallbackRequest(_) => "host_callback",
        wire::SidecarRequestPayload::JsBridgeCallRequest(_) => "js_bridge_call",
        wire::SidecarRequestPayload::ExtEnvelope(_) => "ext",
    }
}

/// Drain one bounded outbound lane into its physically independent stream.
async fn run_writer<W>(
    transport: Weak<SidecarTransport>,
    lane: &'static str,
    mut writer: W,
    mut frames: mpsc::Receiver<Vec<u8>>,
) where
    W: AsyncWrite + Unpin,
{
    while let Some(bytes) = frames.recv().await {
        let result = async {
            writer.write_all(&bytes).await?;
            writer.flush().await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(?error, lane, "sidecar writer failed");
            if let Some(transport) = transport.upgrade() {
                transport.disconnect();
            }
            return;
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum InboundLane {
    Event,
    Control,
}

impl InboundLane {
    fn accepts(self, frame: &wire::ProtocolFrame) -> bool {
        match self {
            Self::Event => {
                matches!(frame, wire::ProtocolFrame::EventFrame(event) if !is_heartbeat(event))
            }
            Self::Control => match frame {
                wire::ProtocolFrame::ResponseFrame(_)
                | wire::ProtocolFrame::SidecarRequestFrame(_) => true,
                wire::ProtocolFrame::EventFrame(event) => is_heartbeat(event),
                wire::ProtocolFrame::RequestFrame(_)
                | wire::ProtocolFrame::SidecarResponseFrame(_)
                | wire::ProtocolFrame::ControlFrame(_) => false,
            },
        }
    }
}

fn is_heartbeat(event: &wire::EventFrame) -> bool {
    matches!(
        &event.payload,
        wire::EventPayload::StructuredEvent(structured) if structured.name == "heartbeat"
    )
}

/// Read length-prefixed BARE frames from one physical inbound lane and route them.
async fn run_reader<R>(transport: Weak<SidecarTransport>, mut reader: R, lane: InboundLane)
where
    R: AsyncRead + Unpin,
{
    loop {
        let mut length_buf = [0u8; 4];
        if let Err(error) = reader.read_exact(&mut length_buf).await {
            if let Some(transport) = transport.upgrade() {
                tracing::warn!(?error, ?lane, "sidecar reader ended");
                transport.disconnect();
            }
            return;
        }
        let length = u32::from_be_bytes(length_buf) as usize;

        let Some(transport) = transport.upgrade() else {
            break;
        };
        let max_frame_bytes = transport.max_frame_bytes.load(Ordering::Relaxed);
        if frame_length_exceeds_limit(length, max_frame_bytes) {
            tracing::warn!(
                size = length,
                max = max_frame_bytes,
                "sidecar frame exceeds negotiated limit"
            );
            transport.disconnect();
            return;
        }

        let mut frame_bytes = vec![0u8; 4 + length];
        frame_bytes[..4].copy_from_slice(&length_buf);
        if let Err(error) = reader.read_exact(&mut frame_bytes[4..]).await {
            tracing::warn!(?error, ?lane, "sidecar reader ended mid-frame");
            transport.disconnect();
            return;
        }
        // Any complete inbound frame proves the sidecar is alive; the silence
        // watchdog measures from here.
        *transport.last_inbound_at.lock() = std::time::Instant::now();

        let codec = WireFrameCodec::new(max_frame_bytes);
        match codec.decode(&frame_bytes) {
            Ok(frame) if lane.accepts(&frame) => transport.handle_wire_frame(frame).await,
            Ok(frame) => {
                tracing::warn!(?lane, frame = ?frame, "sidecar frame arrived on wrong transport lane");
                transport.disconnect();
                return;
            }
            Err(error) => {
                tracing::warn!(?error, ?lane, "failed to decode sidecar frame");
                transport.disconnect();
                return;
            }
        }
    }
}

fn frame_length_exceeds_limit(length: usize, max_frame_bytes: usize) -> bool {
    length > max_frame_bytes
}

/// Kill the sidecar and fail all in-flight requests once the transport has seen no inbound frames
/// (not even heartbeats) for `timeout`. A silent sidecar is dead or wedged, not busy: a busy
/// sidecar still heartbeats every 10s from a dedicated thread. Exits when the transport drops.
async fn run_silence_watchdog(transport: Weak<SidecarTransport>, timeout: std::time::Duration) {
    let check_interval = (timeout / 4).min(std::time::Duration::from_secs(1));
    loop {
        tokio::time::sleep(check_interval).await;
        let Some(transport) = transport.upgrade() else {
            return;
        };
        let silence = transport.last_inbound_at.lock().elapsed();
        if silence < timeout {
            continue;
        }
        tracing::error!(
            silence_ms = silence.as_millis() as u64,
            "sidecar unresponsive: no protocol frames or heartbeats; killing sidecar",
        );
        transport.kill_child();
        transport.fail_all_pending();
        return;
    }
}

fn resolve_sidecar_binary_path(binary_path: Option<String>) -> String {
    binary_path
        .or_else(|| std::env::var(SIDECAR_BIN_ENV).ok())
        .unwrap_or_else(|| "agentos-native-sidecar".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn test_transport() -> SidecarTransport {
        let (request_writer_tx, _request_writer_rx) = mpsc::channel(REQUEST_FRAME_QUEUE_CAPACITY);
        let (control_writer_tx, _control_writer_rx) = mpsc::channel(CONTROL_FRAME_QUEUE_CAPACITY);
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        SidecarTransport {
            child: parking_lot::Mutex::new(None),
            pending: SccHashMap::new(),
            pending_request_lock: parking_lot::Mutex::new(()),
            request_counter: AtomicI64::new(1),
            max_frame_bytes: AtomicUsize::new(wire::DEFAULT_MAX_FRAME_BYTES),
            event_tx,
            callbacks: SccHashMap::new(),
            request_writer_tx,
            control_writer_tx,
            last_inbound_at: parking_lot::Mutex::new(std::time::Instant::now()),
        }
    }

    #[test]
    fn binary_path_prefers_explicit_path_over_env() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let previous = std::env::var(SIDECAR_BIN_ENV).ok();
        std::env::set_var(SIDECAR_BIN_ENV, "/tmp/from-env");

        assert_eq!(
            resolve_sidecar_binary_path(Some("/tmp/from-config".to_string())),
            "/tmp/from-config"
        );

        restore_env(SIDECAR_BIN_ENV, previous);
    }

    #[test]
    fn binary_path_uses_agentos_env_fallback() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let previous = std::env::var(SIDECAR_BIN_ENV).ok();
        std::env::set_var(SIDECAR_BIN_ENV, "/tmp/agentos-native-sidecar");

        assert_eq!(
            resolve_sidecar_binary_path(None),
            "/tmp/agentos-native-sidecar"
        );

        restore_env(SIDECAR_BIN_ENV, previous);
    }

    #[test]
    fn binary_path_defaults_to_agentos_native_sidecar() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let previous = std::env::var(SIDECAR_BIN_ENV).ok();
        std::env::remove_var(SIDECAR_BIN_ENV);

        assert_eq!(resolve_sidecar_binary_path(None), "agentos-native-sidecar");

        restore_env(SIDECAR_BIN_ENV, previous);
    }

    fn restore_env(key: &str, value: Option<String>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn frame_length_limit_rejects_oversized_declared_length() {
        assert!(!frame_length_exceeds_limit(1024, 1024));
        assert!(frame_length_exceeds_limit(1025, 1024));
    }

    #[test]
    fn transport_encodes_requests_with_generated_wire_codec() {
        let transport = test_transport();
        let frame = wire::ProtocolFrame::RequestFrame(wire::RequestFrame {
            schema: wire::protocol_schema(),
            request_id: 7,
            ownership: wire::OwnershipScope::ConnectionOwnership(wire::ConnectionOwnership {
                connection_id: "conn-1".to_string(),
            }),
            payload: wire::RequestPayload::AuthenticateRequest(wire::AuthenticateRequest {
                client_name: "transport-test".to_string(),
                auth_token: "token".to_string(),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: 1,
            }),
        });

        let encoded = transport
            .encode_wire_frame(&frame, None)
            .expect("encode transport frame");
        let decoded = WireFrameCodec::default()
            .decode(&encoded)
            .expect("decode generated wire frame");

        assert!(matches!(
            decoded,
            wire::ProtocolFrame::RequestFrame(wire::RequestFrame {
                payload: wire::RequestPayload::AuthenticateRequest(_),
                ..
            })
        ));
    }

    #[tokio::test]
    async fn transport_fans_out_generated_wire_events() {
        let transport = Arc::new(test_transport());
        let mut wire_events = transport.subscribe_wire_events();

        transport
            .handle_wire_frame(wire::ProtocolFrame::EventFrame(wire::EventFrame {
                schema: wire::protocol_schema(),
                ownership: wire::OwnershipScope::VmOwnership(wire::VmOwnership {
                    connection_id: "conn-1".to_string(),
                    session_id: "session-1".to_string(),
                    vm_id: "vm-1".to_string(),
                }),
                payload: wire::EventPayload::ProcessOutputEvent(wire::ProcessOutputEvent {
                    process_id: "proc-1".to_string(),
                    channel: wire::StreamChannel::Stdout,
                    chunk: b"hello".to_vec(),
                }),
            }))
            .await;

        let (ownership, payload) = wire_events.recv().await.expect("wire event");
        assert!(matches!(
            ownership,
            wire::OwnershipScope::VmOwnership(wire::VmOwnership {
                connection_id,
                session_id,
                vm_id,
            }) if connection_id == "conn-1" && session_id == "session-1" && vm_id == "vm-1"
        ));
        assert!(matches!(
            payload,
            wire::EventPayload::ProcessOutputEvent(wire::ProcessOutputEvent {
                process_id,
                channel: wire::StreamChannel::Stdout,
                chunk,
            }) if process_id == "proc-1" && chunk == b"hello".to_vec()
        ));
    }

    #[tokio::test]
    async fn event_before_response_is_delivered_without_transport_history() {
        let transport = Arc::new(test_transport());
        let mut events = transport.subscribe_wire_events();
        let (response_tx, response_rx) = oneshot::channel();
        transport
            .register_pending_request(7, response_tx)
            .expect("register pending request");
        let ownership = wire::OwnershipScope::ConnectionOwnership(wire::ConnectionOwnership {
            connection_id: "conn-1".to_string(),
        });

        transport
            .handle_wire_frame(wire::ProtocolFrame::EventFrame(wire::EventFrame {
                schema: wire::protocol_schema(),
                ownership: ownership.clone(),
                payload: wire::EventPayload::StructuredEvent(wire::StructuredEvent {
                    name: "process_started_before_reply".to_string(),
                    detail: std::collections::HashMap::new(),
                }),
            }))
            .await;
        transport
            .handle_wire_frame(wire::ProtocolFrame::ResponseFrame(wire::ResponseFrame {
                schema: wire::protocol_schema(),
                request_id: 7,
                ownership,
                payload: wire::ResponsePayload::ExtEnvelope(wire::ExtEnvelope {
                    namespace: "test".to_string(),
                    payload: Vec::new(),
                }),
            }))
            .await;

        let (_, event) = events.recv().await.expect("live event");
        assert!(matches!(
            event,
            wire::EventPayload::StructuredEvent(wire::StructuredEvent { name, .. })
                if name == "process_started_before_reply"
        ));
        assert!(matches!(
            response_rx.await.expect("response"),
            wire::ResponsePayload::ExtEnvelope(_)
        ));
    }

    #[tokio::test]
    async fn bounded_event_fanout_reports_lag_instead_of_retaining_history() {
        let (event_tx, mut events) = broadcast::channel(2);
        for index in 0..3 {
            event_tx
                .send((
                    wire::OwnershipScope::ConnectionOwnership(wire::ConnectionOwnership {
                        connection_id: "conn-1".to_string(),
                    }),
                    wire::EventPayload::StructuredEvent(wire::StructuredEvent {
                        name: format!("event-{index}"),
                        detail: std::collections::HashMap::new(),
                    }),
                ))
                .expect("active receiver");
        }

        assert!(matches!(
            events.recv().await,
            Err(broadcast::error::RecvError::Lagged(1))
        ));
    }

    #[tokio::test]
    async fn silence_watchdog_fails_pending_requests_after_sustained_silence() {
        let transport = Arc::new(test_transport());
        let (tx, rx) = oneshot::channel();
        transport
            .register_pending_request(1, tx)
            .expect("register pending request");

        tokio::spawn(run_silence_watchdog(
            Arc::downgrade(&transport),
            std::time::Duration::from_millis(40),
        ));

        // No inbound activity at all: the watchdog must reject the pending
        // request (dropped sender -> disconnected error at the caller).
        rx.await
            .expect_err("watchdog should drop the pending sender");
        assert_eq!(pending_request_count(&transport), 0);
    }

    #[tokio::test]
    async fn silence_watchdog_stays_quiet_while_frames_arrive() {
        let transport = Arc::new(test_transport());
        let (tx, mut rx) = oneshot::channel();
        transport
            .register_pending_request(1, tx)
            .expect("register pending request");

        tokio::spawn(run_silence_watchdog(
            Arc::downgrade(&transport),
            std::time::Duration::from_millis(120),
        ));

        // Simulate steady inbound activity (what the reader does per frame)
        // for well past the silence window; the watchdog must not fire.
        for _ in 0..6 {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            *transport.last_inbound_at.lock() = std::time::Instant::now();
            assert!(
                rx.try_recv().is_err(),
                "pending request must remain registered while frames arrive"
            );
        }
        assert_eq!(pending_request_count(&transport), 1);
    }

    #[tokio::test]
    async fn heartbeat_events_are_swallowed_before_the_event_fanout() {
        let transport = Arc::new(test_transport());
        let mut wire_events = transport.subscribe_wire_events();

        transport
            .handle_wire_frame(wire::ProtocolFrame::EventFrame(wire::EventFrame {
                schema: wire::protocol_schema(),
                ownership: wire::OwnershipScope::ConnectionOwnership(wire::ConnectionOwnership {
                    connection_id: "sidecar-transport".to_string(),
                }),
                payload: wire::EventPayload::StructuredEvent(wire::StructuredEvent {
                    name: "heartbeat".to_string(),
                    detail: std::collections::HashMap::new(),
                }),
            }))
            .await;
        // A non-heartbeat structured event still fans out, proving the filter
        // is name-scoped rather than dropping all structured events.
        transport
            .handle_wire_frame(wire::ProtocolFrame::EventFrame(wire::EventFrame {
                schema: wire::protocol_schema(),
                ownership: wire::OwnershipScope::ConnectionOwnership(wire::ConnectionOwnership {
                    connection_id: "conn-1".to_string(),
                }),
                payload: wire::EventPayload::StructuredEvent(wire::StructuredEvent {
                    name: "limit_warning".to_string(),
                    detail: std::collections::HashMap::new(),
                }),
            }))
            .await;

        let (_, payload) = wire_events.recv().await.expect("structured event");
        assert!(matches!(
            payload,
            wire::EventPayload::StructuredEvent(wire::StructuredEvent { name, .. })
                if name == "limit_warning"
        ));
        assert!(
            wire_events.try_recv().is_err(),
            "heartbeat must not fan out"
        );
    }

    #[test]
    fn pending_request_guard_removes_registered_slot_on_drop() {
        let transport = test_transport();
        let (tx, _rx) = oneshot::channel();
        transport
            .register_pending_request(1, tx)
            .expect("register pending request");

        {
            let _guard = PendingRequestGuard::new(&transport, 1);
            assert_eq!(pending_request_count(&transport), 1);
        }

        assert_eq!(pending_request_count(&transport), 0);
    }

    #[test]
    fn pending_request_limit_rejects_full_transport() {
        let transport = test_transport();
        for request_id in 1..=PENDING_REQUEST_LIMIT as wire::RequestId {
            let (tx, _rx) = oneshot::channel();
            transport
                .register_pending_request(request_id, tx)
                .expect("register pending request");
        }
        let (tx, _rx) = oneshot::channel();
        let error = transport
            .register_pending_request((PENDING_REQUEST_LIMIT + 1) as wire::RequestId, tx)
            .expect_err("full pending map should reject");

        assert!(
            error
                .to_string()
                .contains("sidecar pending request limit exceeded"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn shutdown_uses_typed_control_frame() {
        let (request_writer_tx, _request_writer_rx) = mpsc::channel(4);
        let (control_writer_tx, mut control_writer_rx) = mpsc::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let transport = SidecarTransport {
            child: parking_lot::Mutex::new(None),
            pending: SccHashMap::new(),
            pending_request_lock: parking_lot::Mutex::new(()),
            request_counter: AtomicI64::new(1),
            max_frame_bytes: AtomicUsize::new(wire::DEFAULT_MAX_FRAME_BYTES),
            event_tx,
            callbacks: SccHashMap::new(),
            request_writer_tx,
            control_writer_tx,
            last_inbound_at: parking_lot::Mutex::new(std::time::Instant::now()),
        };

        transport
            .shutdown("test complete")
            .await
            .expect("enqueue typed shutdown");
        let bytes = control_writer_rx.recv().await.expect("control frame bytes");
        let frame = WireFrameCodec::default()
            .decode(&bytes)
            .expect("decode shutdown frame");
        assert!(matches!(
            frame,
            wire::ProtocolFrame::ControlFrame(wire::ControlFrame {
                payload: wire::ControlPayload::ShutdownControl(wire::ShutdownControl { reason }),
                ..
            }) if reason == "test complete"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_spawn_maps_duplex_control_socket_to_fd_three() {
        let (mut parent, child) = StdUnixStream::pair().expect("control socketpair");
        parent
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("control read timeout");
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf mapped-control >&3");
        map_control_fd(&mut command, child).expect("map child control fd");
        let mut child = command.spawn().expect("spawn fd mapping probe");
        drop(command);

        let mut received = [0_u8; 14];
        std::io::Read::read_exact(&mut parent, &mut received).expect("read mapped fd output");
        assert_eq!(&received, b"mapped-control");
        assert!(child
            .wait()
            .await
            .expect("wait for mapping probe")
            .success());
    }

    #[tokio::test]
    async fn control_writer_progresses_while_ordinary_stream_is_blocked() {
        let transport = Arc::new(test_transport());
        let (ordinary_client, _ordinary_server) = tokio::io::duplex(1);
        let (control_client, mut control_server) = tokio::io::duplex(64);
        let (control_tx, control_rx) = mpsc::channel(CONTROL_FRAME_QUEUE_CAPACITY);
        let (request_tx, request_rx) = mpsc::channel(REQUEST_FRAME_QUEUE_CAPACITY);
        request_tx
            .send(vec![b'r'; 64])
            .await
            .expect("send request frame");
        control_tx
            .send(vec![b'c'])
            .await
            .expect("send control frame");
        let ordinary_writer = tokio::spawn(run_writer(
            Arc::downgrade(&transport),
            "ordinary",
            ordinary_client,
            request_rx,
        ));
        let control_writer = tokio::spawn(run_writer(
            Arc::downgrade(&transport),
            "control",
            control_client,
            control_rx,
        ));
        let mut first = [0u8; 1];
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            control_server.read_exact(&mut first),
        )
        .await
        .expect("control write must not wait for ordinary stream")
        .expect("read first byte");
        assert_eq!(first, [b'c']);
        ordinary_writer.abort();
        control_writer.abort();
    }

    #[tokio::test]
    async fn eof_on_either_inbound_lane_fails_pending_requests() {
        for lane in [InboundLane::Event, InboundLane::Control] {
            let transport = Arc::new(test_transport());
            let (tx, rx) = oneshot::channel();
            transport
                .register_pending_request(1, tx)
                .expect("register pending request");
            let (reader, peer) = tokio::io::duplex(64);
            drop(peer);

            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                run_reader(Arc::downgrade(&transport), reader, lane),
            )
            .await
            .expect("reader must terminate on EOF");

            rx.await.expect_err("EOF must fail pending requests");
            assert_eq!(pending_request_count(&transport), 0);
        }
    }

    #[tokio::test]
    async fn writer_preserves_order_within_one_lane() {
        let transport = Arc::new(test_transport());
        let (client, mut server) = tokio::io::duplex(64);
        let (tx, rx) = mpsc::channel(CONTROL_FRAME_QUEUE_CAPACITY);
        tx.send(vec![b'c']).await.expect("control one");
        tx.send(vec![b'C']).await.expect("control two");
        drop(tx);

        let writer = tokio::spawn(run_writer(
            Arc::downgrade(&transport),
            "control",
            client,
            rx,
        ));
        let mut output = [0u8; 2];
        server.read_exact(&mut output).await.expect("read output");
        writer.await.expect("writer task");

        assert_eq!(output, [b'c', b'C']);
    }
}
