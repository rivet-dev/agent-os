use std::collections::HashMap;
use std::collections::HashSet;
use std::io::{self, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use crate::host_call::{record_sync_bridge_host_phase, BridgeCallRegistry, CallIdRouter};
use crate::ipc_binary::BinaryFrame;
#[cfg(test)]
use crate::runtime_protocol::RuntimeEvent;
use crate::runtime_protocol::{
    validate_bridge_response_status, BridgeResponse, ModuleReaderHandle, RuntimeCommand,
    SessionMessage, StreamEvent,
};
use crate::session::{
    runtime_event_output_channel, RuntimeEventEnvelope, RuntimeEventOutputReceiver,
    RuntimeEventOutputSender, SessionCommand, SessionManager,
};
use crate::snapshot::SnapshotCache;
use crate::{bridge, isolate};
use agentos_runtime::accounting::ResourceClass;

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
const TEST_SESSION_OUTPUT_CHANNEL_CAPACITY: usize = 1024;

pub struct EmbeddedV8Runtime {
    session_mgr: Arc<Mutex<SessionManager>>,
    session_outputs: Arc<Mutex<HashMap<String, SessionOutput>>>,
    snapshot_cache: Arc<SnapshotCache>,
    alive: Arc<AtomicBool>,
    next_output_generation: AtomicU64,
    runtime: agentos_runtime::RuntimeContext,
    executor_teardown_timeout: Duration,
}

#[derive(Clone)]
struct SessionOutput {
    generation: u64,
    sender: RuntimeEventOutputSender,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedV8SessionOutputRegistration {
    session_id: String,
    generation: u64,
}

impl EmbeddedV8Runtime {
    pub fn new(
        max_concurrency: Option<usize>,
        runtime: agentos_runtime::RuntimeContext,
    ) -> io::Result<Self> {
        bridge::init_codec();
        bridge::acquire_embedded_cbor_codec();
        isolate::init_v8_platform();

        // Keep bridge-only, agent-SDK, and wasm-runner userland variants warm
        // without immediately evicting each other.
        let snapshot_cache = Arc::new(SnapshotCache::new(8));
        let call_id_router: CallIdRouter = Arc::new(BridgeCallRegistry::with_default_limit());
        let configured_max_concurrency = runtime.max_active_vm_executors();
        let executor_teardown_timeout = runtime.vm_executor_teardown_timeout();
        let session_mgr = Arc::new(Mutex::new(SessionManager::new(
            max_concurrency.unwrap_or(configured_max_concurrency),
            crate::session::RuntimeEventSender::closed(),
            call_id_router,
            Arc::clone(&snapshot_cache),
            runtime.clone(),
        )));
        let session_outputs = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));

        Ok(Self {
            session_mgr,
            session_outputs,
            snapshot_cache,
            alive,
            next_output_generation: AtomicU64::new(1),
            runtime,
            executor_teardown_timeout,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn snapshot_ready(&self, bridge_code: &str, userland_code: &str) -> bool {
        if userland_code.is_empty() {
            return true;
        }
        self.snapshot_cache
            .try_get_with_userland(bridge_code, Some(userland_code))
            .is_some()
    }

    pub fn pre_warm_workers(
        &self,
        bridge_code: String,
        userland_code: String,
        heap_limit_mb: Option<u32>,
        count: usize,
    ) {
        self.session_mgr
            .lock()
            .expect("embedded runtime session manager lock poisoned")
            .pre_warm_workers(bridge_code, userland_code, heap_limit_mb, count);
    }

    pub fn register_session(&self, session_id: &str) -> io::Result<RuntimeEventOutputReceiver> {
        self.register_session_with_output_registration(session_id)
            .map(|(receiver, _registration)| receiver)
    }

    pub fn register_session_with_output_registration(
        &self,
        session_id: &str,
    ) -> io::Result<(
        RuntimeEventOutputReceiver,
        EmbeddedV8SessionOutputRegistration,
    )> {
        self.register_session_with_runtime(session_id, &self.runtime)
    }

    pub fn register_session_with_runtime(
        &self,
        session_id: &str,
        runtime: &agentos_runtime::RuntimeContext,
    ) -> io::Result<(
        RuntimeEventOutputReceiver,
        EmbeddedV8SessionOutputRegistration,
    )> {
        let capacity = crate::session::configured_resource_capacity(
            runtime,
            ResourceClass::AsyncCompletions,
            "limits.reactor.maxAsyncCompletions",
            "runtime.resources.maxAsyncCompletions",
        )
        .map_err(other_io_error)?;
        self.register_session_with_capacity(session_id, capacity, Arc::clone(runtime.resources()))
    }

    fn register_session_with_capacity(
        &self,
        session_id: &str,
        capacity: usize,
        resources: Arc<agentos_runtime::accounting::ResourceLedger>,
    ) -> io::Result<(
        RuntimeEventOutputReceiver,
        EmbeddedV8SessionOutputRegistration,
    )> {
        let (sender, receiver) = runtime_event_output_channel(capacity, resources);
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
        let generation = self.next_output_generation.fetch_add(1, Ordering::Relaxed);
        outputs.insert(session_id.to_owned(), SessionOutput { generation, sender });
        Ok((
            receiver,
            EmbeddedV8SessionOutputRegistration {
                session_id: session_id.to_owned(),
                generation,
            },
        ))
    }

    pub fn unregister_session(&self, session_id: &str) {
        self.session_outputs
            .lock()
            .expect("embedded runtime session outputs lock poisoned")
            .remove(session_id);
    }

    pub fn destroy_session_if_output_current(
        &self,
        registration: &EmbeddedV8SessionOutputRegistration,
    ) -> io::Result<bool> {
        let output_is_current = self
            .session_outputs
            .lock()
            .expect("embedded runtime session outputs lock poisoned")
            .get(&registration.session_id)
            .is_some_and(|output| output.generation == registration.generation);
        if !output_is_current {
            return Ok(false);
        }

        let detached = {
            let mut mgr = self
                .session_mgr
                .lock()
                .expect("session manager lock poisoned");
            mgr.detach_session_if_output_generation(
                &registration.session_id,
                registration.generation,
            )
            .map_err(other_io_error)?
        };
        if detached {
            remove_session_output_if_current(
                &self.session_outputs,
                &registration.session_id,
                registration.generation,
            );
        }
        Ok(detached)
    }

    pub fn session_handle(self: &Arc<Self>, session_id: String) -> EmbeddedV8SessionHandle {
        let output_generation = self
            .session_outputs
            .lock()
            .expect("embedded runtime session outputs lock poisoned")
            .get(&session_id)
            .map(|output| output.generation);
        EmbeddedV8SessionHandle {
            session_id,
            output_generation,
            runtime: Arc::clone(self),
        }
    }

    pub fn dispatch(&self, command: RuntimeCommand) -> io::Result<()> {
        match command {
            RuntimeCommand::CreateSession {
                session_id,
                heap_limit_mb,
                cpu_time_limit_ms,
                wall_clock_limit_ms,
                warm_hint,
            } => {
                let output = self
                    .session_outputs
                    .lock()
                    .expect("embedded runtime session outputs lock poisoned")
                    .get(&session_id)
                    .cloned();
                let output_generation = output.as_ref().map(|output| output.generation);
                let event_tx = output.map(|output| {
                    crate::session::RuntimeEventSender::direct(output.generation, output.sender)
                });
                let mut mgr = self
                    .session_mgr
                    .lock()
                    .expect("session manager lock poisoned");
                mgr.create_session_with_output_generation_and_sender(
                    session_id,
                    heap_limit_mb,
                    cpu_time_limit_ms,
                    wall_clock_limit_ms,
                    output_generation,
                    warm_hint,
                    event_tx,
                )
                .map_err(other_io_error)
            }
            command => dispatch_runtime_command(&self.session_mgr, &self.snapshot_cache, command),
        }
    }

    fn settle_bridge_response(
        &self,
        session_id: &str,
        output_generation: Option<u64>,
        response: BridgeResponse,
    ) -> io::Result<()> {
        let registry = {
            let mgr = self
                .session_mgr
                .lock()
                .expect("session manager lock poisoned");
            Arc::clone(mgr.call_id_router())
        };
        let phase_start = Instant::now();
        let result = registry
            .settle(session_id, output_generation, response)
            .map_err(other_io_error);
        record_sync_bridge_host_phase(
            "sync_rpc_dispatch",
            "direct_response_settlement",
            phase_start.elapsed(),
        );
        result
    }

    /// Dispatch an in-process create-session command with the VM-scoped runtime
    /// that owns guest work. Serialized IPC commands cannot carry this trusted
    /// host capability and continue to use the manager's process context.
    pub fn dispatch_create_session_with_runtime(
        &self,
        command: RuntimeCommand,
        session_runtime: agentos_runtime::RuntimeContext,
        ready_batch_handle_limit: usize,
        bridge_call_timeout: std::time::Duration,
    ) -> io::Result<()> {
        let RuntimeCommand::CreateSession {
            session_id,
            heap_limit_mb,
            cpu_time_limit_ms,
            wall_clock_limit_ms,
            warm_hint,
        } = command
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "dispatch_create_session_with_runtime requires CreateSession",
            ));
        };

        let output = self
            .session_outputs
            .lock()
            .expect("embedded runtime session outputs lock poisoned")
            .get(&session_id)
            .cloned();
        let output_generation = output.as_ref().map(|output| output.generation);
        let event_tx = output.map(|output| {
            crate::session::RuntimeEventSender::direct(output.generation, output.sender)
        });
        self.session_mgr
            .lock()
            .expect("session manager lock poisoned")
            .create_session_with_output_generation_sender_and_runtime(
                session_id,
                heap_limit_mb,
                cpu_time_limit_ms,
                wall_clock_limit_ms,
                output_generation,
                warm_hint,
                event_tx,
                session_runtime,
                ready_batch_handle_limit,
                bridge_call_timeout,
            )
            .map_err(other_io_error)
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

impl Drop for EmbeddedV8Runtime {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let session_handles = self
            .session_mgr
            .lock()
            .map(|mut mgr| mgr.take_session_shutdown_handles())
            .unwrap_or_default();
        let deadline = Instant::now() + self.executor_teardown_timeout;
        let mut session_handles = session_handles;
        while !session_handles.is_empty() {
            let mut pending = Vec::with_capacity(session_handles.len());
            for handle in session_handles {
                if !handle.is_finished() {
                    pending.push(handle);
                    continue;
                }
                if handle.join().is_err() {
                    eprintln!(
                        "ERR_AGENTOS_VM_EXECUTOR_PANIC: executor panicked during runtime shutdown"
                    );
                }
            }
            session_handles = pending;
            if session_handles.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                eprintln!(
                    "FATAL_AGENTOS_VM_EXECUTOR_SHUTDOWN_TIMEOUT: {} executor(s) survived the {}ms process deadline; raise runtime.executor.teardownTimeoutMs",
                    session_handles.len(),
                    self.executor_teardown_timeout.as_millis()
                );
                // A live untrusted executor may not be detached while the
                // process continues. Process shutdown is the final containment
                // boundary when cooperative V8 termination fails.
                std::process::abort();
            }
            thread::sleep(Duration::from_millis(5));
        }
        if let Ok(mut outputs) = self.session_outputs.lock() {
            outputs.clear();
        }
        bridge::release_embedded_cbor_codec();
    }
}

