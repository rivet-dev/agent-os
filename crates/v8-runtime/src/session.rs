// Session management: create/destroy sessions with V8 isolates on dedicated threads

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crossbeam_channel::{Receiver, Sender};

use crate::execution;
#[cfg(not(test))]
use crate::host_call::{BridgeCallContext, ChannelRuntimeEventSender};
use crate::host_call::{CallIdRouter, SharedCallIdCounter};
use crate::ipc::ExecutionError;
#[cfg(not(test))]
use crate::ipc_binary::ExecutionErrorBin;
use crate::runtime_protocol::{BridgeResponse, RuntimeEvent, SessionMessage, StreamEvent};
use crate::snapshot::SnapshotCache;
#[cfg(not(test))]
use crate::{bridge, isolate, snapshot};

/// Commands sent to a session thread
pub enum SessionCommand {
    /// Shut down the session and destroy the isolate
    Shutdown,
    /// Forward a typed session message to the session thread for processing
    Message(SessionMessage),
}

#[cfg(not(test))]
type SharedIsolateHandle = Arc<Mutex<Option<v8::IsolateHandle>>>;
#[cfg(test)]
type SharedIsolateHandle = Arc<Mutex<Option<()>>>;

/// Sender for typed runtime events produced by session threads.
pub type RuntimeEventSender = crossbeam_channel::Sender<RuntimeEvent>;

const LATE_TERMINATE_EXECUTION_ERROR_CODE: &str = "ERR_LATE_TERMINATE_EXECUTION";
const LATE_STREAM_EVENT_ERROR_CODE: &str = "ERR_LATE_STREAM_EVENT";
const LATE_BRIDGE_RESPONSE_ERROR_CODE: &str = "ERR_LATE_BRIDGE_RESPONSE";

/// Internal entry for a running session
struct SessionEntry {
    /// Channel to send commands to the session thread
    tx: Sender<SessionCommand>,
    /// Thread join handle
    join_handle: Option<thread::JoinHandle<()>>,
    /// Thread-safe V8 isolate handle for out-of-band termination.
    #[cfg_attr(test, allow(dead_code))]
    isolate_handle: SharedIsolateHandle,
    /// Current execution abort handle used to wake sync bridge waits.
    execution_abort: SharedExecutionAbort,
}

/// Concurrency slot tracker shared across session threads
type SlotControl = Arc<(Mutex<usize>, Condvar)>;

/// Shared deferred message queue for non-BridgeResponse frames consumed by
/// sync bridge calls. The event loop drains these before blocking on the channel.
pub(crate) type DeferredQueue = Arc<Mutex<VecDeque<SessionMessage>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionAbortReason {
    Terminated,
    TimedOut,
}

struct ExecutionAbortState {
    sender: Option<crossbeam_channel::Sender<()>>,
    reason: Option<ExecutionAbortReason>,
}

pub(crate) struct SharedExecutionAbort(Arc<Mutex<Option<ExecutionAbortState>>>);

impl Clone for SharedExecutionAbort {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

/// Create a new empty deferred queue.
pub(crate) fn new_deferred_queue() -> DeferredQueue {
    Arc::new(Mutex::new(VecDeque::new()))
}

pub(crate) fn new_execution_abort() -> SharedExecutionAbort {
    SharedExecutionAbort(Arc::new(Mutex::new(None)))
}

pub(crate) struct ActiveExecutionAbort {
    shared: SharedExecutionAbort,
}

impl ActiveExecutionAbort {
    pub(crate) fn arm(shared: &SharedExecutionAbort) -> (Self, crossbeam_channel::Receiver<()>) {
        let (tx, rx) = crossbeam_channel::bounded::<()>(0);
        let mut guard = shared.0.lock().unwrap();
        *guard = Some(ExecutionAbortState {
            sender: Some(tx),
            reason: None,
        });
        (
            Self {
                shared: shared.clone(),
            },
            rx,
        )
    }
}

impl Drop for ActiveExecutionAbort {
    fn drop(&mut self) {
        *self.shared.0.lock().unwrap() = None;
    }
}

pub(crate) fn signal_execution_abort(shared: &SharedExecutionAbort, reason: ExecutionAbortReason) {
    if let Some(state) = shared.0.lock().unwrap().as_mut() {
        state.reason.get_or_insert(reason);
        state.sender.take();
    }
}

#[cfg(not(test))]
fn execution_abort_reason(shared: &SharedExecutionAbort) -> Option<ExecutionAbortReason> {
    shared
        .0
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|state| state.reason)
}

/// Manages V8 sessions with concurrency limiting.
/// Each session runs on a dedicated OS thread with its own V8 isolate.
pub struct SessionManager {
    sessions: HashMap<String, SessionEntry>,
    max_concurrency: usize,
    slot_control: SlotControl,
    /// Typed runtime event sender shared across session threads.
    event_tx: RuntimeEventSender,
    /// Call_id → session_id routing table for BridgeResponse dispatch
    call_id_router: CallIdRouter,
    /// Shared call_id counter — all sessions use this to generate globally unique
    /// call_ids, preventing collisions in the call_id_router
    shared_call_id: SharedCallIdCounter,
    /// Shared snapshot cache for fast isolate creation from pre-compiled bridge code
    snapshot_cache: Arc<SnapshotCache>,
}

impl SessionManager {
    pub fn new(
        max_concurrency: usize,
        event_tx: RuntimeEventSender,
        call_id_router: CallIdRouter,
        snapshot_cache: Arc<SnapshotCache>,
    ) -> Self {
        SessionManager {
            sessions: HashMap::new(),
            max_concurrency,
            slot_control: Arc::new((Mutex::new(0), Condvar::new())),
            event_tx,
            call_id_router,
            shared_call_id: Arc::new(AtomicU64::new(1)),
            snapshot_cache,
        }
    }

    /// Get the snapshot cache for pre-warming from WarmSnapshot messages.
    #[allow(dead_code)]
    pub fn snapshot_cache(&self) -> &Arc<SnapshotCache> {
        &self.snapshot_cache
    }

    /// Create a new session.
    /// Spawns a dedicated thread with a V8 isolate. If max concurrency is
    /// reached, the session thread will block until a slot becomes available.
    pub fn create_session(
        &mut self,
        session_id: String,
        heap_limit_mb: Option<u32>,
        cpu_time_limit_ms: Option<u32>,
    ) -> Result<(), String> {
        if self.sessions.contains_key(&session_id) {
            return Err(format!("session {} already exists", session_id));
        }

        let (tx, rx) = crossbeam_channel::bounded(256);
        let slot_control = Arc::clone(&self.slot_control);
        let max = self.max_concurrency;
        let event_tx = self.event_tx.clone();
        let router = Arc::clone(&self.call_id_router);
        let shared_call_id = Arc::clone(&self.shared_call_id);
        let snap_cache = Arc::clone(&self.snapshot_cache);
        let isolate_handle = Arc::new(Mutex::new(None));
        let execution_abort = new_execution_abort();
        let isolate_handle_for_thread = Arc::clone(&isolate_handle);
        let execution_abort_for_thread = execution_abort.clone();
        let session_id_for_thread = session_id.clone();

        let name_prefix = if session_id.len() > 8 {
            &session_id[..8]
        } else {
            &session_id
        };
        let join_handle = thread::Builder::new()
            .name(format!("session-{}", name_prefix))
            .spawn(move || {
                session_thread(
                    heap_limit_mb,
                    cpu_time_limit_ms,
                    rx,
                    slot_control,
                    max,
                    event_tx,
                    router,
                    shared_call_id,
                    snap_cache,
                    isolate_handle_for_thread,
                    execution_abort_for_thread,
                    session_id_for_thread,
                );
            })
            .map_err(|e| format!("failed to spawn session thread: {}", e))?;

        self.sessions.insert(
            session_id,
            SessionEntry {
                tx,
                join_handle: Some(join_handle),
                isolate_handle,
                execution_abort,
            },
        );

        Ok(())
    }

