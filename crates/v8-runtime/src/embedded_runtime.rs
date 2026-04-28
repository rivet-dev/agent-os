use std::collections::HashMap;
use std::collections::HashSet;
use std::io::{self, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;

use crate::host_call::CallIdRouter;
use crate::ipc_binary::BinaryFrame;
use crate::runtime_protocol::{
    BridgeResponse, RuntimeCommand, RuntimeEvent, SessionMessage, StreamEvent,
};
use crate::session::SessionManager;
use crate::snapshot::SnapshotCache;
use crate::{bridge, isolate};

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

pub struct EmbeddedV8Runtime {
    session_mgr: Arc<Mutex<SessionManager>>,
    session_outputs: Arc<Mutex<HashMap<String, mpsc::Sender<RuntimeEvent>>>>,
    snapshot_cache: Arc<SnapshotCache>,
    alive: Arc<AtomicBool>,
}

impl EmbeddedV8Runtime {
    pub fn new(max_concurrency: Option<usize>) -> io::Result<Self> {
        bridge::init_codec();
        bridge::acquire_embedded_cbor_codec();
        isolate::init_v8_platform();

        let snapshot_cache = Arc::new(SnapshotCache::new(4));
        let (event_tx, event_rx) = crossbeam_channel::bounded::<RuntimeEvent>(1024);
        let call_id_router: CallIdRouter = Arc::new(Mutex::new(HashMap::new()));
        let session_mgr = Arc::new(Mutex::new(SessionManager::new(
            max_concurrency.unwrap_or_else(default_max_concurrency),
            event_tx,
            call_id_router,
            Arc::clone(&snapshot_cache),
        )));
        let session_outputs = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let alive_for_thread = Arc::clone(&alive);
        let session_outputs_for_thread = Arc::clone(&session_outputs);

        thread::Builder::new()
            .name(String::from("agent-os-v8-runtime-dispatch"))
            .spawn(move || {
                while let Ok(event) = event_rx.recv() {
                    route_outbound_event(event, &session_outputs_for_thread);
                }
                alive_for_thread.store(false, Ordering::Release);
            })
            .inspect_err(|_| bridge::release_embedded_cbor_codec())?;

        Ok(Self {
            session_mgr,
            session_outputs,
            snapshot_cache,
            alive,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn register_session(&self, session_id: &str) -> io::Result<mpsc::Receiver<RuntimeEvent>> {
        let (sender, receiver) = mpsc::channel();
        let mut outputs = self
            .session_outputs
            .lock()
            .expect("embedded runtime session outputs lock poisoned");
        if outputs.contains_key(session_id) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("session output {session_id} already exists"),
            ));
        }
        outputs.insert(session_id.to_owned(), sender);
        Ok(receiver)
    }

    pub fn unregister_session(&self, session_id: &str) {
        self.session_outputs
            .lock()
            .expect("embedded runtime session outputs lock poisoned")
            .remove(session_id);
    }

    pub fn session_handle(self: &Arc<Self>, session_id: String) -> EmbeddedV8SessionHandle {
        EmbeddedV8SessionHandle {
            session_id,
            runtime: Arc::clone(self),
        }
    }

    pub fn dispatch(&self, command: RuntimeCommand) -> io::Result<()> {
        dispatch_runtime_command(&self.session_mgr, &self.snapshot_cache, command)
    }

    pub fn session_count(&self) -> usize {
        self.session_mgr
            .lock()
            .expect("embedded runtime session manager lock poisoned")
            .session_count()
    }

    pub fn active_slot_count(&self) -> usize {
        self.session_mgr
            .lock()
            .expect("embedded runtime session manager lock poisoned")
            .active_slot_count()
    }
}

pub struct EmbeddedV8SessionHandle {
    session_id: String,
    runtime: Arc<EmbeddedV8Runtime>,
}

impl EmbeddedV8SessionHandle {
    pub fn send_bridge_response(
        &self,
        call_id: u64,
        status: u8,
        payload: Vec<u8>,
    ) -> io::Result<()> {
        self.runtime.dispatch(RuntimeCommand::SendToSession {
            session_id: self.session_id.clone(),
            message: SessionMessage::BridgeResponse(BridgeResponse {
                call_id,
                status,
                payload,
            }),
        })
    }

    pub fn send_stream_event(&self, event_type: &str, payload: Vec<u8>) -> io::Result<()> {
        self.runtime.dispatch(RuntimeCommand::SendToSession {
            session_id: self.session_id.clone(),
            message: SessionMessage::StreamEvent(StreamEvent {
                event_type: event_type.to_owned(),
                payload,
            }),
        })
    }

    pub fn terminate(&self) -> io::Result<()> {
        self.runtime.dispatch(RuntimeCommand::SendToSession {
            session_id: self.session_id.clone(),
            message: SessionMessage::TerminateExecution,
        })
    }

    pub fn destroy(&self) -> io::Result<()> {
        self.runtime.unregister_session(&self.session_id);
        self.runtime.dispatch(RuntimeCommand::DestroySession {
            session_id: self.session_id.clone(),
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl Clone for EmbeddedV8SessionHandle {
    fn clone(&self) -> Self {
        Self {
            session_id: self.session_id.clone(),
            runtime: Arc::clone(&self.runtime),
        }
    }
}

pub fn shared_embedded_runtime() -> io::Result<Arc<EmbeddedV8Runtime>> {
    static SHARED_RUNTIME: OnceLock<Arc<EmbeddedV8Runtime>> = OnceLock::new();
    static SHARED_RUNTIME_INIT_LOCK: Mutex<()> = Mutex::new(());

    if let Some(shared) = SHARED_RUNTIME.get() {
        return Ok(Arc::clone(shared));
    }

    let _guard = SHARED_RUNTIME_INIT_LOCK
        .lock()
        .expect("shared embedded runtime init lock poisoned");
    if let Some(shared) = SHARED_RUNTIME.get() {
        return Ok(Arc::clone(shared));
    }

    let shared = Arc::new(EmbeddedV8Runtime::new(None)?);
    let _ = SHARED_RUNTIME.set(Arc::clone(&shared));
    Ok(shared)
}

pub struct EmbeddedRuntimeHandle {
    alive: Arc<AtomicBool>,
    codec_released: AtomicBool,
    shutdown_stream: UnixStream,
    join_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl EmbeddedRuntimeHandle {
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn shutdown(&self) {
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
        if let Ok(mut guard) = self.join_handle.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
        self.release_codec();
    }

    fn release_codec(&self) {
        if !self.codec_released.swap(true, Ordering::AcqRel) {
            bridge::release_embedded_cbor_codec();
        }
    }
}

impl Drop for EmbeddedRuntimeHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
        if let Some(handle) = self.join_handle.get_mut().ok().and_then(Option::take) {
            let _ = handle.join();
        }
        self.release_codec();
    }
}

pub fn spawn_embedded_runtime_ipc(
    max_concurrency: Option<usize>,
) -> io::Result<(UnixStream, EmbeddedRuntimeHandle)> {
    bridge::init_codec();
    bridge::acquire_embedded_cbor_codec();
    isolate::init_v8_platform();

    let (host_stream, runtime_stream) = UnixStream::pair()?;
    let shutdown_stream = host_stream.try_clone()?;
    let alive = Arc::new(AtomicBool::new(true));
    let alive_for_thread = Arc::clone(&alive);
    let max_concurrency = max_concurrency.unwrap_or_else(default_max_concurrency);

    let join_handle = thread::Builder::new()
        .name(String::from("agent-os-v8-runtime"))
        .spawn(move || {
            run_embedded_runtime(runtime_stream, max_concurrency);
            alive_for_thread.store(false, Ordering::Release);
        })
        .inspect_err(|_| bridge::release_embedded_cbor_codec())?;

    Ok((
        host_stream,
        EmbeddedRuntimeHandle {
            alive,
            codec_released: AtomicBool::new(false),
            shutdown_stream,
            join_handle: Mutex::new(Some(join_handle)),
        },
    ))
}

fn default_max_concurrency() -> usize {
    thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
}

fn run_embedded_runtime(stream: UnixStream, max_concurrency: usize) {
    let snapshot_cache = Arc::new(SnapshotCache::new(4));
    let writer_stream = match stream.try_clone() {
        Ok(writer_stream) => writer_stream,
        Err(error) => {
            eprintln!("embedded V8 runtime failed to clone stream: {error}");
            return;
        }
    };
    let (event_tx, event_rx) = crossbeam_channel::bounded::<RuntimeEvent>(1024);
    let call_id_router: CallIdRouter = Arc::new(Mutex::new(HashMap::new()));
    let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);

    let writer_handle = match thread::Builder::new()
        .name(format!("v8-ipc-writer-{connection_id}"))
        .spawn(move || ipc_writer_thread(event_rx, writer_stream))
    {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("embedded V8 runtime failed to spawn writer thread: {error}");
            return;
        }
    };

    let session_mgr = Arc::new(Mutex::new(SessionManager::new(
        max_concurrency,
        event_tx,
        call_id_router,
        Arc::clone(&snapshot_cache),
    )));

    handle_connection(stream, connection_id, session_mgr, snapshot_cache);
    let _ = writer_handle.join();
}

fn ipc_writer_thread(rx: crossbeam_channel::Receiver<RuntimeEvent>, mut writer: UnixStream) {
    while let Ok(event) = rx.recv() {
        let frame: BinaryFrame = event.into();
        let bytes = match crate::ipc_binary::frame_to_bytes(&frame) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("embedded V8 runtime writer encode error: {error}");
                break;
            }
        };
        if let Err(error) = writer.write_all(&bytes) {
            eprintln!("embedded V8 runtime writer error: {error}");
            break;
        }
    }
}