pub struct EmbeddedV8SessionHandle {
    session_id: String,
    output_generation: Option<u64>,
    runtime: Arc<EmbeddedV8Runtime>,
}

impl EmbeddedV8SessionHandle {
    // In-process runtime protocol call; the arg list mirrors the Execute frame.
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &self,
        mode: u8,
        file_path: String,
        bridge_code: String,
        post_restore_script: String,
        userland_code: String,
        high_resolution_time: bool,
        user_code: String,
        wasm_module_bytes: Option<Arc<Vec<u8>>>,
    ) -> io::Result<()> {
        validate_execute_mode(mode)?;
        self.runtime.dispatch(RuntimeCommand::SendToSession {
            session_id: self.session_id.clone(),
            message: SessionMessage::Execute {
                mode,
                file_path,
                bridge_code,
                post_restore_script,
                userland_code,
                high_resolution_time,
                user_code,
                wasm_module_bytes,
            },
        })
    }

    pub fn send_bridge_response(
        &self,
        call_id: u64,
        status: u8,
        payload: Vec<u8>,
    ) -> io::Result<()> {
        validate_bridge_response_status(status)?;
        self.runtime.settle_bridge_response(
            &self.session_id,
            self.output_generation,
            BridgeResponse {
                call_id,
                status,
                payload,
                reservation: None,
            },
        )
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

    pub fn publish_readiness(
        &self,
        capability_id: u64,
        capability_generation: u64,
        flags: agentos_runtime::readiness::ReadyFlags,
    ) -> io::Result<()> {
        self.runtime.dispatch(RuntimeCommand::PublishReadiness {
            session_id: self.session_id.clone(),
            capability_id,
            capability_generation,
            flags,
        })
    }

    pub fn remove_readiness(
        &self,
        capability_id: u64,
        capability_generation: u64,
    ) -> io::Result<()> {
        self.runtime.dispatch(RuntimeCommand::RemoveReadiness {
            session_id: self.session_id.clone(),
            capability_id,
            capability_generation,
        })
    }

    pub fn set_application_read_interest(
        &self,
        capability_id: u64,
        capability_generation: u64,
        enabled: bool,
    ) -> io::Result<()> {
        self.runtime
            .session_mgr
            .lock()
            .expect("session manager lock poisoned")
            .set_application_read_interest(
                &self.session_id,
                capability_id,
                capability_generation,
                enabled,
            )
            .map_err(other_io_error)
    }

    pub fn publish_signal(&self, signal: i32) -> io::Result<()> {
        self.runtime
            .session_mgr
            .lock()
            .expect("session manager lock poisoned")
            .publish_signal(&self.session_id, signal)
            .map_err(other_io_error)
    }

    pub fn publish_timer(&self, timer_id: u64) -> io::Result<()> {
        self.runtime.dispatch(RuntimeCommand::PublishTimer {
            session_id: self.session_id.clone(),
            timer_id,
        })
    }

    /// Install a direct module-source reader on this session's thread so module
    /// loads read source directly instead of round-tripping the bridge. Routed
    /// through the dispatch thread (which owns the session manager).
    pub fn set_module_reader(
        &self,
        reader: Box<dyn crate::execution::GuestModuleReader>,
    ) -> io::Result<()> {
        self.runtime
            .dispatch(RuntimeCommand::SetSessionModuleReader {
                session_id: self.session_id.clone(),
                reader: ModuleReaderHandle::new(reader),
            })
    }

    pub fn terminate(&self) -> io::Result<()> {
        self.runtime.dispatch(RuntimeCommand::SendToSession {
            session_id: self.session_id.clone(),
            message: SessionMessage::TerminateExecution,
        })
    }

    pub fn pause(&self) -> io::Result<()> {
        self.runtime.dispatch(RuntimeCommand::PauseSession {
            session_id: self.session_id.clone(),
        })
    }

    pub fn resume(&self) -> io::Result<()> {
        self.runtime.dispatch(RuntimeCommand::ResumeSession {
            session_id: self.session_id.clone(),
        })
    }

    pub fn destroy(&self) -> io::Result<()> {
        // Keep the output lane registered while the session executor joins so
        // terminal results and lifecycle diagnostics can drain. Removing it
        // first closes the receiver underneath the executor. Generation-check
        // the final removal so a concurrently reused session ID keeps its lane.
        let result = self.runtime.dispatch(RuntimeCommand::DestroySession {
            session_id: self.session_id.clone(),
        });
        if let Some(generation) = self.output_generation {
            remove_session_output_if_current(
                &self.runtime.session_outputs,
                &self.session_id,
                generation,
            );
        } else {
            self.runtime.unregister_session(&self.session_id);
        }
        result
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

fn validate_execute_mode(mode: u8) -> io::Result<()> {
    if mode > 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown Execute mode: {mode}"),
        ));
    }
    Ok(())
}