    /// Destroy a session. Sends shutdown to the session thread and joins it.
    pub fn destroy_session(&mut self, session_id: &str) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {} does not exist", session_id))?;

        // Send shutdown, drop the sender so the session thread's rx.recv()
        // returns Err if Shutdown was consumed by an inner loop, then join.
        #[cfg(not(test))]
        if let Some(handle) = entry
            .isolate_handle
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
        {
            handle.terminate_execution();
        }
        signal_execution_abort(&entry.execution_abort, ExecutionAbortReason::Terminated);
        let _ = entry.tx.send(SessionCommand::Shutdown);
        let mut entry = self.sessions.remove(session_id).unwrap();
        drop(entry.tx);
        if let Some(handle) = entry.join_handle.take() {
            let _ = handle.join();
        }

        Ok(())
    }

    /// Send a message to a session.
    pub fn send_to_session(&self, session_id: &str, msg: SessionMessage) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {} does not exist", session_id))?;

        #[cfg(not(test))]
        if matches!(&msg, SessionMessage::TerminateExecution) {
            if let Some(handle) = entry
                .isolate_handle
                .lock()
                .ok()
                .and_then(|guard| guard.as_ref().cloned())
            {
                handle.terminate_execution();
            }
        }
        if matches!(&msg, SessionMessage::TerminateExecution) {
            signal_execution_abort(&entry.execution_abort, ExecutionAbortReason::Terminated);
        }

        entry
            .tx
            .send(SessionCommand::Message(msg))
            .map_err(|e| format!("session thread disconnected: {}", e))
    }

    /// Destroy a set of sessions, ignoring sessions that were already removed.
    pub fn destroy_sessions<I>(&mut self, session_ids: I)
    where
        I: IntoIterator<Item = String>,
    {
        for sid in session_ids {
            let _ = self.destroy_session(&sid);
        }
    }

    /// Number of registered sessions (including those waiting for a slot).
    #[allow(dead_code)]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Return all session IDs.
    #[allow(dead_code)]
    pub fn all_sessions(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    /// Number of sessions that have acquired a concurrency slot.
    #[allow(dead_code)]
    pub fn active_slot_count(&self) -> usize {
        let (lock, _) = &*self.slot_control;
        *lock.lock().unwrap()
    }

    /// Get the call_id routing table for BridgeResponse dispatch.
    pub fn call_id_router(&self) -> &CallIdRouter {
        &self.call_id_router
    }
}

/// Send a typed runtime event without re-serializing it on the session thread.
#[cfg(not(test))]
fn send_event(event_tx: &RuntimeEventSender, event: RuntimeEvent) {
    if let Err(error) = event_tx.send(event) {
        eprintln!("failed to send runtime event: {error}");
    }
}

fn send_late_message_warning(
    event_tx: &RuntimeEventSender,
    session_id: &str,
    error_code: &str,
    detail: String,
) {
    let warning = RuntimeEvent::Log {
        session_id: session_id.to_string(),
        channel: 1,
        message: format!("[{error_code}] {detail}"),
    };
    if let Err(error) = event_tx.send(warning) {
        eprintln!("failed to send late-session warning: {error}");
    }
}

fn handle_late_session_message(
    event_tx: &RuntimeEventSender,
    session_id: &str,
    message: SessionMessage,
) {
    match message {
        SessionMessage::BridgeResponse(BridgeResponse {
            call_id,
            status,
            payload,
        }) => send_late_message_warning(
            event_tx,
            session_id,
            LATE_BRIDGE_RESPONSE_ERROR_CODE,
            format!(
                "dropping BridgeResponse after execution completed (call_id={call_id}, status={status}, payload_len={})",
                payload.len()
            ),
        ),
        SessionMessage::StreamEvent(StreamEvent { event_type, payload }) => {
            if event_type == "timer" {
                return;
            }
            send_late_message_warning(
                event_tx,
                session_id,
                LATE_STREAM_EVENT_ERROR_CODE,
                format!(
                    "dropping StreamEvent after execution completed (event_type={event_type}, payload_len={})",
                    payload.len()
                ),
            )
        }
        SessionMessage::TerminateExecution => send_late_message_warning(
            event_tx,
            session_id,
            LATE_TERMINATE_EXECUTION_ERROR_CODE,
            String::from("dropping TerminateExecution after execution completed"),
        ),
        SessionMessage::InjectGlobals { .. } | SessionMessage::Execute { .. } => {}
    }
}

