use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::protocol::{
    CloseStdinRequest, EventFrame, EventPayload, ExecuteRequest, ExtEnvelope,
    GuestFilesystemCallRequest, GuestFilesystemResultResponse, KillProcessRequest, OwnershipScope,
    ProcessKilledResponse, ProcessStartedResponse, SidecarRequestPayload, SidecarResponsePayload,
    StdinClosedResponse, StdinWrittenResponse, WriteStdinRequest,
};
use crate::state::{SharedEventSink, SharedSidecarRequestClient, VmError};

pub type ExtensionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, VmError>> + 'a>>;

/// One projected agent package's launch surface, served from sidecar-owned VM
/// state (sourced from packed vbare manifests; packed packages ship no
/// `agentos-package.json` for extensions to read from the guest filesystem).
#[derive(Debug, Clone)]
pub struct ProjectedAgentLaunchEntry {
    pub id: String,
    pub acp_entrypoint: String,
    pub env: std::collections::BTreeMap<String, String>,
    pub launch_args: Vec<String>,
}

pub trait ExtensionHost {
    /// Return the VM-scoped ACP/session limits. Test hosts that do not own a VM
    /// use the same generous defaults as a normal sidecar.
    fn vm_acp_limits<'a>(
        &'a mut self,
        _ownership: OwnershipScope,
    ) -> ExtensionFuture<'a, crate::core::limits::AcpLimits> {
        Box::pin(async { Ok(crate::core::limits::AcpLimits::default()) })
    }

    /// Return the VM's single resolved SQLite handle. Reads through this handle
    /// never create another transport, connection pool, or database namespace.
    fn vm_database<'a>(
        &'a mut self,
        ownership: OwnershipScope,
    ) -> ExtensionFuture<'a, Option<crate::vm_sqlite::SharedVmSqliteDatabase>>;

    fn projected_agents<'a>(
        &'a mut self,
        ownership: OwnershipScope,
    ) -> ExtensionFuture<'a, Vec<ProjectedAgentLaunchEntry>>;

    fn spawn_process<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        request: ExecuteRequest,
    ) -> ExtensionFuture<'a, ProcessStartedResponse>;

    fn write_stdin<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        request: WriteStdinRequest,
    ) -> ExtensionFuture<'a, StdinWrittenResponse>;

    fn close_stdin<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        request: CloseStdinRequest,
    ) -> ExtensionFuture<'a, StdinClosedResponse>;

    fn kill_process<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        request: KillProcessRequest,
    ) -> ExtensionFuture<'a, ProcessKilledResponse>;

    fn poll_event<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        timeout: Duration,
    ) -> ExtensionFuture<'a, Option<EventFrame>>;

    fn guest_filesystem_call<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        request: GuestFilesystemCallRequest,
    ) -> ExtensionFuture<'a, GuestFilesystemResultResponse>;

    fn bind_process_to_session<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
    ) -> ExtensionFuture<'a, ()>;

    fn bind_vm_to_session<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
    ) -> ExtensionFuture<'a, ()>;

    fn dispose_session_resources<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
    ) -> ExtensionFuture<'a, Vec<EventFrame>>;

    fn start_buffering_process_output<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        process_id: String,
    ) -> ExtensionFuture<'a, ()>;

    fn handoff_buffered_process_output<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
        timeout: Duration,
    ) -> ExtensionFuture<'a, ExtensionBufferedProcessOutput>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionBufferedProcessOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl ExtensionBufferedProcessOutput {
    pub(crate) fn append_stdout(&mut self, chunk: &[u8], cap: usize) {
        self.stdout_truncated |= append_bounded_bytes(&mut self.stdout, chunk, cap);
    }

    pub(crate) fn append_stderr(&mut self, chunk: &[u8], cap: usize) {
        self.stderr_truncated |= append_bounded_bytes(&mut self.stderr, chunk, cap);
    }
}

fn append_bounded_bytes(buffer: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    buffer.extend_from_slice(chunk);
    if buffer.len() <= cap {
        return false;
    }
    let remove_len = buffer.len() - cap;
    buffer.drain(..remove_len);
    true
}

#[derive(Debug, Clone)]
pub struct ExtensionResponse {
    pub payload: Vec<u8>,
    pub events: Vec<EventFrame>,
}