fn handle_connection(
    mut stream: UnixStream,
    connection_id: u64,
    session_mgr: Arc<Mutex<SessionManager>>,
    snapshot_cache: Arc<SnapshotCache>,
) {
    let mut session_ids = HashSet::new();

    loop {
        let frame = match crate::ipc_binary::read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(ref error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => {
                eprintln!("embedded V8 runtime read error on connection {connection_id}: {error}");
                break;
            }
        };

        let command = match RuntimeCommand::try_from(frame) {
            Ok(command) => command,
            Err(error) => {
                eprintln!(
                    "embedded V8 runtime dispatch error on connection {connection_id}: {error}"
                );
                continue;
            }
        };

        if let RuntimeCommand::CreateSession { session_id, .. } = &command {
            session_ids.insert(session_id.clone());
        } else if let RuntimeCommand::DestroySession { session_id } = &command {
            session_ids.remove(session_id);
        }

        if let Err(error) = dispatch_runtime_command(&session_mgr, &snapshot_cache, command) {
            eprintln!("embedded V8 runtime dispatch error on connection {connection_id}: {error}");
        }
    }

    let mut mgr = session_mgr.lock().expect("session manager lock poisoned");
    mgr.destroy_sessions(session_ids);
}

fn dispatch_runtime_command(
    session_mgr: &Arc<Mutex<SessionManager>>,
    snapshot_cache: &Arc<SnapshotCache>,
    command: RuntimeCommand,
) -> io::Result<()> {
    match command {
        RuntimeCommand::CreateSession {
            session_id,
            heap_limit_mb,
            cpu_time_limit_ms,
        } => {
            let mut mgr = session_mgr.lock().expect("session manager lock poisoned");
            mgr.create_session(session_id, heap_limit_mb, cpu_time_limit_ms)
                .map_err(other_io_error)
        }
        RuntimeCommand::DestroySession { session_id } => {
            let mut mgr = session_mgr.lock().expect("session manager lock poisoned");
            mgr.destroy_session(&session_id).map_err(other_io_error)
        }
        RuntimeCommand::SendToSession {
            session_id,
            message: SessionMessage::BridgeResponse(response),
        } => {
            let mgr = session_mgr.lock().expect("session manager lock poisoned");
            let routed_session_id = mgr
                .call_id_router()
                .lock()
                .expect("call_id router lock poisoned")
                .remove(&response.call_id)
                .unwrap_or(session_id);
            mgr.send_to_session(&routed_session_id, SessionMessage::BridgeResponse(response))
                .map_err(other_io_error)
        }
        RuntimeCommand::SendToSession {
            session_id,
            message,
        } => {
            let mgr = session_mgr.lock().expect("session manager lock poisoned");
            mgr.send_to_session(&session_id, message)
                .map_err(other_io_error)
        }
        RuntimeCommand::WarmSnapshot { bridge_code } => snapshot_cache
            .get_or_create(&bridge_code)
            .map(|_| ())
            .map_err(other_io_error),
    }
}