/// Session thread: acquires a concurrency slot, defers V8 isolate creation
/// to first Execute (when bridge code is known for snapshot lookup), and
/// processes commands until shutdown.
#[allow(clippy::too_many_arguments)]
fn session_thread(
    #[cfg_attr(test, allow(unused_variables))] heap_limit_mb: Option<u32>,
    #[cfg_attr(test, allow(unused_variables))] cpu_time_limit_ms: Option<u32>,
    rx: Receiver<SessionCommand>,
    slot_control: SlotControl,
    max_concurrency: usize,
    #[cfg_attr(test, allow(unused_variables))] event_tx: RuntimeEventSender,
    #[cfg_attr(test, allow(unused_variables))] call_id_router: CallIdRouter,
    #[cfg_attr(test, allow(unused_variables))] shared_call_id: SharedCallIdCounter,
    #[cfg_attr(test, allow(unused_variables))] snapshot_cache: Arc<SnapshotCache>,
    #[cfg_attr(test, allow(unused_variables))] isolate_handle: SharedIsolateHandle,
    #[cfg_attr(test, allow(unused_variables))] execution_abort: SharedExecutionAbort,
    #[cfg_attr(test, allow(unused_variables))] session_id: String,
) {
    // Acquire concurrency slot, but keep polling the session channel so a queued
    // session can still shut down cleanly before it ever gets a slot.
    let mut deferred_commands = VecDeque::new();
    let acquired_slot = {
        let (lock, cvar) = &*slot_control;
        let mut count = lock.lock().unwrap();
        loop {
            if *count < max_concurrency {
                *count += 1;
                break true;
            }

            let (next_count, _) = cvar
                .wait_timeout(count, std::time::Duration::from_millis(50))
                .unwrap();
            count = next_count;

            match rx.try_recv() {
                Ok(SessionCommand::Shutdown)
                | Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    break false;
                }
                Ok(command) => deferred_commands.push_back(command),
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
        }
    };

    if !acquired_slot {
        return;
    }

    // Isolate creation is deferred to first Execute (when bridge code is known
    // for snapshot cache lookup). This avoids creating an isolate that may never
    // be used and enables snapshot-based fast creation.
    #[cfg(not(test))]
    let mut v8_isolate: Option<v8::OwnedIsolate> = None;
    #[cfg(not(test))]
    let mut _v8_context: Option<v8::Global<v8::Context>> = None;

    // Whether the isolate was created from a context snapshot.
    // When true, Execute uses the snapshot's default context (bridge IIFE
    // already executed) and skips re-running the bridge code. Bridge function
    // stubs in the snapshot are replaced with real session-local functions.
    #[cfg(not(test))]
    let mut from_snapshot = false;

    #[cfg(not(test))]
    let pending = bridge::PendingPromises::new();

    // Store latest InjectGlobals V8 payload for re-injection into fresh contexts
    #[cfg(not(test))]
    let mut last_globals_payload: Option<Vec<u8>> = None;

    // Bridge code cache for V8 code caching across executions
    #[cfg(not(test))]
    let mut bridge_cache: Option<execution::BridgeCodeCache> = None;

    // Cached bridge code string to skip resending over IPC
    #[cfg(not(test))]
    let mut last_bridge_code: Option<String> = None;

    // A session can reuse its isolate across Executes only while the effective
    // bridge code stays the same. Fresh contexts cloned from a snapshot inherit
    // the snapshot's bridge IIFE, so a bridge-code change must rebuild the
    // isolate before the next execution or the session will keep restoring the
    // old snapshot forever.
    #[cfg(not(test))]
    let mut isolate_bridge_code: Option<String> = None;

    // Pre-allocated serialization buffers for V8 ValueSerializer output
    #[cfg(not(test))]
    let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());

    // Process commands until shutdown or channel close
    loop {
        let next_command = if let Some(command) = deferred_commands.pop_front() {
            Ok(command)
        } else {
            rx.recv()
        };

        match next_command {
            Ok(SessionCommand::Shutdown) | Err(_) => break,
            Ok(SessionCommand::Message(msg)) => match msg {
                SessionMessage::InjectGlobals { payload } => {
                    #[cfg(not(test))]
                    {
                        // Store V8-serialized config for injection into fresh context at Execute time
                        last_globals_payload = Some(payload);
                    }
                    #[cfg(test)]
                    {
                        let _ = payload;
                    }
                }
                SessionMessage::Execute {
                    mode,
                    file_path,
                    bridge_code,
                    post_restore_script,
                    user_code,
                } => {
                    #[cfg(not(test))]
                    {
                        let session_id = session_id.clone();
                        // Use cached bridge code when host sends empty (0-length = use cached)
                        let effective_bridge_code = if bridge_code.is_empty() {
                            last_bridge_code.as_deref().unwrap_or("").to_string()
                        } else {
                            last_bridge_code = Some(bridge_code.clone());
                            bridge_code
                        };

                        if v8_isolate.is_some()
                            && isolate_bridge_code.as_deref()
                                != Some(effective_bridge_code.as_str())
                        {
                            *isolate_handle
                                .lock()
                                .expect("session isolate handle lock poisoned") = None;
                            drop(_v8_context.take());
                            drop(v8_isolate.take());
                            from_snapshot = false;
                            isolate_bridge_code = None;
                        }

                        // Deferred isolate creation: create on first Execute using snapshot cache
                        if v8_isolate.is_none() {
                            isolate::init_v8_platform();
                            let mut iso = if !effective_bridge_code.is_empty() {
                                match snapshot_cache.get_or_create(&effective_bridge_code) {
                                    Ok(blob) => {
                                        from_snapshot = true;
                                        snapshot::create_isolate_from_snapshot(
                                            (*blob).clone(),
                                            heap_limit_mb,
                                        )
                                    }
                                    Err(e) => {
                                        eprintln!("snapshot creation failed, falling back to fresh isolate: {}", e);
                                        from_snapshot = false;
                                        isolate::create_isolate(heap_limit_mb)
                                    }
                                }
                            } else {
                                from_snapshot = false;
                                isolate::create_isolate(heap_limit_mb)
                            };
                            iso.set_host_import_module_dynamically_callback(
                                execution::dynamic_import_callback,
                            );
                            iso.set_host_initialize_import_meta_object_callback(
                                execution::import_meta_object_callback,
                            );
                            *isolate_handle
                                .lock()
                                .expect("session isolate handle lock poisoned") =
                                Some(iso.thread_safe_handle());
                            let ctx = isolate::create_context(&mut iso);
                            _v8_context = Some(ctx);
                            v8_isolate = Some(iso);
                            isolate_bridge_code = Some(effective_bridge_code.clone());
                        }

                        let iso = v8_isolate.as_mut().unwrap();

                        // Create execution context: Context::new on a snapshot-restored
                        // isolate gives a fresh clone of the snapshot's default context
                        // (bridge IIFE already executed, all infrastructure set up).
                        // On a non-snapshot isolate, this gives a blank context.
                        let exec_context = isolate::create_context(iso);

                        // Inject globals from last InjectGlobals payload
                        if let Some(ref payload) = last_globals_payload {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            execution::inject_globals_from_payload(scope, payload);
                        }

                        // Arm a per-execution abort channel so timeouts and external
                        // terminate requests can unblock sync bridge waits.
                        let (_active_execution_abort, abort_rx) =
                            ActiveExecutionAbort::arm(&execution_abort);

                        // Create deferred queue for sync bridge call filtering
                        let deferred_queue = new_deferred_queue();

                        // Create BridgeCallContext with channel sender (no shared mutex)
                        let channel_rx = ChannelResponseReceiver::with_abort(
                            rx.clone(),
                            abort_rx.clone(),
                            Arc::clone(&deferred_queue),
                        );
                        let bridge_ctx = BridgeCallContext::with_receiver(
                            Box::new(ChannelRuntimeEventSender::new(event_tx.clone())),
                            Box::new(channel_rx),
                            session_id.clone(),
                            Arc::clone(&call_id_router),
                            Arc::clone(&shared_call_id),
                        );

                        // Replace stub bridge functions with real session-local ones
                        // (on snapshot context) or register from scratch (on fresh context).
                        // Both paths use the same function — global.set() works for both.
                        let _sync_store;
                        let _async_store;
                        {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);

                            (_sync_store, _async_store) = bridge::replace_bridge_fns(
                                scope,
                                &bridge_ctx as *const BridgeCallContext,
                                &pending as *const bridge::PendingPromises,
                                &session_buffers
                                    as *const std::cell::RefCell<bridge::SessionBuffers>,
                                SYNC_BRIDGE_FNS,
                                ASYNC_BRIDGE_FNS,
                            );
                        }

                        // Run post-restore init script (config, mutable state reset)
                        // after bridge fn replacement but before user code
                        if !post_restore_script.is_empty() {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            let (prs_code, prs_err) =
                                execution::run_init_script(scope, &post_restore_script);
                            if prs_code != 0 {
                                let result_frame = RuntimeEvent::ExecutionResult {
                                    session_id,
                                    exit_code: prs_code,
                                    exports: None,
                                    error: prs_err.map(|e| ExecutionErrorBin {
                                        error_type: e.error_type,
                                        message: e.message,
                                        stack: e.stack,
                                        code: e.code.unwrap_or_default(),
                                    }),
                                };
                                send_event(&event_tx, result_frame);
                                continue;
                            }
                        }

                        // Start timeout guard before execution
                        let mut timeout_guard = match cpu_time_limit_ms {
                            Some(ms) => {
                                let handle = iso.thread_safe_handle();
                                Some(crate::timeout::TimeoutGuard::with_execution_abort(
                                    ms,
                                    handle,
                                    execution_abort.clone(),
                                ))
                            }
                            _ => None,
                        };

                        // On snapshot-restored context, skip bridge IIFE (already in
                        // snapshot) and run user code only. On fresh context, run full
                        // bridge code + user code as before.
                        let bridge_code_for_exec = if from_snapshot {
                            ""
                        } else {
                            &effective_bridge_code
                        };
                        let file_path_opt = if file_path.is_empty() {
                            None
                        } else {
                            Some(file_path.as_str())
                        };
                        let (mut code, mut exports, mut error) = if mode == 0 {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            let (c, e) = execution::execute_script_with_options(
                                scope,
                                Some(&bridge_ctx),
                                bridge_code_for_exec,
                                &user_code,
                                file_path_opt,
                                &mut bridge_cache,
                            );
                            (c, None, e)
                        } else {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            execution::execute_module(
                                scope,
                                &bridge_ctx,
                                bridge_code_for_exec,
                                &user_code,
                                file_path_opt,
                                &mut bridge_cache,
                            )
                        };

                        // Re-check async ESM completion once immediately so
                        // pure-microtask top-level await settles without
                        // needing a bridge event-loop round-trip.
                        if mode != 0 && error.is_none() {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            if let Some((next_code, next_exports, next_error)) =
                                execution::finalize_pending_module_evaluation(scope)
                            {
                                code = next_code;
                                exports = next_exports;
                                error = next_error;
                            }
                        }

                        // Run event loop while bridge work or async ESM
                        // evaluation is still pending. For ESM modules (mode != 0),
                        // always enter the event loop even if no pending promises
                        // are visible yet — the module body may have registered
                        // timers, stdin listeners, or child_process handles that
                        // need event loop pumping to deliver their callbacks.
                        let should_enter_event_loop = pending.len() > 0
                            || execution::has_pending_module_evaluation()
                            || execution::has_pending_script_evaluation()
                            || !deferred_queue.lock().unwrap().is_empty();
                        let event_loop_status = if should_enter_event_loop {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            run_event_loop(
                                scope,
                                &rx,
                                &pending,
                                Some(&abort_rx),
                                Some(&deferred_queue),
                            )
                        } else {
                            EventLoopStatus::Completed
                        };

                        let mut terminated =
                            matches!(event_loop_status, EventLoopStatus::Terminated);
                        if let EventLoopStatus::Failed(next_code, next_error) = event_loop_status {
                            code = next_code;
                            error = Some(next_error);
                        }

                        // Finalize any entry-module top-level await that was
                        // waiting on bridge-driven async work (timers/network).
                        if !terminated && mode != 0 && error.is_none() {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            if let Some((next_code, next_exports, next_error)) =
                                execution::finalize_pending_module_evaluation(scope)
                            {
                                code = next_code;
                                exports = next_exports;
                                error = next_error;
                            }
                        }

                        // Keep the session alive while handles (timers, child
                        // processes, stdin listeners) are active. Long-lived
                        // ACP adapters often run as plain scripts, so this
                        // cannot be limited to ESM entrypoints.
                        if !terminated && error.is_none() {
                            // Phase 1: call _waitForActiveHandles() to register a pending promise
                            {
                                let scope = &mut v8::HandleScope::new(iso);
                                let ctx = v8::Local::new(scope, &exec_context);
                                let scope = &mut v8::ContextScope::new(scope, ctx);
                                let global = ctx.global(scope);
                                let key = v8::String::new(scope, "_waitForActiveHandles").unwrap();
                                if let Some(func) = global.get(scope, key.into()) {
                                    if func.is_function() {
                                        let func =
                                            v8::Local::<v8::Function>::try_from(func).unwrap();
                                        let recv = v8::undefined(scope).into();
                                        if let Some(result) = func.call(scope, recv, &[]) {
                                            if result.is_promise() {
                                                let promise =
                                                    v8::Local::<v8::Promise>::try_from(result)
                                                        .unwrap();
                                                if promise.state() == v8::PromiseState::Pending {
                                                    execution::set_pending_script_evaluation(
                                                        scope, promise,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Phase 2: pump event loop for active handles
                            if pending.len() > 0
                                || execution::has_pending_script_evaluation()
                                || !deferred_queue.lock().unwrap().is_empty()
                            {
                                let scope = &mut v8::HandleScope::new(iso);
                                let ctx = v8::Local::new(scope, &exec_context);
                                let scope = &mut v8::ContextScope::new(scope, ctx);
                                let event_loop_status = run_event_loop(
                                    scope,
                                    &rx,
                                    &pending,
                                    Some(&abort_rx),
                                    Some(&deferred_queue),
                                );

                                if matches!(event_loop_status, EventLoopStatus::Terminated) {
                                    terminated = true;
                                }
                                if let EventLoopStatus::Failed(next_code, next_error) =
                                    event_loop_status
                                {
                                    code = next_code;
                                    error = Some(next_error);
                                }
                            }
                        }

                        if !terminated && mode == 0 && error.is_none() {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            if let Some((next_code, next_error)) =
                                execution::finalize_pending_script_evaluation(scope)
                            {
                                code = next_code;
                                error = next_error;
                            }
                        }

                        // Check if timeout fired
                        let abort_reason = execution_abort_reason(&execution_abort);
                        let timed_out = timeout_guard.as_ref().is_some_and(|g| g.timed_out())
                            || matches!(abort_reason, Some(ExecutionAbortReason::TimedOut));

                        // Cancel timeout guard (joins timer thread)
                        if let Some(ref mut guard) = timeout_guard {
                            guard.cancel();
                        }
                        drop(timeout_guard);

                        if matches!(abort_reason, Some(ExecutionAbortReason::Terminated)) {
                            terminated = true;
                            code = 1;
                            exports = None;
                            error = None;
                        }

                        // Send ExecutionResult
                        let result_frame = if timed_out {
                            RuntimeEvent::ExecutionResult {
                                session_id,
                                exit_code: 1,
                                exports: None,
                                error: Some(ExecutionErrorBin {
                                    error_type: "Error".into(),
                                    message: "Script execution timed out".into(),
                                    stack: String::new(),
                                    code: "ERR_SCRIPT_EXECUTION_TIMEOUT".into(),
                                }),
                            }
                        } else if terminated {
                            RuntimeEvent::ExecutionResult {
                                session_id,
                                exit_code: 1,
                                exports: None,
                                error: Some(ExecutionErrorBin {
                                    error_type: "Error".into(),
                                    message: "Execution terminated".into(),
                                    stack: String::new(),
                                    code: String::new(),
                                }),
                            }
                        } else {
                            RuntimeEvent::ExecutionResult {
                                session_id,
                                exit_code: code,
                                exports,
                                error: error.map(|e| ExecutionErrorBin {
                                    error_type: e.error_type,
                                    message: e.message,
                                    stack: e.stack,
                                    code: e.code.unwrap_or_default(),
                                }),
                            }
                        };

                        execution::clear_pending_module_evaluation();
                        execution::clear_pending_script_evaluation();
                        execution::clear_module_state();

                        send_event(&event_tx, result_frame);
                    }
                    #[cfg(test)]
                    {
                        let _ = (mode, file_path, bridge_code, post_restore_script, user_code);
                    }
                }
                SessionMessage::BridgeResponse(_)
                | SessionMessage::StreamEvent(_)
                | SessionMessage::TerminateExecution => {
                    handle_late_session_message(&event_tx, &session_id, msg);
                }
            },
        }
    }

    // Drop V8 resources (only present in non-test mode)
    #[cfg(not(test))]
    {
        *isolate_handle
            .lock()
            .expect("session isolate handle lock poisoned") = None;
        drop(_v8_context.take());
        drop(v8_isolate.take());
    }

    // Release concurrency slot
    {
        let (lock, cvar) = &*slot_control;
        let mut count = lock.lock().unwrap();
        *count -= 1;
        cvar.notify_one();
    }
}

/// Sync and async bridge function names registered on the V8 global.
/// These match the bridge contract (bridge-contract.ts HOST_BRIDGE_GLOBAL_KEYS).
///
/// Sync functions block V8 while the host processes the call (applySync/applySyncPromise).
/// Async functions return a Promise to V8, resolved when the host responds (apply).
pub(crate) const SYNC_BRIDGE_FNS: &[&str] = &[
    // Console
    "_log",
    "_error",
    // Python guest VFS RPC bridge
    "_pythonRpc",
    "_pythonStdinRead",
    // Module loading (syncPromise — host resolves async, Rust blocks)
    "_loadPolyfill",
    "_resolveModule",
    "_loadFile",
    // Sync module loading (bypass _loadPolyfill dispatch, used by CJS require)
    "_resolveModuleSync",
    "_loadFileSync",
    "_batchResolveModules",
    // Crypto
    "_cryptoRandomFill",
    "_cryptoRandomUUID",
    "_cryptoHashDigest",
    "_cryptoHmacDigest",
    "_cryptoPbkdf2",
    "_cryptoScrypt",
    "_cryptoCipheriv",
    "_cryptoDecipheriv",
    "_cryptoCipherivCreate",
    "_cryptoCipherivUpdate",
    "_cryptoCipherivFinal",
    "_cryptoSign",
    "_cryptoVerify",
    "_cryptoAsymmetricOp",
    "_cryptoCreateKeyObject",
    "_cryptoGenerateKeyPairSync",
    "_cryptoGenerateKeySync",
    "_cryptoGeneratePrimeSync",
    "_cryptoDiffieHellman",
    "_cryptoDiffieHellmanGroup",
    "_cryptoDiffieHellmanSessionCreate",
    "_cryptoDiffieHellmanSessionCall",
    "_cryptoSubtle",
    // Filesystem (all syncPromise)
    "_fsReadFile",
    "_fsWriteFile",
    "_fsReadFileBinary",
    "_fsWriteFileBinary",
    "_fsReadDir",
    "_fsMkdir",
    "_fsRmdir",
    "_fsExists",
    "_fsStat",
    "_fsUnlink",
    "_fsRename",
    "_fsChmod",
    "_fsChown",
    "_fsLink",
    "_fsSymlink",
    "_fsReadlink",
    "_fsLstat",
    "_fsTruncate",
    "_fsUtimes",
    "fs.openSync",
    "fs.closeSync",
    "fs.readSync",
    "fs.writeSync",
    "fs.fstatSync",
    // Child process (sync)
    "_childProcessSpawnStart",
    "_childProcessPoll",
    "_childProcessStdinWrite",
    "_childProcessStdinClose",
    "_childProcessKill",
    "_childProcessSpawnSync",
    "_processKill",
    "_processSignalState",
    "_vmCreateContext",
    "_vmRunInContext",
    "_vmRunInThisContext",
    "process.memoryUsage",
    "process.cpuUsage",
    "process.resourceUsage",
    "process.versions",
    // HTTP/2 and network bridge operations with sync or syncPromise semantics
    "_networkHttp2ServerListenRaw",
    "_networkHttp2SessionConnectRaw",
    "_networkHttp2SessionRequestRaw",
    "_networkHttp2SessionSettingsRaw",
    "_networkHttp2SessionSetLocalWindowSizeRaw",
    "_networkHttp2SessionGoawayRaw",
    "_networkHttp2SessionCloseRaw",
    "_networkHttp2SessionDestroyRaw",
    "_networkHttp2ServerPollRaw",
    "_networkHttp2SessionPollRaw",
    "_networkHttp2StreamRespondRaw",
    "_networkHttp2StreamPushStreamRaw",
    "_networkHttp2StreamWriteRaw",
    "_networkHttp2StreamEndRaw",
    "_networkHttp2StreamCloseRaw",
    "_networkHttp2StreamPauseRaw",
    "_networkHttp2StreamResumeRaw",
    "_networkHttp2StreamRespondWithFileRaw",
    "_networkHttp2ServerRespondRaw",
    "_networkHttpServerRespondRaw",
    "_upgradeSocketWriteRaw",
    "_upgradeSocketEndRaw",
    "_upgradeSocketDestroyRaw",
    "_netSocketConnectRaw",
    "_netSocketPollRaw",
    "_netSocketReadRaw",
    "_netSocketSetNoDelayRaw",
    "_netSocketSetKeepAliveRaw",
    "_netSocketWriteRaw",
    "_netSocketEndRaw",
    "_netSocketDestroyRaw",
    "_netSocketUpgradeTlsRaw",
    "_netSocketGetTlsClientHelloRaw",
    "_netSocketTlsQueryRaw",
    "_tlsGetCiphersRaw",
    "_netServerListenRaw",
    "_netServerAcceptRaw",
    "_dgramSocketCreateRaw",
    "_dgramSocketBindRaw",
    "_dgramSocketRecvRaw",
    "_dgramSocketSendRaw",
    "_dgramSocketCloseRaw",
    "_dgramSocketAddressRaw",
    "_dgramSocketSetBufferSizeRaw",
    "_dgramSocketGetBufferSizeRaw",
    "_sqliteConstantsRaw",
    "_sqliteDatabaseOpenRaw",
    "_sqliteDatabaseCloseRaw",
    "_sqliteDatabaseExecRaw",
    "_sqliteDatabaseQueryRaw",
    "_sqliteDatabasePrepareRaw",
    "_sqliteDatabaseLocationRaw",
    "_sqliteDatabaseCheckpointRaw",
    "_sqliteStatementRunRaw",
    "_sqliteStatementGetRaw",
    "_sqliteStatementAllRaw",
    "_sqliteStatementColumnsRaw",
    "_sqliteStatementSetReturnArraysRaw",
    "_sqliteStatementSetReadBigIntsRaw",
    "_sqliteStatementSetAllowBareNamedParametersRaw",
    "_sqliteStatementSetAllowUnknownNamedParametersRaw",
    "_sqliteStatementFinalizeRaw",
    "_kernelStdinReadRaw",
    "_kernelStdioWriteRaw",
    "_kernelPollRaw",
    "_ptySetRawMode",
];

pub(crate) const ASYNC_BRIDGE_FNS: &[&str] = &[
    // Module loading (async)
    "_dynamicImport",
    // Timer
    "_scheduleTimer",
    "_kernelStdinRead",
    // Network (async)
    "_networkDnsLookupRaw",
    "_networkDnsResolveRaw",
    "_networkHttpServerListenRaw",
    "_networkHttpServerCloseRaw",
    "_networkHttpServerWaitRaw",
    "_networkHttp2ServerCloseRaw",
    "_networkHttp2ServerWaitRaw",
    "_networkHttp2SessionWaitRaw",
    "_netSocketWaitConnectRaw",
    "_netServerCloseRaw",
    // Filesystem promises (async)
    "_fsReadFileAsync",
    "_fsWriteFileAsync",
    "_fsReadFileBinaryAsync",
    "_fsWriteFileBinaryAsync",
    "_fsReadDirAsync",
    "_fsMkdirAsync",
    "_fsRmdirAsync",
    "_fsAccessAsync",
    "_fsStatAsync",
    "_fsUnlinkAsync",
    "_fsRenameAsync",
    "_fsChmodAsync",
    "_fsChownAsync",
    "_fsLinkAsync",
    "_fsSymlinkAsync",
    "_fsReadlinkAsync",
    "_fsLstatAsync",
    "_fsTruncateAsync",
    "_fsUtimesAsync",
];

/// Run the session event loop: dispatch incoming messages to V8.
///
/// Called after script/module execution when there are pending async promises.
/// Polls the session channel for BridgeResponse, StreamEvent, and
/// TerminateExecution messages, dispatching each into V8 with microtask flush.
///
/// When `deferred` is provided, drains queued messages from sync bridge calls
/// before blocking on the channel. This prevents StreamEvent loss when sync
/// bridge calls consume non-BridgeResponse messages from the shared channel.
///
/// When `abort_rx` is provided (timeout is configured), uses `select!` to
/// also monitor the abort channel — if the timeout fires and drops the sender,
/// the abort channel unblocks and terminates execution.
///
/// Returns true if execution completed normally, false if terminated.
#[doc(hidden)]
pub fn run_event_loop(
    scope: &mut v8::HandleScope,
    rx: &Receiver<SessionCommand>,
    pending: &crate::bridge::PendingPromises,
    abort_rx: Option<&crossbeam_channel::Receiver<()>>,
    deferred: Option<&DeferredQueue>,
) -> EventLoopStatus {
    while pending.len() > 0
        || execution::pending_module_evaluation_needs_wait(scope)
        || execution::pending_script_evaluation_needs_wait(scope)
        || pending_guest_timer_count(scope) > 0
        || deferred
            .map(|dq| !dq.lock().unwrap().is_empty())
            .unwrap_or(false)
    {
        pump_v8_message_loop(scope);

        // Drain deferred messages queued by sync bridge calls before blocking
        if let Some(dq) = deferred {
            let frames: Vec<SessionMessage> = dq.lock().unwrap().drain(..).collect();
            for frame in frames {
                let status = dispatch_event_loop_frame(scope, frame, pending);
                if !matches!(status, EventLoopStatus::Completed) {
                    return status;
                }
            }
            if pending.len() == 0
                && !execution::pending_module_evaluation_needs_wait(scope)
                && !execution::pending_script_evaluation_needs_wait(scope)
                && pending_guest_timer_count(scope) == 0
            {
                break;
            }
        }

        // Flush microtasks before blocking. Run in a loop to drain the full
        // microtask queue -- each checkpoint may resolve Promises that schedule
        // new microtasks (e.g., async function await chains).
        for _ in 0..100 {
            scope.perform_microtask_checkpoint();
            pump_v8_message_loop(scope);
            // Check if new deferred work appeared from microtask processing
            if let Some(dq) = deferred {
                if !dq.lock().unwrap().is_empty() {
                    break; // New bridge work to process
                }
            }
        }

        // Re-check exit conditions after microtask flush — the microtask may
        // have resolved all pending promises or registered new handles.
        if pending.len() == 0
            && !execution::pending_module_evaluation_needs_wait(scope)
            && !execution::pending_script_evaluation_needs_wait(scope)
            && pending_guest_timer_count(scope) == 0
            && deferred
                .map(|dq| dq.lock().unwrap().is_empty())
                .unwrap_or(true)
        {
            break;
        }

        // Receive next command with interleaved microtask processing.
        // Instead of blocking indefinitely, use a short timeout so we can
        // periodically flush microtasks (like Node.js's libuv + DrainTasks pattern).
        let cmd = loop {
            let recv_result = if let Some(abort) = abort_rx {
                crossbeam_channel::select! {
                    recv(rx) -> result => result.ok(),
                    recv(abort) -> _ => {
                        scope.terminate_execution();
                        return EventLoopStatus::Terminated;
                    },
                    default(std::time::Duration::from_millis(1)) => None,
                }
            } else {
                rx.recv_timeout(std::time::Duration::from_millis(1)).ok()
            };
            if let Some(cmd) = recv_result {
                break cmd;
            }
            // No command received — flush microtasks and check deferred queue
            scope.perform_microtask_checkpoint();
            pump_v8_message_loop(scope);
            if let Some(dq) = deferred {
                if !dq.lock().unwrap().is_empty() {
                    // New deferred work appeared — drain it in the outer loop
                    let frames: Vec<SessionMessage> = dq.lock().unwrap().drain(..).collect();
                    for frame in frames {
                        let status = dispatch_event_loop_frame(scope, frame, pending);
                        if !matches!(status, EventLoopStatus::Completed) {
                            return status;
                        }
                    }
                }
            }
            // Check if we should exit
            if pending.len() == 0
                && !execution::pending_module_evaluation_needs_wait(scope)
                && !execution::pending_script_evaluation_needs_wait(scope)
                && pending_guest_timer_count(scope) == 0
                && deferred
                    .map(|dq| dq.lock().unwrap().is_empty())
                    .unwrap_or(true)
            {
                return EventLoopStatus::Completed;
            }
        };

        match cmd {
            SessionCommand::Message(frame) => {
                let status = dispatch_event_loop_frame(scope, frame, pending);
                if !matches!(status, EventLoopStatus::Completed) {
                    return status;
                }
            }
            SessionCommand::Shutdown => return EventLoopStatus::Terminated,
        }
    }
    EventLoopStatus::Completed
}

fn pending_guest_timer_count(scope: &mut v8::HandleScope) -> usize {
    let tc = &mut v8::TryCatch::new(scope);
    let context = tc.get_current_context();
    let global = context.global(tc);
    let key = match v8::String::new(tc, "_getPendingTimerCount") {
        Some(key) => key,
        None => return 0,
    };
    let Some(func_value) = global.get(tc, key.into()) else {
        return 0;
    };
    let Ok(func) = v8::Local::<v8::Function>::try_from(func_value) else {
        return 0;
    };
    let Some(result) = func.call(tc, global.into(), &[]) else {
        return 0;
    };

    result
        .integer_value(tc)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
}

fn pump_v8_message_loop(scope: &mut v8::HandleScope) {
    let platform = v8::V8::get_current_platform();
    while v8::Platform::pump_message_loop(&platform, scope, false) {
        scope.perform_microtask_checkpoint();
    }
}

/// Dispatch a single session message within the event loop.
/// Returns the event-loop status after handling the frame.
#[derive(Debug)]
#[doc(hidden)]
pub enum EventLoopStatus {
    Completed,
    Terminated,
    Failed(i32, ExecutionError),
}

fn dispatch_event_loop_frame(
    scope: &mut v8::HandleScope,
    frame: SessionMessage,
    pending: &crate::bridge::PendingPromises,
) -> EventLoopStatus {
    match frame {
        SessionMessage::BridgeResponse(BridgeResponse {
            call_id,
            status,
            payload,
        }) => {
            let (result, error) = if status == 1 {
                (None, Some(String::from_utf8_lossy(&payload).to_string()))
            } else if !payload.is_empty() {
                // status=0: V8-serialized, status=2: raw binary (Uint8Array)
                (Some(payload), None)
            } else {
                (None, None)
            };
            let _ = crate::bridge::resolve_pending_promise(scope, pending, call_id, result, error);
            // Microtasks already flushed in resolve_pending_promise
            EventLoopStatus::Completed
        }
        SessionMessage::StreamEvent(StreamEvent {
            event_type,
            payload,
        }) => {
            let tc = &mut v8::TryCatch::new(scope);
            crate::stream::dispatch_stream_event(tc, &event_type, &payload);
            tc.perform_microtask_checkpoint();
            if let Some(exception) = tc.exception() {
                let (code, err) = execution::exception_to_result(tc, exception);
                return EventLoopStatus::Failed(code, err);
            }
            if let Some(err) = execution::take_unhandled_promise_rejection(tc) {
                return EventLoopStatus::Failed(1, err);
            }
            EventLoopStatus::Completed
        }
        SessionMessage::TerminateExecution => {
            scope.terminate_execution();
            EventLoopStatus::Terminated
        }
        _ => {
            // Ignore other messages during event loop
            EventLoopStatus::Completed
        }
    }
}

/// ResponseReceiver that receives typed session messages directly from the session channel.
///
/// Only returns BridgeResponse frames from recv_response(). Non-BridgeResponse
/// messages (StreamEvent, TerminateExecution) consumed during sync bridge calls
/// are queued in the deferred queue for later processing by the event loop.
///
/// When `abort_rx` is set (timeout configured), uses `select!` to also monitor
/// the abort channel. If the timeout fires, the abort sender is dropped, which
/// unblocks the select and returns a timeout error.
pub(crate) struct ChannelResponseReceiver {
    rx: Receiver<SessionCommand>,
    abort_rx: Option<crossbeam_channel::Receiver<()>>,
    deferred: DeferredQueue,
}

impl ChannelResponseReceiver {
    #[allow(dead_code)]
    pub(crate) fn new(rx: Receiver<SessionCommand>, deferred: DeferredQueue) -> Self {
        ChannelResponseReceiver {
            rx,
            abort_rx: None,
            deferred,
        }
    }

    pub(crate) fn with_abort(
        rx: Receiver<SessionCommand>,
        abort_rx: crossbeam_channel::Receiver<()>,
        deferred: DeferredQueue,
    ) -> Self {
        ChannelResponseReceiver {
            rx,
            abort_rx: Some(abort_rx),
            deferred,
        }
    }
}

impl crate::host_call::BridgeResponseReceiver for ChannelResponseReceiver {
    fn recv_response(&self, expected_call_id: u64) -> Result<BridgeResponse, String> {
        loop {
            // Wait for next command, with optional abort monitoring
            let cmd = if let Some(ref abort) = self.abort_rx {
                crossbeam_channel::select! {
                    recv(self.rx) -> result => match result {
                        Ok(cmd) => cmd,
                        Err(_) => return Err("channel closed".into()),
                    },
                    recv(abort) -> _ => {
                        return Err("execution aborted".into());
                    },
                }
            } else {
                match self.rx.recv() {
                    Ok(cmd) => cmd,
                    Err(_) => return Err("channel closed".into()),
                }
            };

            match cmd {
                SessionCommand::Message(frame) => {
                    if let SessionMessage::BridgeResponse(response) = &frame {
                        let call_id = response.call_id;
                        if call_id == expected_call_id {
                            return Ok(response.clone());
                        }
                        self.deferred.lock().unwrap().push_back(frame);
                        continue;
                    }
                    // Queue non-BridgeResponse for later event loop processing
                    self.deferred.lock().unwrap().push_back(frame);
                }
                SessionCommand::Shutdown => return Err("session shutdown".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_os_bridge::{bridge_contract, BridgeCallConvention};
    use std::collections::{HashMap, HashSet};

    /// Helper to create a SessionManager for tests
    fn test_manager(max: usize) -> SessionManager {
        test_manager_with_events(max).0
    }

    fn test_manager_with_events(max: usize) -> (SessionManager, Receiver<RuntimeEvent>) {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let router: CallIdRouter = Arc::new(Mutex::new(HashMap::new()));
        let snap_cache = Arc::new(SnapshotCache::new(4));
        let manager = SessionManager::new(max, tx, router, snap_cache);
        (manager, _rx)
    }

    fn expect_late_message_warning(
        rx: &Receiver<RuntimeEvent>,
        session_id: &str,
        error_code: &str,
        detail_fragment: &str,
    ) {
        let event = rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .expect("late-message warning");
        match event {
            RuntimeEvent::Log {
                session_id: observed_session_id,
                channel,
                message,
            } => {
                assert_eq!(observed_session_id, session_id);
                assert_eq!(channel, 1, "late warnings should use stderr channel");
                assert!(
                    message.contains(error_code),
                    "warning should contain error code {error_code}, got {message}"
                );
                assert!(
                    message.contains(detail_fragment),
                    "warning should mention {detail_fragment}, got {message}"
                );
            }
            other => panic!("expected late-message warning log, got {other:?}"),
        }
    }

    #[test]
    fn bridge_contract_globals_are_registered() {
        let contract = bridge_contract();

        let expected_sync = contract
            .groups
            .iter()
            .filter(|group| {
                matches!(
                    group.convention,
                    BridgeCallConvention::Sync | BridgeCallConvention::SyncPromise
                )
            })
            .flat_map(|group| group.names.iter().map(String::as_str))
            .collect::<HashSet<_>>();
        let expected_async = contract
            .groups
            .iter()
            .filter(|group| group.convention == BridgeCallConvention::Async)
            .flat_map(|group| group.names.iter().map(String::as_str))
            .collect::<HashSet<_>>();

        let registered_sync = SYNC_BRIDGE_FNS.iter().copied().collect::<HashSet<_>>();
        let registered_async = ASYNC_BRIDGE_FNS.iter().copied().collect::<HashSet<_>>();

        assert_eq!(
            registered_sync, expected_sync,
            "SYNC_BRIDGE_FNS drifted from crates/bridge/bridge-contract.json"
        );
        assert_eq!(
            registered_async, expected_async,
            "ASYNC_BRIDGE_FNS drifted from crates/bridge/bridge-contract.json"
        );
    }

    #[test]
    fn session_management() {
        // Consolidated test to avoid V8 inter-test SIGSEGV issues.
        // Covers: lifecycle and concurrency queuing.

        // --- Part 1: Single session create/destroy ---
        {
            let mut mgr = test_manager(4);

            mgr.create_session("session-aaa".into(), None, None)
                .expect("create session A");
            assert_eq!(mgr.session_count(), 1);

            // Wait for thread to acquire slot and create isolate
            std::thread::sleep(std::time::Duration::from_millis(200));

            // Destroy session A
            mgr.destroy_session("session-aaa")
                .expect("destroy session A");
            assert_eq!(mgr.session_count(), 0);
        }

        // --- Part 2: Multiple sessions ---
        {
            let mut mgr = test_manager(4);

            mgr.create_session("session-bbb".into(), None, None)
                .expect("create session B");
            mgr.create_session("session-ccc".into(), Some(16), None)
                .expect("create session C");
            assert_eq!(mgr.session_count(), 2);

            std::thread::sleep(std::time::Duration::from_millis(200));

            // Duplicate session ID is rejected
            let err = mgr.create_session("session-bbb".into(), None, None);
            assert!(err.is_err());
            assert!(err.unwrap_err().contains("already exists"));

            // Sending to a missing session still fails.
            let err = mgr.send_to_session("missing", SessionMessage::TerminateExecution);
            assert!(err.is_err());
            assert!(err.unwrap_err().contains("does not exist"));

            // Destroy non-existent session
            let err = mgr.destroy_session("no-such-session");
            assert!(err.is_err());
            assert!(err.unwrap_err().contains("does not exist"));

            mgr.destroy_sessions(["session-bbb".into(), "session-ccc".into()]);
            assert_eq!(mgr.session_count(), 0);
        }

        // --- Part 3: Max concurrency queuing ---
        {
            let mut mgr = test_manager(2);

            mgr.create_session("s1".into(), None, None)
                .expect("create s1");
            mgr.create_session("s2".into(), None, None)
                .expect("create s2");
            mgr.create_session("s3".into(), None, None)
                .expect("create s3");

            // Allow threads to acquire slots
            std::thread::sleep(std::time::Duration::from_millis(300));

            // Only 2 slots active (s3 is queued)
            assert_eq!(mgr.active_slot_count(), 2);
            assert_eq!(mgr.session_count(), 3);

            // Destroy s1 — releases slot, s3 acquires it
            mgr.destroy_session("s1").expect("destroy s1");
            std::thread::sleep(std::time::Duration::from_millis(300));
            assert_eq!(mgr.active_slot_count(), 2);
            assert_eq!(mgr.session_count(), 2);

            // Destroy remaining
            mgr.destroy_sessions(["s2".into(), "s3".into()]);
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert_eq!(mgr.session_count(), 0);
            assert_eq!(mgr.active_slot_count(), 0);
        }
    }

    #[test]
    fn channel_response_receiver_filters_bridge_response() {
        use crate::host_call::BridgeResponseReceiver;

        // Sync bridge call interleaved with StreamEvent does not drop the StreamEvent
        let (tx, rx) = crossbeam_channel::bounded(10);
        let deferred = new_deferred_queue();
        let receiver = ChannelResponseReceiver::new(rx, Arc::clone(&deferred));

        // Send: StreamEvent, TerminateExecution, then BridgeResponse
        tx.send(SessionCommand::Message(SessionMessage::StreamEvent(
            StreamEvent {
                event_type: "child_stdout".into(),
                payload: vec![0x01, 0x02],
            },
        )))
        .unwrap();
        tx.send(SessionCommand::Message(SessionMessage::TerminateExecution))
            .unwrap();
        tx.send(SessionCommand::Message(SessionMessage::BridgeResponse(
            BridgeResponse {
                call_id: 1,
                status: 0,
                payload: vec![0xAB],
            },
        )))
        .unwrap();

        // recv_response should skip StreamEvent and TerminateExecution, return BridgeResponse
        let frame = receiver.recv_response(1).unwrap();
        assert!(
            frame.call_id == 1,
            "expected BridgeResponse with call_id=1, got {:?}",
            frame
        );

        // Deferred queue should contain the StreamEvent and TerminateExecution
        let dq = deferred.lock().unwrap();
        assert_eq!(dq.len(), 2, "expected 2 deferred messages");
        assert!(
            matches!(&dq[0], SessionMessage::StreamEvent(StreamEvent { event_type, .. }) if event_type == "child_stdout"),
            "first deferred should be StreamEvent"
        );
        assert!(
            matches!(&dq[1], SessionMessage::TerminateExecution),
            "second deferred should be TerminateExecution"
        );
    }

    #[test]
    fn late_terminate_execution_is_logged_instead_of_silently_dropped() {
        let (mut mgr, rx) = test_manager_with_events(1);
        mgr.create_session("late-terminate".into(), None, None)
            .expect("create session");

        mgr.send_to_session("late-terminate", SessionMessage::TerminateExecution)
            .expect("send late terminate");

        expect_late_message_warning(
            &rx,
            "late-terminate",
            LATE_TERMINATE_EXECUTION_ERROR_CODE,
            "TerminateExecution",
        );

        mgr.destroy_session("late-terminate")
            .expect("destroy session");
    }

    #[test]
    fn channel_response_receiver_abort_unblocks_waiting_sync_call() {
        use crate::host_call::BridgeResponseReceiver;

        let (_tx, rx) = crossbeam_channel::bounded(1);
        let deferred = new_deferred_queue();
        let execution_abort = new_execution_abort();
        let (_active_abort, abort_rx) = ActiveExecutionAbort::arm(&execution_abort);
        let receiver = ChannelResponseReceiver::with_abort(rx, abort_rx, deferred);

        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let join_handle = std::thread::spawn(move || {
            let _ = result_tx.send(receiver.recv_response(1));
        });

        std::thread::sleep(std::time::Duration::from_millis(25));
        signal_execution_abort(&execution_abort, ExecutionAbortReason::Terminated);

        let result = result_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("abort should unblock the waiting receiver");
        assert_eq!(
            result.expect_err("abort should not yield a bridge response"),
            "execution aborted"
        );

        join_handle
            .join()
            .expect("receiver thread should exit cleanly");
    }

    #[test]
    fn late_bridge_response_is_logged_instead_of_silently_dropped() {
        let (mut mgr, rx) = test_manager_with_events(1);
        mgr.create_session("late-bridge".into(), None, None)
            .expect("create session");

        mgr.send_to_session(
            "late-bridge",
            SessionMessage::BridgeResponse(BridgeResponse {
                call_id: 41,
                status: 0,
                payload: vec![0xAA, 0xBB],
            }),
        )
        .expect("send late bridge response");

        expect_late_message_warning(
            &rx,
            "late-bridge",
            LATE_BRIDGE_RESPONSE_ERROR_CODE,
            "BridgeResponse",
        );

        mgr.destroy_session("late-bridge").expect("destroy session");
    }
}