impl ExtensionResponse {
    pub fn new(payload: Vec<u8>) -> Self {
        Self {
            payload,
            events: Vec::new(),
        }
    }

    pub fn with_events(payload: Vec<u8>, events: Vec<EventFrame>) -> Self {
        Self { payload, events }
    }

    pub fn with_wire_events(
        payload: Vec<u8>,
        events: Vec<crate::wire::EventFrame>,
    ) -> Result<Self, VmError> {
        let events = events
            .into_iter()
            .map(crate::wire::event_frame_to_compat)
            .collect::<Result<Vec<_>, _>>()
            .map_err(wire_protocol_error)?;
        Ok(Self { payload, events })
    }
}

#[derive(Clone)]
pub struct ExtensionSnapshot {
    namespace: String,
    ownership: OwnershipScope,
    sidecar_requests: SharedSidecarRequestClient,
    event_sink: SharedEventSink,
}

pub struct ExtensionContext<'a> {
    snapshot: ExtensionSnapshot,
    host: &'a mut dyn ExtensionHost,
}

impl ExtensionSnapshot {
    pub(crate) fn new(
        namespace: String,
        ownership: OwnershipScope,
        sidecar_requests: SharedSidecarRequestClient,
        event_sink: SharedEventSink,
    ) -> Self {
        Self {
            namespace,
            ownership,
            sidecar_requests,
            event_sink,
        }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn ownership(&self) -> &OwnershipScope {
        &self.ownership
    }

    pub fn ext_event(&self, payload: Vec<u8>) -> EventFrame {
        EventFrame::new(
            self.ownership.clone(),
            EventPayload::Ext(ExtEnvelope {
                namespace: self.namespace.clone(),
                payload,
            }),
        )
    }

    pub fn ext_event_wire(&self, payload: Vec<u8>) -> Result<crate::wire::EventFrame, VmError> {
        crate::wire::event_frame_from_compat(self.ext_event(payload)).map_err(wire_protocol_error)
    }

    /// Emit a wire event frame to the host the instant it is produced, rather
    /// than collecting it into the dispatch result and waiting for the whole
    /// request to resolve. Returns `Ok(None)` when delivered live, or
    /// `Ok(Some(event))` when no live sink is configured (in-process sidecar) so
    /// the caller can fall back to returning it in the dispatch's batch.
    pub fn emit_event_wire(
        &self,
        event: crate::wire::EventFrame,
    ) -> Result<Option<crate::wire::EventFrame>, VmError> {
        self.event_sink.try_emit(event)
    }

    /// Build a namespaced ext-event from `payload` and emit it live (see
    /// [`Self::emit_event_wire`]).
    pub fn emit_ext_event(
        &self,
        payload: Vec<u8>,
    ) -> Result<Option<crate::wire::EventFrame>, VmError> {
        let event = self.ext_event_wire(payload)?;
        self.emit_event_wire(event)
    }

    pub fn invoke_callback(&self, payload: Vec<u8>, timeout: Duration) -> Result<Vec<u8>, VmError> {
        let response = self.sidecar_requests.invoke(
            self.ownership.clone(),
            SidecarRequestPayload::Ext(ExtEnvelope {
                namespace: self.namespace.clone(),
                payload,
            }),
            timeout,
        )?;
        extension_callback_response_payload(&self.namespace, response)
    }
}

impl<'a> ExtensionContext<'a> {
    pub(crate) fn new(snapshot: ExtensionSnapshot, host: &'a mut dyn ExtensionHost) -> Self {
        Self { snapshot, host }
    }

    pub fn snapshot(&self) -> ExtensionSnapshot {
        self.snapshot.clone()
    }

    pub fn namespace(&self) -> &str {
        self.snapshot.namespace()
    }

    pub fn ownership(&self) -> &OwnershipScope {
        self.snapshot.ownership()
    }

    pub fn ext_event(&self, payload: Vec<u8>) -> EventFrame {
        self.snapshot.ext_event(payload)
    }

    pub fn ext_event_wire(&self, payload: Vec<u8>) -> Result<crate::wire::EventFrame, VmError> {
        self.snapshot.ext_event_wire(payload)
    }