impl Clone for EmbeddedV8SessionHandle {
    fn clone(&self) -> Self {
        Self {
            session_id: self.session_id.clone(),
            output_generation: self.output_generation,
            runtime: Arc::clone(&self.runtime),
        }
    }
}

pub fn shared_embedded_runtime(
    runtime: agentos_runtime::RuntimeContext,
) -> io::Result<Arc<EmbeddedV8Runtime>> {
    static SHARED_RUNTIME: OnceLock<Mutex<Weak<EmbeddedV8Runtime>>> = OnceLock::new();

    let shared_slot = SHARED_RUNTIME.get_or_init(|| Mutex::new(Weak::new()));
    let mut shared_guard = shared_slot
        .lock()
        .expect("shared embedded runtime init lock poisoned");
    if let Some(shared) = shared_guard.upgrade() {
        return Ok(shared);
    }

    let shared = Arc::new(EmbeddedV8Runtime::new(None, runtime)?);
    *shared_guard = Arc::downgrade(&shared);
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
    runtime: agentos_runtime::RuntimeContext,
) -> io::Result<(UnixStream, EmbeddedRuntimeHandle)> {
    bridge::init_codec();
    bridge::acquire_embedded_cbor_codec();
    isolate::init_v8_platform();

    let (host_stream, runtime_stream) = UnixStream::pair()?;
    let shutdown_stream = host_stream.try_clone()?;
    let alive = Arc::new(AtomicBool::new(true));
    let alive_for_thread = Arc::clone(&alive);
    let max_concurrency = max_concurrency.unwrap_or_else(|| runtime.max_active_vm_executors());

    // AGENTOS_THREAD_SITE: embedded-v8-dispatch
    let join_handle = thread::Builder::new()
        .name(String::from("agentos-v8-runtime"))
        .spawn(move || {
            run_embedded_runtime(runtime_stream, max_concurrency, runtime);
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

fn run_embedded_runtime(
    stream: UnixStream,
    max_concurrency: usize,
    runtime: agentos_runtime::RuntimeContext,
) {
    // Keep bridge-only, agent-SDK, and wasm-runner userland variants warm
    // without immediately evicting each other.
    let snapshot_cache = Arc::new(SnapshotCache::new(8));
    let writer_stream = match stream.try_clone() {
        Ok(writer_stream) => writer_stream,
        Err(error) => {
            eprintln!("embedded V8 runtime failed to clone stream: {error}");
            return;
        }
    };
    let output_capacity = match crate::session::configured_resource_capacity(
        &runtime,
        ResourceClass::AsyncCompletions,
        "limits.reactor.maxAsyncCompletions",
        "runtime.resources.maxAsyncCompletions",
    ) {
        Ok(capacity) => capacity,
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };
    let (event_tx, event_rx) = crossbeam_channel::bounded::<RuntimeEventEnvelope>(output_capacity);
    let call_id_router: CallIdRouter = Arc::new(BridgeCallRegistry::with_default_limit());
    let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);

    // AGENTOS_THREAD_SITE: embedded-v8-writer
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
        runtime,
    )));

    handle_connection(stream, connection_id, session_mgr, snapshot_cache);
    let _ = writer_handle.join();
}