fn route_outbound_event(
    event: RuntimeEvent,
    session_outputs: &Arc<Mutex<HashMap<String, mpsc::Sender<RuntimeEvent>>>>,
) {
    let session_id = event.session_id().to_owned();

    let sender = session_outputs
        .lock()
        .expect("embedded runtime session outputs lock poisoned")
        .get(&session_id)
        .cloned();

    if let Some(sender) = sender {
        if sender.send(event).is_err() {
            session_outputs
                .lock()
                .expect("embedded runtime session outputs lock poisoned")
                .remove(&session_id);
        }
    }
}

fn other_io_error(message: String) -> io::Error {
    io::Error::other(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_protocol::{BridgeResponse, RuntimeCommand, RuntimeEvent, SessionMessage};
    use std::time::Duration;

    #[test]
    fn embedded_runtime_handle_reports_liveness_and_shutdown() {
        let (_stream, handle) =
            spawn_embedded_runtime_ipc(Some(1)).expect("spawn embedded runtime");
        assert!(
            handle.is_alive(),
            "embedded runtime should be alive after spawn"
        );
        handle.shutdown();
        assert!(
            !handle.is_alive(),
            "embedded runtime should report not alive after shutdown"
        );
    }

    #[test]
    fn embedded_runtime_session_shared_runtime_is_lazy() {
        let first = shared_embedded_runtime().expect("shared embedded runtime");
        let second = shared_embedded_runtime().expect("shared embedded runtime");
        assert!(
            Arc::ptr_eq(&first, &second),
            "shared_embedded_runtime() should reuse the same runtime instance"
        );
    }

    #[test]
    fn embedded_runtime_stream_bridge_response_routing_prefers_call_id_router() {
        let snapshot_cache = Arc::new(SnapshotCache::new(1));
        let (event_tx, _event_rx) = crossbeam_channel::unbounded::<RuntimeEvent>();
        let call_id_router: CallIdRouter = Arc::new(Mutex::new(HashMap::new()));
        let session_mgr = Arc::new(Mutex::new(SessionManager::new(
            1,
            event_tx,
            Arc::clone(&call_id_router),
            Arc::clone(&snapshot_cache),
        )));

        {
            let mut mgr = session_mgr.lock().expect("session manager");
            mgr.create_session("stream-target".into(), None, None)
                .expect("create target session");
        }
        call_id_router
            .lock()
            .expect("call_id router")
            .insert(41, "stream-target".into());

        dispatch_runtime_command(
            &session_mgr,
            &snapshot_cache,
            RuntimeCommand::SendToSession {
                session_id: "wrong-session".into(),
                message: SessionMessage::BridgeResponse(BridgeResponse {
                    call_id: 41,
                    status: 0,
                    payload: vec![0xAB],
                }),
            },
        )
        .expect("bridge response should route via call_id table");

        assert!(
            call_id_router
                .lock()
                .expect("call_id router")
                .get(&41)
                .is_none(),
            "bridge response routing should consume the call_id entry"
        );

        session_mgr
            .lock()
            .expect("session manager")
            .destroy_session("stream-target")
            .expect("destroy target session");
    }

    #[test]
    fn embedded_runtime_stream_events_preserve_order_per_session() {
        let (sender, receiver) = mpsc::channel();
        let session_outputs = Arc::new(Mutex::new(HashMap::from([(
            String::from("stream-order"),
            sender,
        )])));

        route_outbound_event(
            RuntimeEvent::Log {
                session_id: "stream-order".into(),
                channel: 0,
                message: "first".into(),
            },
            &session_outputs,
        );
        route_outbound_event(
            RuntimeEvent::StreamCallback {
                session_id: "stream-order".into(),
                callback_type: "stdin".into(),
                payload: vec![1, 2, 3],
            },
            &session_outputs,
        );

        let first = receiver
            .recv_timeout(Duration::from_millis(100))
            .expect("first event");
        let second = receiver
            .recv_timeout(Duration::from_millis(100))
            .expect("second event");

        assert!(matches!(
            first,
            RuntimeEvent::Log { ref message, .. } if message == "first"
        ));
        assert!(matches!(
            second,
            RuntimeEvent::StreamCallback { ref callback_type, ref payload, .. }
                if callback_type == "stdin" && payload == &vec![1, 2, 3]
        ));
    }

    #[test]
    fn embedded_runtime_stream_termination_race_drops_late_events_after_receiver_close() {
        let (sender, receiver) = mpsc::channel();
        let session_outputs = Arc::new(Mutex::new(HashMap::from([(
            String::from("stream-race"),
            sender,
        )])));
        drop(receiver);

        route_outbound_event(
            RuntimeEvent::ExecutionResult {
                session_id: "stream-race".into(),
                exit_code: 0,
                exports: None,
                error: None,
            },
            &session_outputs,
        );

        assert!(
            session_outputs
                .lock()
                .expect("session outputs")
                .get("stream-race")
                .is_none(),
            "late events should drop stale receiver registrations during teardown races"
        );
    }
}