    /// Emit a wire event frame to the host live (see
    /// [`ExtensionSnapshot::emit_event_wire`]).
    pub fn emit_event_wire(
        &self,
        event: crate::wire::EventFrame,
    ) -> Result<Option<crate::wire::EventFrame>, VmError> {
        self.snapshot.emit_event_wire(event)
    }

    /// Build a namespaced ext-event from `payload` and emit it live (see
    /// [`ExtensionSnapshot::emit_ext_event`]).
    pub fn emit_ext_event(
        &self,
        payload: Vec<u8>,
    ) -> Result<Option<crate::wire::EventFrame>, VmError> {
        self.snapshot.emit_ext_event(payload)
    }

    pub fn invoke_callback(&self, payload: Vec<u8>, timeout: Duration) -> Result<Vec<u8>, VmError> {
        self.snapshot.invoke_callback(payload, timeout)
    }

    pub async fn vm_database(
        &mut self,
    ) -> Result<Option<crate::vm_sqlite::SharedVmSqliteDatabase>, VmError> {
        self.host.vm_database(self.snapshot.ownership.clone()).await
    }

    pub async fn vm_acp_limits(&mut self) -> Result<crate::core::limits::AcpLimits, VmError> {
        self.host
            .vm_acp_limits(self.snapshot.ownership.clone())
            .await
    }

    pub async fn spawn_process(
        &mut self,
        request: ExecuteRequest,
    ) -> Result<ProcessStartedResponse, VmError> {
        self.host
            .spawn_process(self.snapshot.ownership.clone(), request)
            .await
    }

    pub async fn spawn_process_wire(
        &mut self,
        request: crate::wire::ExecuteRequest,
    ) -> Result<crate::wire::ProcessStartedResponse, VmError> {
        let payload = crate::wire::request_payload_to_compat(
            self.snapshot.ownership(),
            crate::wire::RequestPayload::ExecuteRequest(request),
        )
        .map_err(wire_protocol_error)?;
        let crate::protocol::RequestPayload::Execute(request) = payload else {
            return Err(unexpected_wire_request_payload("execute"));
        };
        let response = self.spawn_process(request).await?;
        let payload = crate::wire::response_payload_from_compat(
            self.snapshot.ownership(),
            crate::protocol::ResponsePayload::ProcessStarted(response),
        )
        .map_err(wire_protocol_error)?;
        let crate::wire::ResponsePayload::ProcessStartedResponse(response) = payload else {
            return Err(unexpected_wire_response_payload("process started"));
        };
        Ok(response)
    }

    pub async fn write_stdin(
        &mut self,
        request: WriteStdinRequest,
    ) -> Result<StdinWrittenResponse, VmError> {
        self.host
            .write_stdin(self.snapshot.ownership.clone(), request)
            .await
    }

    pub async fn write_stdin_wire(
        &mut self,
        request: crate::wire::WriteStdinRequest,
    ) -> Result<crate::wire::StdinWrittenResponse, VmError> {
        let payload = crate::wire::request_payload_to_compat(
            self.snapshot.ownership(),
            crate::wire::RequestPayload::WriteStdinRequest(request),
        )
        .map_err(wire_protocol_error)?;
        let crate::protocol::RequestPayload::WriteStdin(request) = payload else {
            return Err(unexpected_wire_request_payload("write stdin"));
        };
        let response = self.write_stdin(request).await?;
        let payload = crate::wire::response_payload_from_compat(
            self.snapshot.ownership(),
            crate::protocol::ResponsePayload::StdinWritten(response),
        )
        .map_err(wire_protocol_error)?;
        let crate::wire::ResponsePayload::StdinWrittenResponse(response) = payload else {
            return Err(unexpected_wire_response_payload("stdin written"));
        };
        Ok(response)
    }

    pub async fn close_stdin(
        &mut self,
        request: CloseStdinRequest,
    ) -> Result<StdinClosedResponse, VmError> {
        self.host
            .close_stdin(self.snapshot.ownership.clone(), request)
            .await
    }

