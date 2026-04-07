// Session management: create/destroy sessions with V8 isolates on dedicated threads

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crossbeam_channel::{Receiver, Sender};

use crate::execution;
use crate::ipc::ExecutionError;
use crate::host_call::{CallIdRouter, SharedCallIdCounter};
#[cfg(not(test))]
use crate::host_call::{BridgeCallContext, ChannelFrameSender};
use crate::ipc_binary::BinaryFrame;
#[cfg(not(test))]
use crate::ipc_binary::{self, ExecutionErrorBin};
use crate::snapshot::SnapshotCache;
#[cfg(not(test))]
use crate::{bridge, isolate, snapshot};

/// Commands sent to a session thread
pub enum SessionCommand {
    /// Shut down the session and destroy the isolate
    Shutdown,
    /// Forward a binary frame to the session for processing
    Message(BinaryFrame),
}

/// Per-connection IPC sender: each session serializes frames independently
/// and sends complete byte vectors through this channel to a dedicated writer thread.
pub type IpcSender = crossbeam_channel::Sender<Vec<u8>>;

/// Internal entry for a running session
struct SessionEntry {
    /// Channel to send commands to the session thread
    tx: Sender<SessionCommand>,
    /// Connection that owns this session
    connection_id: u64,
    /// Thread join handle
    join_handle: Option<thread::JoinHandle<()>>,
}

/// Concurrency slot tracker shared across session threads
type SlotControl = Arc<(Mutex<usize>, Condvar)>;

/// Shared deferred message queue for non-BridgeResponse frames consumed by
/// sync bridge calls. The event loop drains these before blocking on the channel.
pub(crate) type DeferredQueue = Arc<Mutex<VecDeque<BinaryFrame>>>;