fn ipc_writer_thread(
    rx: crossbeam_channel::Receiver<RuntimeEventEnvelope>,
    mut writer: UnixStream,
) {
    while let Ok(envelope) = rx.recv() {
        let frame: BinaryFrame = envelope.event.into();
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

    {
        let mut mgr = session_mgr.lock().expect("session manager lock poisoned");
        for session_id in session_ids {
            if let Err(error) = mgr.detach_session(&session_id) {
                eprintln!(
                    "ERR_AGENTOS_VM_EXECUTOR_QUARANTINE: failed to detach session {session_id}: {error}"
                );
            }
        }
    }
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
            wall_clock_limit_ms,
            warm_hint,
        } => {
            let mut mgr = session_mgr.lock().expect("session manager lock poisoned");
            mgr.create_session_with_output_generation(
                session_id,
                heap_limit_mb,
                cpu_time_limit_ms,
                wall_clock_limit_ms,
                None,
                warm_hint,
            )
            .map_err(other_io_error)
        }
        RuntimeCommand::DestroySession { session_id } => {
            // Explicit destruction is a quiescence boundary. Remove the entry
            // while holding the manager lock, then join after releasing it so
            // the executor cannot leak into a successor session and the event
            // dispatcher remains free to drain terminal output.
            let shutdown = session_mgr
                .lock()
                .expect("session manager lock poisoned")
                .begin_destroy_session(&session_id)
                .map_err(other_io_error)?;
            shutdown.finish();
            Ok(())
        }
        RuntimeCommand::PauseSession { session_id } => {
            let mgr = session_mgr.lock().expect("session manager lock poisoned");
            mgr.pause_session(&session_id).map_err(other_io_error)
        }
        RuntimeCommand::ResumeSession { session_id } => {
            let mgr = session_mgr.lock().expect("session manager lock poisoned");
            mgr.resume_session(&session_id).map_err(other_io_error)
        }
        RuntimeCommand::SendToSession {
            session_id,
            message,
        } => {
            let message = match message {
                SessionMessage::BridgeResponse(response) => {
                    let (registry, output_generation) = {
                        let mgr = session_mgr.lock().expect("session manager lock poisoned");
                        (
                            Arc::clone(mgr.call_id_router()),
                            mgr.session_output_generation(&session_id),
                        )
                    };
                    let phase_start = Instant::now();
                    let result = registry
                        .settle(&session_id, output_generation, response)
                        .map(|_| ())
                        .map_err(other_io_error);
                    record_sync_bridge_host_phase(
                        "sync_rpc_dispatch",
                        "direct_response_settlement",
                        phase_start.elapsed(),
                    );
                    return result;
                }
                message => message,
            };

            {
                let mgr = session_mgr.lock().expect("session manager lock poisoned");
                let phase_start = Instant::now();
                let result = mgr
                    .try_send_to_session(&session_id, message)
                    .map_err(other_io_error);
                record_sync_bridge_host_phase(
                    "session_dispatch",
                    "nonblocking_command_admission",
                    phase_start.elapsed(),
                );
                result
            }
        }
        RuntimeCommand::PublishReadiness {
            session_id,
            capability_id,
            capability_generation,
            flags,
        } => session_mgr
            .lock()
            .expect("session manager lock poisoned")
            .publish_readiness(&session_id, capability_id, capability_generation, flags)
            .map_err(other_io_error),
        RuntimeCommand::RemoveReadiness {
            session_id,
            capability_id,
            capability_generation,
        } => session_mgr
            .lock()
            .expect("session manager lock poisoned")
            .remove_readiness(&session_id, capability_id, capability_generation)
            .map_err(other_io_error),
        RuntimeCommand::PublishSignal { session_id, signal } => session_mgr
            .lock()
            .expect("session manager lock poisoned")
            .publish_signal(&session_id, signal)
            .map_err(other_io_error),
        RuntimeCommand::PublishTimer {
            session_id,
            timer_id,
        } => session_mgr
            .lock()
            .expect("session manager lock poisoned")
            .publish_timer(&session_id, timer_id)
            .map_err(other_io_error),
        RuntimeCommand::SetSessionModuleReader { session_id, reader } => {
            // Resolve the sender under the lock, release, then forward the live
            // reader as a SetModuleReader command to the session thread.
            let (sender, command_capacity) = {
                let mgr = session_mgr.lock().expect("session manager lock poisoned");
                mgr.session_sender(&session_id)
            }
            .map_err(other_io_error)?;
            match reader.take() {
                Some(reader) => sender
                    .try_send(SessionCommand::SetModuleReader(reader))
                    .map_err(|error| match error {
                        crossbeam_channel::TrySendError::Full(_) => other_io_error(format!(
                            "ERR_AGENTOS_SESSION_COMMAND_LIMIT: session {session_id} command queue exceeded limit of {command_capacity} while admitting module_reader (queued={}); raise limits.reactor.maxHandleCommands",
                            sender.len()
                        )),
                        crossbeam_channel::TrySendError::Disconnected(_) => other_io_error(
                            format!("session thread disconnected for session {session_id}"),
                        ),
                    }),
                None => Ok(()),
            }
        }
        RuntimeCommand::WarmSnapshot {
            bridge_code,
            userland_code,
        } => snapshot_cache
            .get_or_create_with_userland(
                &bridge_code,
                (!userland_code.is_empty()).then_some(userland_code.as_str()),
            )
            .map(|_| ())
            .map_err(other_io_error),
    }
}