    pub async fn close_stdin_wire(
        &mut self,
        request: crate::wire::CloseStdinRequest,
    ) -> Result<crate::wire::StdinClosedResponse, VmError> {
        let payload = crate::wire::request_payload_to_compat(
            self.snapshot.ownership(),
            crate::wire::RequestPayload::CloseStdinRequest(request),
        )
        .map_err(wire_protocol_error)?;
        let crate::protocol::RequestPayload::CloseStdin(request) = payload else {
            return Err(unexpected_wire_request_payload("close stdin"));
        };
        let response = self.close_stdin(request).await?;
        let payload = crate::wire::response_payload_from_compat(
            self.snapshot.ownership(),
            crate::protocol::ResponsePayload::StdinClosed(response),
        )
        .map_err(wire_protocol_error)?;
        let crate::wire::ResponsePayload::StdinClosedResponse(response) = payload else {
            return Err(unexpected_wire_response_payload("stdin closed"));
        };
        Ok(response)
    }

    pub async fn kill_process(
        &mut self,
        request: KillProcessRequest,
    ) -> Result<ProcessKilledResponse, VmError> {
        self.host
            .kill_process(self.snapshot.ownership.clone(), request)
            .await
    }

    pub async fn kill_process_wire(
        &mut self,
        request: crate::wire::KillProcessRequest,
    ) -> Result<crate::wire::ProcessKilledResponse, VmError> {
        let payload = crate::wire::request_payload_to_compat(
            self.snapshot.ownership(),
            crate::wire::RequestPayload::KillProcessRequest(request),
        )
        .map_err(wire_protocol_error)?;
        let crate::protocol::RequestPayload::KillProcess(request) = payload else {
            return Err(unexpected_wire_request_payload("kill process"));
        };
        let response = self.kill_process(request).await?;
        let payload = crate::wire::response_payload_from_compat(
            self.snapshot.ownership(),
            crate::protocol::ResponsePayload::ProcessKilled(response),
        )
        .map_err(wire_protocol_error)?;
        let crate::wire::ResponsePayload::ProcessKilledResponse(response) = payload else {
            return Err(unexpected_wire_response_payload("process killed"));
        };
        Ok(response)
    }

    pub async fn poll_event(&mut self, timeout: Duration) -> Result<Option<EventFrame>, VmError> {
        self.host
            .poll_event(self.snapshot.ownership.clone(), timeout)
            .await
    }