/// Create a new empty deferred queue.
pub(crate) fn new_deferred_queue() -> DeferredQueue {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Manages V8 sessions with concurrency limiting and connection binding.
///
/// Sessions are bound to the connection that created them. Other connections
/// cannot interact with a session they don't own. Each session runs on a
/// dedicated OS thread with its own V8 isolate.
pub struct SessionManager {
    sessions: HashMap<String, SessionEntry>,
    max_concurrency: usize,
    slot_control: SlotControl,
    /// Per-connection IPC sender — session threads clone this to send frames
    /// to the dedicated writer thread without shared mutex contention
    ipc_tx: IpcSender,
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
        ipc_tx: IpcSender,
        call_id_router: CallIdRouter,
        snapshot_cache: Arc<SnapshotCache>,
    ) -> Self {
        SessionManager {
            sessions: HashMap::new(),
            max_concurrency,
            slot_control: Arc::new((Mutex::new(0), Condvar::new())),
            ipc_tx,
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

    /// Create a new session bound to the given connection.
    /// Spawns a dedicated thread with a V8 isolate. If max concurrency is
    /// reached, the session thread will block until a slot becomes available.
    pub fn create_session(
        &mut self,
        session_id: String,
        connection_id: u64,
        heap_limit_mb: Option<u32>,
        cpu_time_limit_ms: Option<u32>,
    ) -> Result<(), String> {
        if self.sessions.contains_key(&session_id) {
            return Err(format!("session {} already exists", session_id));
        }

        let (tx, rx) = crossbeam_channel::bounded(256);
        let slot_control = Arc::clone(&self.slot_control);
        let max = self.max_concurrency;
        let ipc_tx = self.ipc_tx.clone();
        let router = Arc::clone(&self.call_id_router);
        let shared_call_id = Arc::clone(&self.shared_call_id);
        let snap_cache = Arc::clone(&self.snapshot_cache);

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
                    ipc_tx,
                    router,
                    shared_call_id,
                    snap_cache,
                );
            })
            .map_err(|e| format!("failed to spawn session thread: {}", e))?;

        self.sessions.insert(
            session_id,
            SessionEntry {
                tx,
                connection_id,
                join_handle: Some(join_handle),
            },
        );

        Ok(())
    }

    /// Destroy a session. Sends shutdown to the session thread and joins it.
    /// Returns an error if the session doesn't exist or belongs to another connection.
    pub fn destroy_session(&mut self, session_id: &str, connection_id: u64) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {} does not exist", session_id))?;

        if entry.connection_id != connection_id {
            return Err(format!(
                "session {} is not owned by this connection",
                session_id
            ));
        }

        // Send shutdown, drop the sender so the session thread's rx.recv()
        // returns Err if Shutdown was consumed by an inner loop, then join.
        let _ = entry.tx.send(SessionCommand::Shutdown);
        let mut entry = self.sessions.remove(session_id).unwrap();
        drop(entry.tx);
        if let Some(handle) = entry.join_handle.take() {
            let _ = handle.join();
        }

        Ok(())
    }

    /// Send a message to a session, verifying connection ownership.
    pub fn send_to_session(
        &self,
        session_id: &str,
        connection_id: u64,
        msg: BinaryFrame,
    ) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {} does not exist", session_id))?;

        if entry.connection_id != connection_id {
            return Err(format!(
                "session {} is not owned by this connection",
                session_id
            ));
        }

        entry
            .tx
            .send(SessionCommand::Message(msg))
            .map_err(|e| format!("session thread disconnected: {}", e))
    }

    /// Destroy all sessions belonging to a connection (called on disconnect).
    pub fn destroy_connection_sessions(&mut self, connection_id: u64) {
        let session_ids: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, entry)| entry.connection_id == connection_id)
            .map(|(id, _)| id.clone())
            .collect();

        for sid in session_ids {
            let _ = self.destroy_session(&sid, connection_id);
        }
    }

    /// Number of registered sessions (including those waiting for a slot).
    #[allow(dead_code)]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Return all session IDs with their owning connection IDs.
    #[allow(dead_code)]
    pub fn all_sessions(&self) -> Vec<(String, u64)> {
        self.sessions
            .iter()
            .map(|(id, entry)| (id.clone(), entry.connection_id))
            .collect()
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

/// Serialize and send a BinaryFrame via the per-connection IPC channel.
/// Uses a pre-allocated frame buffer to avoid per-call allocation.
/// No shared mutex is held — serialization happens on the session thread.
#[cfg(not(test))]
fn send_message(ipc_tx: &IpcSender, frame: &BinaryFrame, frame_buf: &mut Vec<u8>) {
    match ipc_binary::encode_frame_into(frame_buf, frame) {
        Ok(()) => {
            if let Err(e) = ipc_tx.send(frame_buf.clone()) {
                eprintln!("failed to send IPC message: {}", e);
            }
        }
        Err(e) => {
            eprintln!("failed to encode IPC message: {}", e);
        }
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
    #[cfg_attr(test, allow(unused_variables))] ipc_tx: IpcSender,
    #[cfg_attr(test, allow(unused_variables))] call_id_router: CallIdRouter,
    #[cfg_attr(test, allow(unused_variables))] shared_call_id: SharedCallIdCounter,
    #[cfg_attr(test, allow(unused_variables))] snapshot_cache: Arc<SnapshotCache>,
) {
    // Acquire concurrency slot (blocks if at capacity)
    {
        let (lock, cvar) = &*slot_control;
        let mut count = lock.lock().unwrap();
        while *count >= max_concurrency {
            count = cvar.wait(count).unwrap();
        }
        *count += 1;
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

    // Pre-allocated serialization buffers for V8 ValueSerializer output
    #[cfg(not(test))]
    let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());

    // Pre-allocated frame buffer for send_message (ExecutionResult etc.)
    #[cfg(not(test))]
    let mut msg_frame_buf: Vec<u8> = Vec::with_capacity(256);

    // Process commands until shutdown or channel close
    loop {
        match rx.recv() {
            Ok(SessionCommand::Shutdown) | Err(_) => break,
            Ok(SessionCommand::Message(_msg)) => {
                #[cfg(not(test))]
                match _msg {
                    BinaryFrame::InjectGlobals { payload, .. } => {
                        // Store V8-serialized config for injection into fresh context at Execute time
                        last_globals_payload = Some(payload);
                    }
                    BinaryFrame::Execute {
                        session_id,
                        bridge_code,
                        post_restore_script,
                        user_code,
                        mode,
                        file_path,
                    } => {
                        // Use cached bridge code when host sends empty (0-length = use cached)
                        let effective_bridge_code = if bridge_code.is_empty() {
                            last_bridge_code.as_deref().unwrap_or("").to_string()
                        } else {
                            last_bridge_code = Some(bridge_code.clone());
                            bridge_code
                        };

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
                                        isolate::create_isolate(heap_limit_mb)
                                    }
                                }
                            } else {
                                isolate::create_isolate(heap_limit_mb)
                            };
                            // Must re-apply WASM disable after every restore (not captured in snapshot)
                            execution::disable_wasm(&mut iso);
                            iso.set_host_import_module_dynamically_callback(
                                execution::dynamic_import_callback,
                            );
                            iso.set_host_initialize_import_meta_object_callback(
                                execution::import_meta_object_callback,
                            );
                            let ctx = isolate::create_context(&mut iso);
                            _v8_context = Some(ctx);
                            v8_isolate = Some(iso);
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

                        // Create abort channel for timeout enforcement
                        let (maybe_abort_tx, maybe_abort_rx) = if cpu_time_limit_ms.is_some() {
                            let (tx, rx) = crossbeam_channel::bounded::<()>(0);
                            (Some(tx), Some(rx))
                        } else {
                            (None, None)
                        };

                        // Create deferred queue for sync bridge call filtering
                        let deferred_queue = new_deferred_queue();

                        // Create BridgeCallContext with channel sender (no shared mutex)
                        let channel_rx = match maybe_abort_rx {
                            Some(ref arx) => ChannelResponseReceiver::with_abort(
                                rx.clone(),
                                arx.clone(),
                                Arc::clone(&deferred_queue),
                            ),
                            None => ChannelResponseReceiver::new(
                                rx.clone(),
                                Arc::clone(&deferred_queue),
                            ),
                        };
                        let bridge_ctx = BridgeCallContext::with_receiver(
                            Box::new(ChannelFrameSender::new(ipc_tx.clone())),
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
                                &SYNC_BRIDGE_FNS,
                                &ASYNC_BRIDGE_FNS,
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
                                let result_frame = BinaryFrame::ExecutionResult {
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
                                send_message(&ipc_tx, &result_frame, &mut msg_frame_buf);
                                continue;
                            }
                        }

                        // Start timeout guard before execution
                        let mut timeout_guard = match (cpu_time_limit_ms, maybe_abort_tx) {
                            (Some(ms), Some(abort_tx)) => {
                                let handle = iso.thread_safe_handle();
                                Some(crate::timeout::TimeoutGuard::new(ms, handle, abort_tx))
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
                            let (c, e) = execution::execute_script(
                                scope,
                                bridge_code_for_exec,
                                &user_code,
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
                        let should_enter_event_loop =
                            pending.len() > 0
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
                                    maybe_abort_rx.as_ref(),
                                    Some(&deferred_queue),
                                )
                            } else {
                                EventLoopStatus::Completed
                            };

                        let mut terminated = matches!(event_loop_status, EventLoopStatus::Terminated);
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

                        // For ESM modules: call _waitForActiveHandles() to keep
                        // the session alive while handles (timers, child processes,
                        // stdin listeners) are active. This creates a pending promise
                        // that the event loop pumps until all handles resolve.
                        if !terminated && mode != 0 && error.is_none() {
                            // Phase 1: call _waitForActiveHandles() to register a pending promise
                            {
                                let scope = &mut v8::HandleScope::new(iso);
                                let ctx = v8::Local::new(scope, &exec_context);
                                let scope = &mut v8::ContextScope::new(scope, ctx);
                                let global = ctx.global(scope);
                                let key = v8::String::new(scope, "_waitForActiveHandles").unwrap();
                                if let Some(func) = global.get(scope, key.into()) {
                                    if func.is_function() {
                                        let func = v8::Local::<v8::Function>::try_from(func).unwrap();
                                        let recv = v8::undefined(scope).into();
                                        if let Some(result) = func.call(scope, recv, &[]) {
                                            if result.is_promise() {
                                                let promise = v8::Local::<v8::Promise>::try_from(result).unwrap();
                                                if promise.state() == v8::PromiseState::Pending {
                                                    execution::set_pending_script_evaluation(scope, promise);
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
                                    maybe_abort_rx.as_ref(),
                                    Some(&deferred_queue),
                                );

                                if matches!(event_loop_status, EventLoopStatus::Terminated) {
                                    terminated = true;
                                }
                                if let EventLoopStatus::Failed(next_code, next_error) = event_loop_status {
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
                        let timed_out = timeout_guard.as_ref().is_some_and(|g| g.timed_out());

                        // Cancel timeout guard (joins timer thread)
                        if let Some(ref mut guard) = timeout_guard {
                            guard.cancel();
                        }
                        drop(timeout_guard);

                        // Send ExecutionResult
                        let result_frame = if timed_out {
                            BinaryFrame::ExecutionResult {
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
                            BinaryFrame::ExecutionResult {
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
                            BinaryFrame::ExecutionResult {
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

                        send_message(&ipc_tx, &result_frame, &mut msg_frame_buf);
                    }
                    _ => {
                        // Other messages handled in later stories
                    }
                }
            }
        }
    }

    // Drop V8 resources (only present in non-test mode)
    #[cfg(not(test))]
    {
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
pub(crate) const SYNC_BRIDGE_FNS: [&str; 34] = [
    // Console
    "_log",
    "_error",
    // Module loading (syncPromise — host resolves async, Rust blocks)
    "_resolveModule",
    "_loadFile",
    "_loadPolyfill",
    // Sync module loading (bypass _loadPolyfill dispatch, used by CJS require)
    "_resolveModuleSync",
    "_loadFileSync",
    // Crypto
    "_cryptoRandomFill",
    "_cryptoRandomUUID",
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
    // Child process (sync)
    "_childProcessSpawnStart",
    "_childProcessStdinWrite",
    "_childProcessStdinClose",
    "_childProcessKill",
    "_childProcessSpawnSync",
    "_networkHttpServerRespondRaw",
];

pub(crate) const ASYNC_BRIDGE_FNS: [&str; 12] = [
    // Module loading (async)
    "_dynamicImport",
    // Timer
    "_scheduleTimer",
    "_kernelStdinRead",
    // Network (async)
    "_networkFetchRaw",
    "_networkDnsLookupRaw",
    "_networkHttpRequestRaw",
    "_networkHttpServerListenRaw",
    "_networkHttpServerCloseRaw",
    "_networkHttpServerWaitRaw",
    "_networkHttp2ServerWaitRaw",
    "_networkHttp2SessionWaitRaw",
    "_netSocketWaitConnectRaw",
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
pub(crate) fn run_event_loop(
    scope: &mut v8::HandleScope,
    rx: &Receiver<SessionCommand>,
    pending: &crate::bridge::PendingPromises,
    abort_rx: Option<&crossbeam_channel::Receiver<()>>,
    deferred: Option<&DeferredQueue>,
) -> EventLoopStatus {
    while pending.len() > 0
        || execution::pending_module_evaluation_needs_wait(scope)
        || execution::pending_script_evaluation_needs_wait(scope)
        || deferred
            .map(|dq| !dq.lock().unwrap().is_empty())
            .unwrap_or(false)
    {
        // Drain deferred messages queued by sync bridge calls before blocking
        if let Some(dq) = deferred {
            let frames: Vec<BinaryFrame> = dq.lock().unwrap().drain(..).collect();
            for frame in frames {
                let status = dispatch_event_loop_frame(scope, frame, pending);
                if !matches!(status, EventLoopStatus::Completed) {
                    return status;
                }
            }
            if pending.len() == 0
                && !execution::pending_module_evaluation_needs_wait(scope)
                && !execution::pending_script_evaluation_needs_wait(scope)
            {
                break;
            }
        }

        // Flush microtasks before blocking. Run in a loop to drain the full
        // microtask queue -- each checkpoint may resolve Promises that schedule
        // new microtasks (e.g., async function await chains).
        for _ in 0..100 {
            scope.perform_microtask_checkpoint();
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
            && deferred.map(|dq| dq.lock().unwrap().is_empty()).unwrap_or(true)
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
            if let Some(dq) = deferred {
                if !dq.lock().unwrap().is_empty() {
                    // New deferred work appeared — drain it in the outer loop
                    let frames: Vec<BinaryFrame> = dq.lock().unwrap().drain(..).collect();
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
                && deferred.map(|dq| dq.lock().unwrap().is_empty()).unwrap_or(true)
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

/// Dispatch a single BinaryFrame within the event loop.
/// Returns the event-loop status after handling the frame.
pub(crate) enum EventLoopStatus {
    Completed,
    Terminated,
    Failed(i32, ExecutionError),
}

fn dispatch_event_loop_frame(
    scope: &mut v8::HandleScope,
    frame: BinaryFrame,
    pending: &crate::bridge::PendingPromises,
) -> EventLoopStatus {
    match frame {
        BinaryFrame::BridgeResponse {
            call_id,
            status,
            payload,
            ..
        } => {
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
        BinaryFrame::StreamEvent {
            event_type,
            payload,
            ..
        } => {
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
        BinaryFrame::TerminateExecution { .. } => {
            scope.terminate_execution();
            EventLoopStatus::Terminated
        }
        _ => {
            // Ignore other messages during event loop
            EventLoopStatus::Completed
        }
    }
}

/// ResponseReceiver that receives BinaryFrame directly from the session channel.
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
    pub(crate) fn new(rx: Receiver<SessionCommand>, deferred: DeferredQueue) -> Self {
        ChannelResponseReceiver {
            rx,
            abort_rx: None,
            deferred,
        }
    }

    #[allow(dead_code)]
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

impl crate::host_call::ResponseReceiver for ChannelResponseReceiver {
    fn recv_response(&self, expected_call_id: u64) -> Result<BinaryFrame, String> {
        loop {
            // Wait for next command, with optional abort monitoring
            let cmd = if let Some(ref abort) = self.abort_rx {
                crossbeam_channel::select! {
                    recv(self.rx) -> result => match result {
                        Ok(cmd) => cmd,
                        Err(_) => return Err("channel closed".into()),
                    },
                    recv(abort) -> _ => {
                        return Err("execution timed out".into());
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
                    if let BinaryFrame::BridgeResponse { call_id, .. } = &frame {
                        if *call_id == expected_call_id {
                            return Ok(frame);
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
    use std::collections::HashMap;

    /// Helper to create a SessionManager for tests
    fn test_manager(max: usize) -> SessionManager {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let router: CallIdRouter = Arc::new(Mutex::new(HashMap::new()));
        let snap_cache = Arc::new(SnapshotCache::new(4));
        SessionManager::new(max, tx, router, snap_cache)
    }

    #[test]
    fn session_management() {
        // Consolidated test to avoid V8 inter-test SIGSEGV issues.
        // Covers: lifecycle, connection binding, concurrency queuing, multi-connection.

        // --- Part 1: Single session create/destroy ---
        {
            let mut mgr = test_manager(4);

            mgr.create_session("session-aaa".into(), 1, None, None)
                .expect("create session A");
            assert_eq!(mgr.session_count(), 1);

            // Wait for thread to acquire slot and create isolate
            std::thread::sleep(std::time::Duration::from_millis(200));

            // Destroy session A
            mgr.destroy_session("session-aaa", 1)
                .expect("destroy session A");
            assert_eq!(mgr.session_count(), 0);
        }

        // --- Part 2: Multiple sessions + connection binding ---
        {
            let mut mgr = test_manager(4);

            mgr.create_session("session-bbb".into(), 1, None, None)
                .expect("create session B");
            mgr.create_session("session-ccc".into(), 1, Some(16), None)
                .expect("create session C");
            assert_eq!(mgr.session_count(), 2);

            std::thread::sleep(std::time::Duration::from_millis(200));

            // Duplicate session ID is rejected
            let err = mgr.create_session("session-bbb".into(), 1, None, None);
            assert!(err.is_err());
            assert!(err.unwrap_err().contains("already exists"));

            // Connection binding: connection 2 cannot destroy connection 1's session
            let err = mgr.destroy_session("session-bbb", 2);
            assert!(err.is_err());
            assert!(err.unwrap_err().contains("not owned"));

            // Connection binding: cannot send to another connection's session
            let err = mgr.send_to_session(
                "session-bbb",
                2,
                BinaryFrame::TerminateExecution {
                    session_id: "session-bbb".into(),
                },
            );
            assert!(err.is_err());
            assert!(err.unwrap_err().contains("not owned"));

            // Destroy non-existent session
            let err = mgr.destroy_session("no-such-session", 1);
            assert!(err.is_err());
            assert!(err.unwrap_err().contains("does not exist"));

            // Destroy remaining on disconnect
            mgr.destroy_connection_sessions(1);
            assert_eq!(mgr.session_count(), 0);
        }

        // --- Part 3: Max concurrency queuing ---
        {
            let mut mgr = test_manager(2);

            mgr.create_session("s1".into(), 1, None, None)
                .expect("create s1");
            mgr.create_session("s2".into(), 1, None, None)
                .expect("create s2");
            mgr.create_session("s3".into(), 1, None, None)
                .expect("create s3");

            // Allow threads to acquire slots
            std::thread::sleep(std::time::Duration::from_millis(300));

            // Only 2 slots active (s3 is queued)
            assert_eq!(mgr.active_slot_count(), 2);
            assert_eq!(mgr.session_count(), 3);

            // Destroy s1 — releases slot, s3 acquires it
            mgr.destroy_session("s1", 1).expect("destroy s1");
            std::thread::sleep(std::time::Duration::from_millis(300));
            assert_eq!(mgr.active_slot_count(), 2);
            assert_eq!(mgr.session_count(), 2);

            // Destroy remaining
            mgr.destroy_connection_sessions(1);
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert_eq!(mgr.session_count(), 0);
            assert_eq!(mgr.active_slot_count(), 0);
        }

        // --- Part 4: Multiple connections ---
        {
            let mut mgr = test_manager(4);

            mgr.create_session("conn1-s1".into(), 100, None, None)
                .expect("create");
            mgr.create_session("conn2-s1".into(), 200, None, None)
                .expect("create");

            std::thread::sleep(std::time::Duration::from_millis(200));

            // Connection 100 cannot touch connection 200's session
            let err = mgr.destroy_session("conn2-s1", 100);
            assert!(err.is_err());

            // destroy_connection_sessions only cleans up the given connection
            mgr.destroy_connection_sessions(100);
            assert_eq!(mgr.session_count(), 1);

            mgr.destroy_session("conn2-s1", 200).expect("destroy");
            assert_eq!(mgr.session_count(), 0);
        }
    }

    #[test]
    fn channel_response_receiver_filters_bridge_response() {
        use crate::host_call::ResponseReceiver;

        // Sync bridge call interleaved with StreamEvent does not drop the StreamEvent
        let (tx, rx) = crossbeam_channel::bounded(10);
        let deferred = new_deferred_queue();
        let receiver = ChannelResponseReceiver::new(rx, Arc::clone(&deferred));

        // Send: StreamEvent, TerminateExecution, then BridgeResponse
        tx.send(SessionCommand::Message(BinaryFrame::StreamEvent {
            session_id: "s1".into(),
            event_type: "child_stdout".into(),
            payload: vec![0x01, 0x02],
        }))
        .unwrap();
        tx.send(SessionCommand::Message(BinaryFrame::TerminateExecution {
            session_id: "s1".into(),
        }))
        .unwrap();
        tx.send(SessionCommand::Message(BinaryFrame::BridgeResponse {
            session_id: "s1".into(),
            call_id: 1,
            status: 0,
            payload: vec![0xAB],
        }))
        .unwrap();

        // recv_response should skip StreamEvent and TerminateExecution, return BridgeResponse
        let frame = receiver.recv_response(1).unwrap();
        assert!(
            matches!(&frame, BinaryFrame::BridgeResponse { call_id: 1, .. }),
            "expected BridgeResponse with call_id=1, got {:?}",
            frame
        );

        // Deferred queue should contain the StreamEvent and TerminateExecution
        let dq = deferred.lock().unwrap();
        assert_eq!(dq.len(), 2, "expected 2 deferred messages");
        assert!(
            matches!(&dq[0], BinaryFrame::StreamEvent { event_type, .. } if event_type == "child_stdout"),
            "first deferred should be StreamEvent"
        );
        assert!(
            matches!(&dq[1], BinaryFrame::TerminateExecution { .. }),
            "second deferred should be TerminateExecution"
        );
    }
}