#[cfg(test)]
fn route_outbound_event(
    envelope: RuntimeEventEnvelope,
    session_outputs: &Arc<Mutex<HashMap<String, SessionOutput>>>,
    session_mgr: &Arc<Mutex<SessionManager>>,
) -> bool {
    let RuntimeEventEnvelope {
        output_generation,
        event,
    } = envelope;
    let session_id = event.session_id().to_owned();

    let output = session_outputs
        .lock()
        .expect("embedded runtime session outputs lock poisoned")
        .get(&session_id)
        .cloned();

    let Some(output) = output else {
        clear_dropped_bridge_call_route(&event, session_mgr);
        return false;
    };

    if output_generation != Some(output.generation) {
        clear_dropped_bridge_call_route(&event, session_mgr);
        return false;
    }

    match output.sender.try_send(event) {
        Ok(()) => {}
        Err(_) => {
            if remove_session_output_if_current(session_outputs, &session_id, output.generation) {
                return session_mgr
                    .lock()
                    .expect("session manager lock poisoned")
                    .detach_session_if_output_generation(&session_id, output.generation)
                    .unwrap_or(false);
            }
        }
    }
    false
}

#[cfg(test)]
fn clear_dropped_bridge_call_route(event: &RuntimeEvent, session_mgr: &Arc<Mutex<SessionManager>>) {
    if let RuntimeEvent::BridgeCall { call_id, .. } = event {
        session_mgr
            .lock()
            .expect("session manager lock poisoned")
            .clear_call_route(*call_id);
    }
}

fn remove_session_output_if_current(
    session_outputs: &Arc<Mutex<HashMap<String, SessionOutput>>>,
    session_id: &str,
    generation: u64,
) -> bool {
    let mut outputs = session_outputs
        .lock()
        .expect("embedded runtime session outputs lock poisoned");
    if outputs
        .get(session_id)
        .is_some_and(|output| output.generation == generation)
    {
        outputs.remove(session_id);
        return true;
    }
    false
}