    pub async fn poll_event_wire(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<crate::wire::EventFrame>, VmError> {
        self.poll_event(timeout)
            .await?
            .map(crate::wire::event_frame_from_compat)
            .transpose()
            .map_err(wire_protocol_error)
    }

    pub async fn guest_filesystem_call(
        &mut self,
        request: GuestFilesystemCallRequest,
    ) -> Result<GuestFilesystemResultResponse, VmError> {
        self.host
            .guest_filesystem_call(self.snapshot.ownership.clone(), request)
            .await
    }

    /// Enumerate the VM's projected agent packages (id + launch surface) from
    /// sidecar-owned state. This is the agent source of truth for extensions;
    /// it reflects `ConfigureVm` and live `LinkPackage` updates.
    pub async fn projected_agents(&mut self) -> Result<Vec<ProjectedAgentLaunchEntry>, VmError> {
        let ownership = self.snapshot.ownership().clone();
        self.host.projected_agents(ownership).await
    }

    pub async fn guest_filesystem_call_wire(
        &mut self,
        request: crate::wire::GuestFilesystemCallRequest,
    ) -> Result<crate::wire::GuestFilesystemResultResponse, VmError> {
        let payload = crate::wire::request_payload_to_compat(
            self.snapshot.ownership(),
            crate::wire::RequestPayload::GuestFilesystemCallRequest(request),
        )
        .map_err(wire_protocol_error)?;
        let crate::protocol::RequestPayload::GuestFilesystemCall(request) = payload else {
            return Err(unexpected_wire_request_payload("guest filesystem call"));
        };
        let response = self.guest_filesystem_call(request).await?;
        let payload = crate::wire::response_payload_from_compat(
            self.snapshot.ownership(),
            crate::protocol::ResponsePayload::GuestFilesystemResult(response),
        )
        .map_err(wire_protocol_error)?;
        let crate::wire::ResponsePayload::GuestFilesystemResultResponse(response) = payload else {
            return Err(unexpected_wire_response_payload("guest filesystem result"));
        };
        Ok(response)
    }

    pub async fn bind_process_to_session(
        &mut self,
        ext_session_id: impl Into<String>,
        process_id: impl Into<String>,
    ) -> Result<(), VmError> {
        self.host
            .bind_process_to_session(
                self.snapshot.ownership.clone(),
                self.snapshot.namespace.clone(),
                ext_session_id.into(),
                process_id.into(),
            )
            .await
    }

    pub async fn bind_vm_to_session(
        &mut self,
        ext_session_id: impl Into<String>,
    ) -> Result<(), VmError> {
        self.host
            .bind_vm_to_session(
                self.snapshot.ownership.clone(),
                self.snapshot.namespace.clone(),
                ext_session_id.into(),
            )
            .await
    }

    pub async fn dispose_session_resources(
        &mut self,
        ext_session_id: impl Into<String>,
    ) -> Result<Vec<EventFrame>, VmError> {
        self.host
            .dispose_session_resources(
                self.snapshot.ownership.clone(),
                self.snapshot.namespace.clone(),
                ext_session_id.into(),
            )
            .await
    }

    pub async fn dispose_session_resources_wire(
        &mut self,
        ext_session_id: impl Into<String>,
    ) -> Result<Vec<crate::wire::EventFrame>, VmError> {
        self.dispose_session_resources(ext_session_id)
            .await?
            .into_iter()
            .map(crate::wire::event_frame_from_compat)
            .collect::<Result<Vec<_>, _>>()
            .map_err(wire_protocol_error)
    }

    pub async fn start_buffering_process_output(
        &mut self,
        process_id: impl Into<String>,
    ) -> Result<(), VmError> {
        self.host
            .start_buffering_process_output(self.snapshot.ownership.clone(), process_id.into())
            .await
    }

    pub async fn handoff_buffered_process_output(
        &mut self,
        ext_session_id: impl Into<String>,
        process_id: impl Into<String>,
        timeout: Duration,
    ) -> Result<ExtensionBufferedProcessOutput, VmError> {
        self.host
            .handoff_buffered_process_output(
                self.snapshot.ownership.clone(),
                self.snapshot.namespace.clone(),
                ext_session_id.into(),
                process_id.into(),
                timeout,
            )
            .await
    }
}

fn wire_protocol_error(error: crate::wire::ProtocolCodecError) -> VmError {
    VmError::InvalidState(format!("invalid generated wire protocol frame: {error}"))
}

fn unexpected_wire_request_payload(operation: &str) -> VmError {
    VmError::InvalidState(format!(
        "generated wire {operation} request converted to the wrong compatibility payload"
    ))
}

fn unexpected_wire_response_payload(operation: &str) -> VmError {
    VmError::InvalidState(format!(
        "compatibility {operation} response converted to the wrong generated wire payload"
    ))
}

fn extension_callback_response_payload(
    namespace: &str,
    response: SidecarResponsePayload,
) -> Result<Vec<u8>, VmError> {
    match response {
        SidecarResponsePayload::ExtResult(envelope) if envelope.namespace == namespace => {
            Ok(envelope.payload)
        }
        SidecarResponsePayload::ExtResult(envelope) => Err(VmError::InvalidState(format!(
            "extension callback response namespace {} did not match {}",
            envelope.namespace, namespace
        ))),
        SidecarResponsePayload::HostCallbackResult(_)
        | SidecarResponsePayload::JsBridgeResult(_) => Err(VmError::InvalidState(String::from(
            "extension callback received a non-extension response",
        ))),
    }
}

pub enum ExtensionInterruptRequest<'a> {
    ExtensionPayload {
        payload: &'a [u8],
        ownership: &'a OwnershipScope,
    },
    KillProcess,
}

#[derive(Debug, Clone)]
pub struct ExtensionInterruptResponse {
    /// Whether the active request future should be dropped and replaced by the
    /// synthetic response. Cooperative interrupts can signal the active future
    /// and leave this false so it commits its own terminal state.
    pub interrupt_active: bool,
    pub interrupted_response_payload: Vec<u8>,
    pub interrupting_response_payload: Option<Vec<u8>>,
}

pub trait Extension: Send + Sync {
    fn namespace(&self) -> &str;

    fn handle_request<'a>(
        &'a self,
        ctx: ExtensionContext<'a>,
        payload: Vec<u8>,
    ) -> ExtensionFuture<'a, ExtensionResponse>;

    /// Register/migrate extension-owned schemas during VM database bootstrap,
    /// before VFS or extension requests can observe the VM.
    fn bootstrap_vm_database<'a>(
        &'a self,
        _database: crate::vm_sqlite::SharedVmSqliteDatabase,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn on_vm_created<'a>(&'a self, _ctx: ExtensionSnapshot) -> ExtensionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    /// Per-session teardown hook. The host invokes this for every registered
    /// extension when a session is disposed because its connection closed
    /// (`DisposeReason::ConnectionClosed`), giving the extension the disposed
    /// session's ownership scope so it can release the per-session state it
    /// keyed on that session. Default is a no-op. This is the only signal an
    /// extension receives that a client has disconnected, so it is what lets an
    /// ACP-style extension free per-session state instead of leaking it for the
    /// process lifetime.
    fn on_session_disposed<'a>(&'a self, _ctx: ExtensionSnapshot) -> ExtensionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn is_blocking_request(&self, _payload: &[u8]) -> bool {
        false
    }

    fn interrupt_blocking_request(
        &self,
        _blocking_payload: &[u8],
        _interrupt: ExtensionInterruptRequest<'_>,
    ) -> Option<ExtensionInterruptResponse> {
        None
    }

    fn on_dispose<'a>(&'a self) -> ExtensionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod live_event_tests {
    use super::*;
    use crate::state::EventSinkTransport;
    use std::sync::{Arc, Mutex};

    /// Records every event handed to the live sink, standing in for the stdio
    /// `FrameEventTransport` that writes `ProtocolFrame::EventFrame`s to stdout.
    #[derive(Default)]
    struct RecordingEventSink {
        events: Arc<Mutex<Vec<crate::wire::EventFrame>>>,
    }

    impl EventSinkTransport for RecordingEventSink {
        fn emit_event(&self, event: crate::wire::EventFrame) -> Result<(), VmError> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }
    }

    fn snapshot_with_sink(event_sink: SharedEventSink) -> ExtensionSnapshot {
        ExtensionSnapshot::new(
            String::from("dev.rivet.test.live-event"),
            OwnershipScope::session("conn-live", "sess-live"),
            SharedSidecarRequestClient::default(),
            event_sink,
        )
    }

    // With a transport configured (the stdio path), an ext event is emitted live
    // and `emit_ext_event` reports nothing left to batch.
    #[test]
    fn emit_ext_event_streams_live_when_sink_configured() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut sink = SharedEventSink::default();
        sink.set_transport(Arc::new(RecordingEventSink {
            events: recorded.clone(),
        }));

        let snapshot = snapshot_with_sink(sink);
        let leftover = snapshot
            .emit_ext_event(b"live-update".to_vec())
            .expect("emit must succeed");

        assert!(
            leftover.is_none(),
            "a configured sink consumes the event live, leaving nothing to batch"
        );
        let recorded = recorded.lock().unwrap();
        assert_eq!(recorded.len(), 1, "the event must reach the live transport");
        // The live frame round-trips back to the namespaced ext payload.
        let compat = crate::wire::event_frame_to_compat(recorded[0].clone())
            .expect("recorded frame converts back to compat");
        match compat.payload {
            EventPayload::Ext(envelope) => {
                assert_eq!(envelope.namespace, "dev.rivet.test.live-event");
                assert_eq!(envelope.payload, b"live-update");
            }
            other => panic!("unexpected live event payload: {other:?}"),
        }
    }

    // With no transport (an in-process sidecar), the event is handed back so the
    // caller can fall back to the dispatch-result batch — preserving delivery.
    #[test]
    fn emit_ext_event_falls_back_to_batch_without_sink() {
        let snapshot = snapshot_with_sink(SharedEventSink::default());
        let leftover = snapshot
            .emit_ext_event(b"batched-update".to_vec())
            .expect("emit must succeed");

        let frame = leftover.expect("without a sink the event is returned for batching");
        let compat = crate::wire::event_frame_to_compat(frame)
            .expect("returned frame converts back to compat");
        match compat.payload {
            EventPayload::Ext(envelope) => assert_eq!(envelope.payload, b"batched-update"),
            other => panic!("unexpected batched event payload: {other:?}"),
        }
    }
}