fn other_io_error(message: String) -> io::Error {
    io::Error::other(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_protocol::{BridgeResponse, RuntimeCommand, RuntimeEvent, SessionMessage};
    use std::process::Command;
    use std::time::Duration;

    static EMBEDDED_RUNTIME_CODEC_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn run_isolated_unit_test(env_name: &str, test_name: &str) -> bool {
        if std::env::var_os(env_name).is_some() {
            return true;
        }
        let output = Command::new(std::env::current_exe().expect("current test binary"))
            .arg(test_name)
            .arg("--exact")
            .arg("--nocapture")
            .env(env_name, "1")
            .output()
            .unwrap_or_else(|error| panic!("spawn isolated test {test_name}: {error}"));
        assert!(
            output.status.success(),
            "isolated test {test_name} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        false
    }

    fn test_runtime_context() -> agentos_runtime::RuntimeContext {
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .expect("test process runtime")
            .context()
    }

    fn test_output_channel(
        capacity: usize,
    ) -> (RuntimeEventOutputSender, RuntimeEventOutputReceiver) {
        let runtime = test_runtime_context();
        runtime_event_output_channel(capacity, Arc::clone(runtime.resources()))
    }

    fn output_event(message: &str) -> RuntimeEvent {
        RuntimeEvent::Log {
            session_id: String::from("aggregate-test"),
            channel: 0,
            message: message.to_owned(),
        }
    }

    #[test]
    fn session_outputs_share_one_vm_completion_limit() {
        use agentos_runtime::accounting::{ResourceLedger, ResourceLimit};

        let resources = Arc::new(ResourceLedger::root(
            "v8-output-test-vm",
            [(
                ResourceClass::AsyncCompletions,
                ResourceLimit::new(2, "limits.reactor.maxAsyncCompletions"),
            )],
        ));
        let (first_tx, first_rx) = runtime_event_output_channel(2, Arc::clone(&resources));
        let (second_tx, second_rx) = runtime_event_output_channel(2, Arc::clone(&resources));

        first_tx
            .try_send(output_event("first"))
            .expect("first session output admission");
        second_tx
            .try_send(output_event("second"))
            .expect("second session output admission");
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 2);

        let error = first_tx
            .try_send(output_event("overflow"))
            .expect_err("aggregate VM limit must span both session lanes");
        assert!(error.contains("limits.reactor.maxAsyncCompletions"));

        first_rx.try_recv().expect("release one output reservation");
        second_tx
            .try_send(output_event("replacement"))
            .expect("released slot can be used by another session lane");
        drop(first_rx);
        drop(second_rx);
        assert_eq!(
            resources.usage(ResourceClass::AsyncCompletions).used,
            0,
            "session output teardown must release queued completion reservations"
        );

        let (disconnected_tx, disconnected_rx) =
            runtime_event_output_channel(1, Arc::clone(&resources));
        drop(disconnected_rx);
        disconnected_tx
            .try_send(output_event("disconnected"))
            .expect_err("disconnected output must reject insertion");
        assert_eq!(
            resources.usage(ResourceClass::AsyncCompletions).used,
            0,
            "failed insertion must release its reservation"
        );
    }

    #[test]
    fn embedded_runtime_uses_configured_executor_and_output_bounds() {
        if !run_isolated_unit_test(
            "AGENTOS_V8_CONFIGURED_EMBEDDED_RUNTIME_SUBPROCESS",
            "embedded_runtime::tests::embedded_runtime_uses_configured_executor_and_output_bounds",
        ) {
            return;
        }
        let _codec_guard = EMBEDDED_RUNTIME_CODEC_TEST_LOCK
            .lock()
            .expect("embedded runtime codec test lock poisoned");
        let mut config = agentos_runtime::RuntimeConfig {
            max_active_vm_executors: 2,
            vm_executor_teardown_timeout_ms: 31,
            ..agentos_runtime::RuntimeConfig::default()
        };
        config.resources.max_async_completions = 3;
        let runtime_context = agentos_runtime::SidecarRuntime::process(&config)
            .expect("configured process runtime")
            .context();
        let runtime = EmbeddedV8Runtime::new(None, runtime_context.clone())
            .expect("configured embedded runtime");

        assert_eq!(
            runtime
                .session_mgr
                .lock()
                .expect("session manager")
                .max_concurrency(),
            2
        );
        assert_eq!(runtime.executor_teardown_timeout, Duration::from_millis(31));
        let (_receiver, registration) = runtime
            .register_session_with_runtime("configured-output", &runtime_context)
            .expect("register configured output lane");
        let outputs = runtime.session_outputs.lock().expect("session outputs");
        assert_eq!(outputs[&registration.session_id].sender.capacity(), Some(3));
    }

    #[test]
    fn embedded_runtime_handle_reports_liveness_and_shutdown() {
        let _codec_guard = EMBEDDED_RUNTIME_CODEC_TEST_LOCK
            .lock()
            .expect("embedded runtime codec test lock poisoned");
        let (_stream, handle) = spawn_embedded_runtime_ipc(Some(1), test_runtime_context())
            .expect("spawn embedded runtime");
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
        let _codec_guard = EMBEDDED_RUNTIME_CODEC_TEST_LOCK
            .lock()
            .expect("embedded runtime codec test lock poisoned");
        let first =
            shared_embedded_runtime(test_runtime_context()).expect("shared embedded runtime");
        let second =
            shared_embedded_runtime(test_runtime_context()).expect("shared embedded runtime");
        assert!(
            Arc::ptr_eq(&first, &second),
            "shared_embedded_runtime() should reuse the same runtime instance"
        );
    }

    #[test]
    fn in_process_session_creation_preserves_vm_resource_scope() {
        let _codec_guard = EMBEDDED_RUNTIME_CODEC_TEST_LOCK
            .lock()
            .expect("embedded runtime codec test lock poisoned");
        let process = test_runtime_context();
        let vm_resources = Arc::new(agentos_runtime::accounting::ResourceLedger::child(
            "embedded-runtime-vm",
            [
                (
                    agentos_runtime::accounting::ResourceClass::BridgeCalls,
                    agentos_runtime::accounting::ResourceLimit::new(
                        1,
                        "limits.reactor.maxBridgeCalls",
                    ),
                ),
                (
                    agentos_runtime::accounting::ResourceClass::HandleCommands,
                    agentos_runtime::accounting::ResourceLimit::new(
                        2,
                        "limits.reactor.maxHandleCommands",
                    ),
                ),
                (
                    agentos_runtime::accounting::ResourceClass::ReadyHandles,
                    agentos_runtime::accounting::ResourceLimit::new(
                        2,
                        "limits.reactor.maxReadyHandles",
                    ),
                ),
                (
                    agentos_runtime::accounting::ResourceClass::Timers,
                    agentos_runtime::accounting::ResourceLimit::new(
                        2,
                        "limits.jsRuntime.maxTimers",
                    ),
                ),
            ],
            Arc::clone(process.resources()),
        ));
        let vm_runtime = process.scoped_for_vm(Arc::clone(&vm_resources), 42);
        let runtime = EmbeddedV8Runtime::new(Some(1), process).expect("embedded runtime");

        runtime
            .dispatch_create_session_with_runtime(
                RuntimeCommand::CreateSession {
                    session_id: "vm-scoped-session".into(),
                    heap_limit_mb: None,
                    cpu_time_limit_ms: None,
                    wall_clock_limit_ms: None,
                    warm_hint: None,
                },
                vm_runtime,
                64,
                std::time::Duration::from_secs(30),
            )
            .expect("create VM-scoped session");

        let actual = runtime
            .session_mgr
            .lock()
            .expect("session manager lock poisoned")
            .session_resources("vm-scoped-session")
            .expect("session resources");
        assert!(
            Arc::ptr_eq(&actual, &vm_resources),
            "session work must retain the caller's VM ledger"
        );
    }

    #[test]
    fn embedded_runtime_drop_releases_codec_after_destroying_sessions() {
        let _codec_guard = EMBEDDED_RUNTIME_CODEC_TEST_LOCK
            .lock()
            .expect("embedded runtime codec test lock poisoned");
        let codec_before = bridge::is_cbor_codec();
        let alive = {
            let runtime =
                EmbeddedV8Runtime::new(Some(1), test_runtime_context()).expect("embedded runtime");
            let alive = Arc::clone(&runtime.alive);
            assert!(
                bridge::is_cbor_codec(),
                "embedded runtime should enable the CBOR bridge codec while alive"
            );
            let (_receiver, _registration) = runtime
                .register_session_with_output_registration("drop-lifecycle")
                .expect("register session output");
            runtime
                .dispatch(RuntimeCommand::CreateSession {
                    session_id: "drop-lifecycle".into(),
                    heap_limit_mb: None,
                    cpu_time_limit_ms: None,
                    wall_clock_limit_ms: None,
                    warm_hint: None,
                })
                .expect("create session");
            assert_eq!(
                runtime.session_count(),
                1,
                "test should drop a runtime with a live session"
            );
            alive
        };

        assert!(
            !alive.load(Ordering::Acquire),
            "dropping embedded runtime should stop the dispatch thread"
        );
        assert_eq!(
            bridge::is_cbor_codec(),
            codec_before,
            "dropping embedded runtime should restore the prior codec state"
        );
    }

    #[test]
    fn embedded_runtime_bridge_response_requires_matching_session_generation() {
        let snapshot_cache = Arc::new(SnapshotCache::new(1));
        let (event_tx, _event_rx) = crossbeam_channel::unbounded::<RuntimeEventEnvelope>();
        let call_id_router: CallIdRouter = Arc::new(BridgeCallRegistry::with_default_limit());
        let runtime = test_runtime_context();
        let session_mgr = Arc::new(Mutex::new(SessionManager::new(
            1,
            event_tx,
            Arc::clone(&call_id_router),
            Arc::clone(&snapshot_cache),
            runtime.clone(),
        )));

        {
            let mut mgr = session_mgr.lock().expect("session manager");
            mgr.create_session("stream-target".into(), None, None, None)
                .expect("create target session");
        }
        let waiter = call_id_router
            .register_sync(&runtime, 0, 1, 41, "stream-target", None)
            .expect("register bridge call target");

        let error = dispatch_runtime_command(
            &session_mgr,
            &snapshot_cache,
            RuntimeCommand::SendToSession {
                session_id: "wrong-session".into(),
                message: SessionMessage::BridgeResponse(BridgeResponse {
                    call_id: 41,
                    status: 0,
                    payload: vec![0xAB],
                    reservation: None,
                }),
            },
        )
        .expect_err("wrong-session bridge response must be rejected");
        assert!(
            error
                .to_string()
                .contains("ERR_AGENTOS_BRIDGE_STALE_GENERATION"),
            "wrong-session rejection should be typed: {error}"
        );
        assert_eq!(call_id_router.pending_len(), 1);

        dispatch_runtime_command(
            &session_mgr,
            &snapshot_cache,
            RuntimeCommand::SendToSession {
                session_id: "stream-target".into(),
                message: SessionMessage::BridgeResponse(BridgeResponse {
                    call_id: 41,
                    status: 0,
                    payload: vec![0xAB],
                    reservation: None,
                }),
            },
        )
        .expect("matching bridge response should settle directly");
        assert_eq!(
            waiter.recv().expect("settled bridge response").payload,
            vec![0xAB]
        );
        assert_eq!(call_id_router.pending_len(), 0);

        session_mgr
            .lock()
            .expect("session manager")
            .destroy_session("stream-target")
            .expect("destroy target session");
    }

    #[test]
    fn embedded_runtime_session_handle_rejects_unknown_bridge_response_status() {
        let _codec_guard = EMBEDDED_RUNTIME_CODEC_TEST_LOCK
            .lock()
            .expect("embedded runtime codec test lock poisoned");
        let runtime = Arc::new(
            EmbeddedV8Runtime::new(Some(1), test_runtime_context()).expect("embedded runtime"),
        );
        let handle = runtime.session_handle("missing-session".into());

        let err = handle
            .send_bridge_response(1, 3, Vec::new())
            .expect_err("unknown bridge response status should be rejected");

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("unknown BridgeResponse status"));
    }

    #[test]
    fn embedded_runtime_stream_events_preserve_order_per_session() {
        let (sender, receiver) = test_output_channel(TEST_SESSION_OUTPUT_CHANNEL_CAPACITY);
        let session_outputs = Arc::new(Mutex::new(HashMap::from([(
            String::from("stream-order"),
            SessionOutput {
                generation: 1,
                sender,
            },
        )])));
        let session_mgr = test_session_manager();

        route_outbound_event(
            runtime_envelope(
                1,
                RuntimeEvent::Log {
                    session_id: "stream-order".into(),
                    channel: 0,
                    message: "first".into(),
                },
            ),
            &session_outputs,
            &session_mgr,
        );
        route_outbound_event(
            runtime_envelope(
                1,
                RuntimeEvent::StreamCallback {
                    session_id: "stream-order".into(),
                    callback_type: "stdin".into(),
                    payload: vec![1, 2, 3],
                },
            ),
            &session_outputs,
            &session_mgr,
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
        let (sender, receiver) = test_output_channel(TEST_SESSION_OUTPUT_CHANNEL_CAPACITY);
        let session_outputs = Arc::new(Mutex::new(HashMap::from([(
            String::from("stream-race"),
            SessionOutput {
                generation: 1,
                sender,
            },
        )])));
        let session_mgr = test_session_manager();
        drop(receiver);

        route_outbound_event(
            runtime_envelope(
                1,
                RuntimeEvent::ExecutionResult {
                    session_id: "stream-race".into(),
                    exit_code: 0,
                    exports: None,
                    error: None,
                },
            ),
            &session_outputs,
            &session_mgr,
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

    #[test]
    fn embedded_runtime_stream_backpressure_drops_full_session_output() {
        let (sender, receiver) = test_output_channel(1);
        let session_outputs = Arc::new(Mutex::new(HashMap::from([(
            String::from("stream-full"),
            SessionOutput {
                generation: 1,
                sender,
            },
        )])));
        let session_mgr = test_session_manager_with_session("stream-full");

        route_outbound_event(
            runtime_envelope(
                1,
                RuntimeEvent::Log {
                    session_id: "stream-full".into(),
                    channel: 0,
                    message: "first".into(),
                },
            ),
            &session_outputs,
            &session_mgr,
        );
        let cleaned_up = route_outbound_event(
            runtime_envelope(
                1,
                RuntimeEvent::Log {
                    session_id: "stream-full".into(),
                    channel: 0,
                    message: "second".into(),
                },
            ),
            &session_outputs,
            &session_mgr,
        );
        assert!(cleaned_up, "full session output should detach the session");

        let first = receiver
            .recv_timeout(Duration::from_millis(100))
            .expect("first event");
        assert!(matches!(
            first,
            RuntimeEvent::Log { ref message, .. } if message == "first"
        ));
        assert!(
            receiver.recv_timeout(Duration::from_millis(20)).is_err(),
            "full session output should drop the overflowing event"
        );
        assert!(
            session_outputs
                .lock()
                .expect("session outputs")
                .get("stream-full")
                .is_none(),
            "full session output should remove the stale registration"
        );
        assert_eq!(
            session_mgr.lock().expect("session manager").session_count(),
            0,
            "full session output should destroy the runtime session"
        );
    }

    #[test]
    fn embedded_runtime_drops_stale_generation_events_for_reused_session_id() {
        let (sender, receiver) = test_output_channel(TEST_SESSION_OUTPUT_CHANNEL_CAPACITY);
        let session_outputs = Arc::new(Mutex::new(HashMap::from([(
            String::from("stream-reused"),
            SessionOutput {
                generation: 2,
                sender,
            },
        )])));
        let session_mgr = test_session_manager_with_generation("stream-reused", 2);
        let runtime = test_runtime_context();
        let _waiter = session_mgr
            .lock()
            .expect("session manager")
            .call_id_router()
            .register_sync(&runtime, 0, 1, 99, "stream-reused", Some(1))
            .expect("register stale bridge call target");

        let routed = route_outbound_event(
            runtime_envelope(
                1,
                RuntimeEvent::BridgeCall {
                    session_id: "stream-reused".into(),
                    call_id: 99,
                    method: "_stale".into(),
                    payload: Vec::new(),
                },
            ),
            &session_outputs,
            &session_mgr,
        );

        assert!(!routed, "stale generation event should not trigger cleanup");
        assert!(
            receiver.recv_timeout(Duration::from_millis(20)).is_err(),
            "stale generation event should not reach reused session output"
        );
        assert_eq!(
            session_mgr.lock().expect("session manager").session_count(),
            1,
            "stale generation event must leave reused session alive"
        );
        assert!(
            session_mgr
                .lock()
                .expect("session manager")
                .call_id_router()
                .pending_len()
                == 0,
            "stale bridge calls should clear their call route"
        );
    }

    #[test]
    fn embedded_runtime_clears_bridge_route_when_output_is_missing() {
        let session_outputs = Arc::new(Mutex::new(HashMap::new()));
        let session_mgr = test_session_manager();
        let runtime = test_runtime_context();
        let _waiter = session_mgr
            .lock()
            .expect("session manager")
            .call_id_router()
            .register_sync(&runtime, 0, 1, 123, "stream-detached", None)
            .expect("register detached bridge call target");

        let routed = route_outbound_event(
            runtime_envelope(
                1,
                RuntimeEvent::BridgeCall {
                    session_id: "stream-detached".into(),
                    call_id: 123,
                    method: "_detached".into(),
                    payload: Vec::new(),
                },
            ),
            &session_outputs,
            &session_mgr,
        );

        assert!(!routed, "missing output should not route the bridge call");
        assert!(
            session_mgr
                .lock()
                .expect("session manager")
                .call_id_router()
                .pending_len()
                == 0,
            "bridge calls dropped with no output should clear their call route"
        );
    }

    #[test]
    fn embedded_runtime_stale_output_registration_cannot_destroy_reused_session_id() {
        let _codec_guard = EMBEDDED_RUNTIME_CODEC_TEST_LOCK
            .lock()
            .expect("embedded runtime codec test lock poisoned");
        let runtime = Arc::new(
            EmbeddedV8Runtime::new(Some(1), test_runtime_context()).expect("embedded runtime"),
        );
        let session_id = "stream-generation-reuse";
        let (_first_receiver, first_registration) = runtime
            .register_session_with_capacity(session_id, 1, Arc::clone(runtime.runtime.resources()))
            .expect("register first session output");
        runtime
            .dispatch(RuntimeCommand::CreateSession {
                session_id: session_id.into(),
                heap_limit_mb: None,
                cpu_time_limit_ms: None,
                wall_clock_limit_ms: None,
                warm_hint: None,
            })
            .expect("create first session");
        runtime
            .session_handle(session_id.into())
            .destroy()
            .expect("destroy first session");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let reconciled = {
                let mut manager = runtime
                    .session_mgr
                    .lock()
                    .expect("session manager lock poisoned");
                manager.quarantined_session_count() == 0 && manager.active_slot_count() == 0
            };
            if reconciled {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "destroyed generation did not release its quarantined executor permit"
            );
            thread::yield_now();
        }

        let (_second_receiver, _second_registration) = runtime
            .register_session_with_capacity(session_id, 1, Arc::clone(runtime.runtime.resources()))
            .expect("register reused session output");
        runtime
            .dispatch(RuntimeCommand::CreateSession {
                session_id: session_id.into(),
                heap_limit_mb: None,
                cpu_time_limit_ms: None,
                wall_clock_limit_ms: None,
                warm_hint: None,
            })
            .expect("create reused session");

        assert!(
            !runtime
                .destroy_session_if_output_current(&first_registration)
                .expect("stale destroy should be ignored"),
            "stale registration should not match the reused session output"
        );
        assert_eq!(
            runtime.session_count(),
            1,
            "stale registration must not destroy the reused session"
        );

        runtime
            .session_handle(session_id.into())
            .destroy()
            .expect("destroy reused session");
    }

    #[test]
    fn session_cleanup_generation_guard_does_not_destroy_reused_session_id() {
        let session_mgr = test_session_manager();
        {
            let mut mgr = session_mgr.lock().expect("session manager");
            mgr.create_session_with_output_generation(
                "reused".into(),
                None,
                None,
                None,
                Some(1),
                None,
            )
            .expect("create first session");
            mgr.destroy_session("reused")
                .expect("destroy first session");
            mgr.create_session_with_output_generation(
                "reused".into(),
                None,
                None,
                None,
                Some(2),
                None,
            )
            .expect("create reused session");

            assert!(
                !mgr.destroy_session_if_output_generation("reused", 1)
                    .expect("stale generation destroy should be ignored"),
                "stale cleanup generation should not match reused session"
            );
            assert_eq!(
                mgr.session_count(),
                1,
                "stale cleanup generation must leave reused session alive"
            );
            mgr.destroy_session("reused")
                .expect("destroy reused session");
        }
    }

    fn test_session_manager() -> Arc<Mutex<SessionManager>> {
        let (event_tx, _event_rx) = crossbeam_channel::bounded::<RuntimeEventEnvelope>(1);
        Arc::new(Mutex::new(SessionManager::new(
            1,
            event_tx,
            Arc::new(BridgeCallRegistry::with_default_limit()),
            Arc::new(SnapshotCache::new(1)),
            test_runtime_context(),
        )))
    }

    fn runtime_envelope(output_generation: u64, event: RuntimeEvent) -> RuntimeEventEnvelope {
        RuntimeEventEnvelope {
            output_generation: Some(output_generation),
            event,
        }
    }

    fn test_session_manager_with_session(session_id: &str) -> Arc<Mutex<SessionManager>> {
        test_session_manager_with_generation(session_id, 1)
    }

    fn test_session_manager_with_generation(
        session_id: &str,
        output_generation: u64,
    ) -> Arc<Mutex<SessionManager>> {
        let session_mgr = test_session_manager();
        session_mgr
            .lock()
            .expect("session manager")
            .create_session_with_output_generation(
                session_id.into(),
                None,
                None,
                None,
                Some(output_generation),
                None,
            )
            .expect("create test session");
        session_mgr
    }
}
