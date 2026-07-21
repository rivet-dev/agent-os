//! Execution lifecycle, cancellation, interruption, and teardown.

use super::super::{StartWasmExecutionRequest, WasmExecutionError, WasmExecutionEvent};
use super::diagnostics::ExecutionDiagnostics;
use super::engine::{
    WasmtimeEngineHandle, WasmtimeEngineProfile, WasmtimeEngineRegistry, WasmtimeMetricsSnapshot,
};
use super::limits;
use super::linker;
use super::store::{self, WasmtimeHostClient};
use crate::backend::{
    ExecutionWakeIdentity, HostCallReply, HostServiceError, PayloadLimit, PublishedSignalCheckpoint,
};
use crate::host::{
    BoundedString, BoundedUsize, ExecutableImageSource, FilesystemOperation, HostOperation,
    ProcessHostCapabilitySet, ProcessOperation,
};
use agentos_runtime::accounting::{Reservation, ResourceClass, ResourceLedger};
use agentos_runtime::RuntimeContext;
use base64::Engine as _;
use flume::{Receiver, Sender};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

const MODULE_READ_CHUNK_BYTES: usize = 512 * 1024;
const DEFAULT_MAX_MODULE_FILE_BYTES: usize = 256 * 1024 * 1024;

/// One queued executor event plus the ledger ownership for bytes retained by
/// that queue slot. The reservation is released when the event leaves the
/// Wasmtime queue; downstream output/host-call paths apply their own existing
/// retention accounting after that handoff.
pub(super) struct QueuedWasmtimeEvent {
    event: Result<WasmExecutionEvent, WasmExecutionError>,
    _retained_bytes: Option<Reservation>,
}

impl QueuedWasmtimeEvent {
    pub(super) fn new(
        resources: &Arc<ResourceLedger>,
        event: Result<WasmExecutionEvent, WasmExecutionError>,
        retained_bytes: usize,
    ) -> Result<Self, HostServiceError> {
        let retained_bytes = if retained_bytes == 0 {
            None
        } else {
            Some(
                resources
                    .reserve(ResourceClass::AsyncCompletionBytes, retained_bytes)
                    .map_err(|error| {
                        HostServiceError::new(
                            "ERR_AGENTOS_WASMTIME_EVENT_BYTES_LIMIT",
                            error.to_string(),
                        )
                        .with_details(serde_json::json!({
                            "limitName": "limits.reactor.maxAsyncCompletionBytes",
                            "observed": retained_bytes,
                        }))
                    })?,
            )
        };
        Ok(Self {
            event,
            _retained_bytes: retained_bytes,
        })
    }

    fn into_event(self) -> Result<WasmExecutionEvent, WasmExecutionError> {
        self.event
    }
}

#[derive(Debug, Default)]
struct HostLatch {
    value: Mutex<Option<ProcessHostCapabilitySet>>,
    notify: Notify,
}

#[derive(Debug)]
struct Control {
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    pause_notify: Arc<Notify>,
    cancel_notify: Arc<Notify>,
    started: AtomicBool,
    start_notify: Notify,
    engine: Mutex<Option<Arc<WasmtimeEngineHandle>>>,
    signal_checkpoints: Mutex<VecDeque<(ExecutionWakeIdentity, PublishedSignalCheckpoint)>>,
    max_signal_checkpoints: usize,
    signal_pending: Arc<AtomicBool>,
}

impl Control {
    fn new(started: bool, max_signal_checkpoints: usize) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            pause_notify: Arc::new(Notify::new()),
            cancel_notify: Arc::new(Notify::new()),
            started: AtomicBool::new(started),
            start_notify: Notify::new(),
            engine: Mutex::new(None),
            signal_checkpoints: Mutex::new(VecDeque::new()),
            max_signal_checkpoints: max_signal_checkpoints.max(1),
            signal_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    fn publish_signal(
        &self,
        identity: ExecutionWakeIdentity,
        delivery: PublishedSignalCheckpoint,
    ) -> Result<(), HostServiceError> {
        let mut checkpoints = self.signal_checkpoints.lock().map_err(|_| {
            HostServiceError::new(
                "EIO",
                "ERR_AGENTOS_WASMTIME_SIGNAL_INBOX_POISONED: signal checkpoint state is poisoned",
            )
        })?;
        if checkpoints.len() >= self.max_signal_checkpoints {
            return Err(HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_SIGNAL_CHECKPOINT_LIMIT",
                "Wasmtime signal checkpoint inbox is full",
            )
            .with_details(serde_json::json!({
                "limitName": "limits.process.pendingEventCount",
                "limit": self.max_signal_checkpoints,
            })));
        }
        if checkpoints.len().saturating_add(1) * 5 >= self.max_signal_checkpoints * 4 {
            eprintln!(
                "WARN_AGENTOS_WASMTIME_SIGNAL_CHECKPOINT_LIMIT: signal checkpoint inbox is at {}/{} entries",
                checkpoints.len().saturating_add(1),
                self.max_signal_checkpoints
            );
        }
        checkpoints.push_back((identity, delivery));
        self.signal_pending.store(true, Ordering::Release);
        Ok(())
    }

    fn take_signal(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        let mut checkpoints = self.signal_checkpoints.lock().map_err(|_| {
            HostServiceError::new(
                "EIO",
                "ERR_AGENTOS_WASMTIME_SIGNAL_INBOX_POISONED: signal checkpoint state is poisoned",
            )
        })?;
        let Some((pending_identity, _)) = checkpoints.front() else {
            return Ok(None);
        };
        if *pending_identity != identity {
            return Err(HostServiceError::new(
                "ESTALE",
                "published signal delivery identity does not match the active Wasmtime execution",
            ));
        }
        let delivery = checkpoints.pop_front().map(|(_, delivery)| delivery);
        self.signal_pending
            .store(!checkpoints.is_empty(), Ordering::Release);
        Ok(delivery)
    }

    fn discard_signals(&self, identity: ExecutionWakeIdentity) -> Result<(), HostServiceError> {
        let mut checkpoints = self.signal_checkpoints.lock().map_err(|_| {
            HostServiceError::new(
                "EIO",
                "ERR_AGENTOS_WASMTIME_SIGNAL_INBOX_POISONED: signal checkpoint state is poisoned",
            )
        })?;
        checkpoints.retain(|(pending_identity, _)| *pending_identity != identity);
        self.signal_pending
            .store(!checkpoints.is_empty(), Ordering::Release);
        Ok(())
    }

    fn interrupt(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.started.store(true, Ordering::Release);
        self.start_notify.notify_waiters();
        self.pause_notify.notify_waiters();
        self.cancel_notify.notify_waiters();
        if let Ok(engine) = self.engine.lock() {
            if let Some(engine) = engine.as_ref() {
                engine.engine().increment_epoch();
            }
        }
    }
}

pub struct WasmtimeExecution {
    execution_id: String,
    events: Receiver<QueuedWasmtimeEvent>,
    host: Arc<HostLatch>,
    control: Arc<Control>,
    worker_done: Receiver<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
    teardown_timeout: Duration,
    prepared: bool,
}

impl std::fmt::Debug for WasmtimeExecution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmtimeExecution")
            .field("execution_id", &self.execution_id)
            .field("prepared", &self.prepared)
            .field("cancelled", &self.control.cancelled.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl WasmtimeExecution {
    pub fn spawn(
        execution_id: String,
        module_path: String,
        request: StartWasmExecutionRequest,
        runtime: RuntimeContext,
        event_notify: Option<Arc<Notify>>,
        defer_execute: bool,
    ) -> Result<Self, WasmExecutionError> {
        let permit = runtime
            .vm_executor_admission()
            .try_acquire()
            .map_err(|error| {
                WasmExecutionError::Host(
                    HostServiceError::new("ERR_AGENTOS_VM_EXECUTOR_LIMIT", error.to_string())
                        .with_details(serde_json::json!({
                            "limitName": "runtime.maxActiveVmExecutors",
                            "limit": runtime.max_active_vm_executors(),
                        })),
                )
            })?;
        let pending_count = request.limits.pending_event_count.unwrap_or(64).max(1);
        let (event_sender, events) = flume::bounded(pending_count);
        let (done_sender, worker_done) = flume::bounded(1);
        let host = Arc::new(HostLatch::default());
        let control = Arc::new(Control::new(
            !defer_execute,
            request.limits.pending_event_count.unwrap_or(64),
        ));
        let worker_host = Arc::clone(&host);
        let worker_control = Arc::clone(&control);
        let worker_runtime = runtime.clone();
        // AGENTOS_THREAD_SITE: admitted-wasmtime-guest-executor
        let worker = std::thread::Builder::new()
            .name(format!("agentos-wasmtime-{execution_id}"))
            .spawn(move || {
                let _permit = permit;
                let event_resources = Arc::clone(worker_runtime.resources());
                let result = worker_runtime.handle().block_on(run_execution(
                    module_path,
                    request,
                    worker_runtime.clone(),
                    Arc::clone(&worker_host),
                    Arc::clone(&worker_control),
                    event_sender.clone(),
                    event_notify.clone(),
                ));
                publish_worker_result(
                    &event_sender,
                    event_notify.as_ref(),
                    &event_resources,
                    result,
                );
                if done_sender.send(()).is_err() {
                    eprintln!(
                        "ERR_AGENTOS_WASMTIME_TEARDOWN_CHANNEL: worker completion receiver was dropped"
                    );
                }
            })
            .map_err(WasmExecutionError::Spawn)?;
        Ok(Self {
            execution_id,
            events,
            host,
            control,
            worker_done,
            worker: Mutex::new(Some(worker)),
            teardown_timeout: runtime.vm_executor_teardown_timeout(),
            prepared: defer_execute,
        })
    }

    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn configure_host_services(&self, host: ProcessHostCapabilitySet) {
        match self.host.value.lock() {
            Ok(mut slot) if slot.is_none() => {
                *slot = Some(host);
                self.host.notify.notify_waiters();
            }
            Ok(_) => eprintln!(
                "ERR_AGENTOS_WASMTIME_HOST_ALREADY_BOUND: ignored duplicate host capability binding"
            ),
            Err(_) => eprintln!(
                "ERR_AGENTOS_WASMTIME_HOST_LATCH_POISONED: failed to bind host capabilities"
            ),
        }
    }

    pub fn is_prepared_for_start(&self) -> bool {
        self.prepared && !self.control.started.load(Ordering::Acquire)
    }

    pub fn start_prepared(&mut self) -> Result<(), WasmExecutionError> {
        if !self.prepared {
            return Err(WasmExecutionError::Host(HostServiceError::new(
                "ERR_AGENTOS_EXECUTION_NOT_PREPARED",
                "Wasmtime execution was not created as a prepared image",
            )));
        }
        if self.control.started.swap(true, Ordering::AcqRel) {
            return Err(WasmExecutionError::Host(HostServiceError::new(
                "EALREADY",
                "prepared Wasmtime execution has already started",
            )));
        }
        self.prepared = false;
        self.control.start_notify.notify_waiters();
        Ok(())
    }

    pub fn terminate(&self) {
        self.control.interrupt();
        self.host.notify.notify_waiters();
    }

    pub fn set_paused(&self, paused: bool) {
        self.control.paused.store(paused, Ordering::Release);
        if !paused {
            self.control.pause_notify.notify_waiters();
        }
        if let Ok(engine) = self.control.engine.lock() {
            if let Some(engine) = engine.as_ref() {
                engine.engine().increment_epoch();
            }
        }
    }

    pub fn deliver_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
        signal: i32,
        delivery_token: u64,
        flags: u32,
    ) -> Result<(), HostServiceError> {
        self.control.publish_signal(
            identity,
            PublishedSignalCheckpoint {
                signal,
                delivery_token,
                flags,
            },
        )
    }

    pub fn take_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        self.control.take_signal(identity)
    }

    pub fn discard_signal_checkpoints(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<(), HostServiceError> {
        self.control.discard_signals(identity)
    }

    pub async fn poll_event(
        &self,
        timeout: Duration,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        match tokio::time::timeout(timeout, self.events.recv_async()).await {
            Ok(Ok(event)) => event.into_event().map(Some),
            Ok(Err(_)) => Err(WasmExecutionError::EventChannelClosed),
            Err(_) => Ok(None),
        }
    }

    pub fn try_poll_event(&self) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        match self.events.try_recv() {
            Ok(event) => event.into_event().map(Some),
            Err(flume::TryRecvError::Empty) => Ok(None),
            Err(flume::TryRecvError::Disconnected) => Err(WasmExecutionError::EventChannelClosed),
        }
    }

    pub fn poll_event_blocking(
        &self,
        timeout: Duration,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => event.into_event().map(Some),
            Err(flume::RecvTimeoutError::Timeout) => Ok(None),
            Err(flume::RecvTimeoutError::Disconnected) => {
                Err(WasmExecutionError::EventChannelClosed)
            }
        }
    }

    pub fn next_event_blocking(&self) -> Result<WasmExecutionEvent, WasmExecutionError> {
        self.events
            .recv()
            .map_err(|_| WasmExecutionError::EventChannelClosed)?
            .into_event()
    }

    fn join_worker(&self) {
        let handle = match self.worker.lock() {
            Ok(mut worker) => worker.take(),
            Err(_) => {
                eprintln!(
                    "ERR_AGENTOS_WASMTIME_WORKER_LOCK_POISONED: unable to join guest executor"
                );
                None
            }
        };
        let Some(handle) = handle else {
            return;
        };
        match self.worker_done.recv_timeout(self.teardown_timeout) {
            Ok(()) => {
                if handle.join().is_err() {
                    eprintln!("ERR_AGENTOS_WASMTIME_WORKER_PANIC: guest executor panicked");
                }
            }
            Err(error) => eprintln!(
                "ERR_AGENTOS_WASMTIME_TEARDOWN_TIMEOUT: guest executor did not stop within {} ms: {error}",
                self.teardown_timeout.as_millis()
            ),
        }
    }
}

impl Drop for WasmtimeExecution {
    fn drop(&mut self) {
        self.terminate();
        self.join_worker();
    }
}

pub struct WasmtimeExecutionEngine;

impl WasmtimeExecutionEngine {
    pub fn metrics() -> Result<WasmtimeMetricsSnapshot, HostServiceError> {
        WasmtimeEngineRegistry::process().metrics()
    }
}

async fn run_execution(
    module_path: String,
    request: StartWasmExecutionRequest,
    runtime: RuntimeContext,
    host_latch: Arc<HostLatch>,
    control: Arc<Control>,
    event_sender: Sender<QueuedWasmtimeEvent>,
    event_notify: Option<Arc<Notify>>,
) -> Result<i32, HostServiceError> {
    let diagnostics = Arc::new(ExecutionDiagnostics::new(
        request
            .env
            .get("AGENTOS_WASM_WARMUP_DEBUG")
            .is_some_and(|value| value == "1"),
    ));
    wait_until_started(&control).await?;
    let host = wait_for_host(&host_latch, &control.cancelled).await?;
    let host = WasmtimeHostClient::new(
        host,
        store::max_host_reply_bytes(&request)?,
        Arc::clone(&control.cancelled),
        Arc::clone(&control.cancel_notify),
        Arc::clone(&control.signal_pending),
        Arc::clone(runtime.resources()),
        event_sender,
        event_notify,
    )
    .with_diagnostics(Arc::clone(&diagnostics));
    let engine_started = Instant::now();
    let profile = WasmtimeEngineProfile::new(request.limits.max_stack_bytes)?;
    let engine = WasmtimeEngineRegistry::process().get_or_create(profile)?;
    diagnostics.phase("Engine", engine_started.elapsed());
    control
        .engine
        .lock()
        .map_err(|_| {
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_CONTROL_POISONED",
                "Wasmtime Engine control slot is poisoned",
            )
        })?
        .replace(Arc::clone(&engine));

    let future = run_loaded_module(
        module_path.clone(),
        request.clone(),
        runtime,
        host.clone(),
        engine,
        profile,
        Arc::clone(&control.paused),
        Arc::clone(&control.pause_notify),
        Arc::clone(&diagnostics),
    );
    let result = if let Some(limit_ms) = request.limits.wall_clock_limit_ms {
        match tokio::time::timeout(Duration::from_millis(limit_ms), future).await {
            Ok(result) => result,
            Err(_) => {
                control.interrupt();
                Err(HostServiceError::new(
                    "ERR_AGENTOS_WASMTIME_WALL_CLOCK_LIMIT",
                    "Wasmtime execution exceeded its wall-clock budget",
                )
                .with_details(serde_json::json!({
                    "limitName": "limits.resources.maxWasmWallClockTimeMs",
                    "limit": limit_ms,
                })))
            }
        }
    } else {
        future.await
    };
    if diagnostics.enabled() {
        let reason = if result.is_ok() {
            "completed"
        } else {
            "failed"
        };
        if let Some(line) = diagnostics.line(reason, &module_path) {
            if let Err(error) = host.publish_stderr(line).await {
                eprintln!(
                    "ERR_AGENTOS_WASMTIME_DIAGNOSTICS_PUBLISH: failed to publish phase diagnostics: {}: {}",
                    error.code, error.message
                );
            }
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_loaded_module(
    module_path: String,
    request: StartWasmExecutionRequest,
    runtime: RuntimeContext,
    host: WasmtimeHostClient,
    engine: Arc<WasmtimeEngineHandle>,
    profile: WasmtimeEngineProfile,
    paused: Arc<AtomicBool>,
    pause_notify: Arc<Notify>,
    diagnostics: Arc<ExecutionDiagnostics>,
) -> Result<i32, HostServiceError> {
    // The kernel owns the authoritative WASI capability roots and descriptor
    // numbers. Initialize them before guest code starts so both executors see
    // the same fd namespace without maintaining a Wasmtime-local projection.
    let preopens_started = Instant::now();
    host.submit(
        HostOperation::Filesystem(FilesystemOperation::CanonicalPreopens),
        0,
    )
    .await?;
    diagnostics.phase("canonicalPreopens", preopens_started.elapsed());
    let module_read_started = Instant::now();
    let bytes = load_module(&host, &module_path, &request).await?;
    diagnostics.phase("moduleRead", module_read_started.elapsed());
    let compiled = super::module::compile_module(&engine, &bytes)?;
    diagnostics.phase("profileValidation", compiled.profile_validation);
    diagnostics.phase("moduleCompile", compiled.compilation);
    diagnostics.module(bytes.len(), compiled.cache_hit);
    let mut module = compiled.module;
    let mut request = request;
    let import_validation_started = Instant::now();
    linker::validate_module_imports(&module, request.permission_tier)?;
    diagnostics.phase("importValidation", import_validation_started.elapsed());
    let linker_started = Instant::now();
    let linker =
        linker::build_linker(engine.engine(), request.permission_tier).map_err(|error| {
            eprintln!("ERR_AGENTOS_WASMTIME_LINKER: private linker diagnostic: {error:#}");
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_LINKER",
                "failed to build the AgentOS WebAssembly host linker",
            )
        })?;
    diagnostics.phase("Linker", linker_started.elapsed());
    // One process image may replace itself repeatedly with fexecve. Preserve
    // the same active-CPU origin across Stores so exec cannot reset its budget.
    let active_cpu_started_ns = store::thread_cpu_time_ns();
    loop {
        let store_started = Instant::now();
        let mut store = store::create_store(
            Arc::clone(&engine),
            &runtime,
            host.clone(),
            &request,
            profile,
            active_cpu_started_ns,
            Arc::clone(&paused),
            Arc::clone(&pause_notify),
        )?;
        diagnostics.phase("Store", store_started.elapsed());
        let async_stack_bytes = profile.async_stack_bytes()?;
        let reserved_store_bytes = async_stack_bytes
            .saturating_add(limits::aggregate_store_memory_bytes(&request.limits)?);
        let instantiate_started = Instant::now();
        let instance = linker
            .instantiate_async(&mut store, &module)
            .await
            .map_err(|error| {
                super::error::normalize("ERR_AGENTOS_WASMTIME_INSTANTIATE", &error, false)
            })?;
        diagnostics.phase("Instance", instantiate_started.elapsed());
        let signal_started = Instant::now();
        linker::initialize_inherited_signal_mask(&mut store, &instance).await?;
        diagnostics.phase("signalMaskInit", signal_started.elapsed());
        let entrypoint_started = Instant::now();
        let start = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|error| {
                eprintln!(
                    "ERR_AGENTOS_WASMTIME_ENTRYPOINT: private entrypoint diagnostic: {error:#}"
                );
                HostServiceError::new(
                    "ERR_AGENTOS_WASMTIME_ENTRYPOINT",
                    "WebAssembly module does not export a valid _start function",
                )
            })?;
        diagnostics.phase("entrypointLookup", entrypoint_started.elapsed());
        diagnostics.store_memory(
            guest_linear_memory_bytes(&instance, &mut store),
            async_stack_bytes,
            reserved_store_bytes,
        );
        let call_started = Instant::now();
        let call_result = start.call_async(&mut store, ()).await;
        diagnostics.phase("wasi.start", call_started.elapsed());
        diagnostics.store_memory(
            guest_linear_memory_bytes(&instance, &mut store),
            async_stack_bytes,
            reserved_store_bytes,
        );
        match call_result {
            Ok(()) => {
                let exit_code = store.data().exit_code.unwrap_or(0);
                let teardown_started = Instant::now();
                drop(start);
                drop(store);
                diagnostics.phase("Store.teardown", teardown_started.elapsed());
                return Ok(exit_code);
            }
            Err(error) => {
                if let Some(exit_code) = store.data().exit_code {
                    let teardown_started = Instant::now();
                    drop(start);
                    drop(store);
                    diagnostics.phase("Store.teardown", teardown_started.elapsed());
                    return Ok(exit_code);
                }
                if let Some(replacement) = store.data_mut().pending_exec_replacement.take() {
                    request.argv = replacement.argv;
                    request.env = replacement.env;
                    module = replacement.module;
                    linker::validate_module_imports(&module, request.permission_tier)?;
                    let teardown_started = Instant::now();
                    drop(start);
                    drop(store);
                    diagnostics.phase("Store.teardown", teardown_started.elapsed());
                    continue;
                }
                if store.data().exec_replaced {
                    let teardown_started = Instant::now();
                    drop(start);
                    drop(store);
                    diagnostics.phase("Store.teardown", teardown_started.elapsed());
                    return Err(HostServiceError::new(
                        "ERR_AGENTOS_EXEC_REPLACED",
                        "the kernel committed a replacement process image",
                    ));
                }
                let normalized = super::error::normalize(
                    "ERR_AGENTOS_WASMTIME_TRAP",
                    &error,
                    store.data().canceled(),
                );
                let teardown_started = Instant::now();
                drop(start);
                drop(store);
                diagnostics.phase("Store.teardown", teardown_started.elapsed());
                return Err(normalized);
            }
        }
    }
}

fn guest_linear_memory_bytes(
    instance: &wasmtime::Instance,
    store: &mut wasmtime::Store<store::WasmtimeStoreState>,
) -> usize {
    instance
        .get_memory(&mut *store, "memory")
        .map_or(0, |memory| memory.data_size(&*store))
}

async fn wait_until_started(control: &Control) -> Result<(), HostServiceError> {
    loop {
        let notified = control.start_notify.notified();
        if control.cancelled.load(Ordering::Acquire) {
            return Err(HostServiceError::new(
                "ECANCELED",
                "Wasmtime execution was canceled before start",
            ));
        }
        if control.started.load(Ordering::Acquire) {
            return Ok(());
        }
        notified.await;
    }
}

async fn wait_for_host(
    latch: &HostLatch,
    cancelled: &AtomicBool,
) -> Result<ProcessHostCapabilitySet, HostServiceError> {
    loop {
        let notified = latch.notify.notified();
        let host = latch
            .value
            .lock()
            .map_err(|_| {
                HostServiceError::new(
                    "ERR_AGENTOS_WASMTIME_HOST_LATCH_POISONED",
                    "Wasmtime host capability latch is poisoned",
                )
            })?
            .clone();
        if let Some(host) = host {
            return Ok(host);
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(HostServiceError::new(
                "ECANCELED",
                "Wasmtime execution was canceled before host binding",
            ));
        }
        notified.await;
    }
}

async fn load_module(
    host: &WasmtimeHostClient,
    module_path: &str,
    request: &StartWasmExecutionRequest,
) -> Result<Vec<u8>, HostServiceError> {
    let path_limit = PayloadLimit::new("runtime.filesystem.maxPathBytes", 4096)?;
    let source = if let Some(path) = module_path.strip_prefix(super::TRUSTED_INITIAL_MODULE_PREFIX)
    {
        ExecutableImageSource::TrustedInitialPath(BoundedString::try_new(
            path.to_owned(),
            &path_limit,
        )?)
    } else {
        ExecutableImageSource::Path(BoundedString::try_new(module_path.to_owned(), &path_limit)?)
    };
    let maximum = request
        .limits
        .max_module_file_bytes
        .map(usize::try_from)
        .transpose()
        .map_err(|_| HostServiceError::new("EFBIG", "module byte limit does not fit usize"))?
        .unwrap_or(DEFAULT_MAX_MODULE_FILE_BYTES);
    load_executable_image(host, source, maximum).await
}

pub(super) async fn load_executable_image(
    host: &WasmtimeHostClient,
    source: ExecutableImageSource,
    maximum: usize,
) -> Result<Vec<u8>, HostServiceError> {
    let retained_request_bytes = match &source {
        ExecutableImageSource::TrustedInitialPath(path) | ExecutableImageSource::Path(path) => {
            path.as_str().len()
        }
        ExecutableImageSource::Descriptor(_) => std::mem::size_of::<u32>(),
    };
    let open = host
        .submit(
            HostOperation::Process(ProcessOperation::OpenExecutableImage { source }),
            retained_request_bytes,
        )
        .await?;
    let (handle, size) = decode_open_image(open)?;
    let result = read_module_image(host, handle, size, maximum).await;
    let close = host
        .submit(
            HostOperation::Process(ProcessOperation::CloseExecutableImage { handle }),
            std::mem::size_of::<u64>(),
        )
        .await;
    match (result, close) {
        (Ok(bytes), Ok(_)) => Ok(bytes),
        (Err(error), Ok(_)) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(close_error)) => {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_IMAGE_CLOSE: primary module-load error {}; image-close error {}",
                error, close_error
            );
            Err(error)
        }
    }
}

fn decode_open_image(reply: HostCallReply) -> Result<(u64, usize), HostServiceError> {
    let HostCallReply::Json(value) = reply else {
        return Err(HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_IMAGE_REPLY",
            "executable-image open returned a non-JSON reply",
        ));
    };
    let handle = value
        .get("handle")
        .and_then(Value::as_str)
        .ok_or_else(|| HostServiceError::new("EIO", "image reply is missing handle"))?
        .parse::<u64>()
        .map_err(|_| HostServiceError::new("EIO", "image handle is not a u64"))?;
    let size = value
        .get("size")
        .and_then(Value::as_u64)
        .and_then(|size| usize::try_from(size).ok())
        .ok_or_else(|| HostServiceError::new("EFBIG", "image size does not fit this platform"))?;
    Ok((handle, size))
}

async fn read_module_image(
    host: &WasmtimeHostClient,
    handle: u64,
    size: usize,
    maximum: usize,
) -> Result<Vec<u8>, HostServiceError> {
    if size > maximum {
        return Err(HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_MODULE_FILE_LIMIT",
            "WebAssembly module exceeds the configured executable-image limit",
        )
        .with_details(serde_json::json!({
            "limitName": "limits.resources.maxWasmModuleFileBytes",
            "limit": maximum,
            "observed": size,
        })));
    }
    let read_limit =
        PayloadLimit::new("limits.wasm.moduleReadChunkBytes", MODULE_READ_CHUNK_BYTES)?;
    let mut bytes = Vec::with_capacity(size);
    while bytes.len() < size {
        let requested = (size - bytes.len()).min(MODULE_READ_CHUNK_BYTES);
        let reply = host
            .submit(
                HostOperation::Process(ProcessOperation::ReadExecutableImage {
                    handle,
                    offset: bytes.len() as u64,
                    max_bytes: BoundedUsize::try_new(requested, &read_limit)?,
                }),
                std::mem::size_of::<u64>() * 2 + std::mem::size_of::<usize>(),
            )
            .await?;
        let chunk = decode_image_bytes(reply)?;
        if chunk.is_empty() {
            return Err(HostServiceError::new(
                "EIO",
                "executable-image read returned EOF before its declared size",
            ));
        }
        if chunk.len() > requested || bytes.len().saturating_add(chunk.len()) > size {
            return Err(HostServiceError::new(
                "EIO",
                "executable-image read exceeded its requested or declared size",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn decode_image_bytes(reply: HostCallReply) -> Result<Vec<u8>, HostServiceError> {
    match reply {
        HostCallReply::Raw(bytes) => Ok(bytes),
        HostCallReply::Json(value) => {
            let encoded = value
                .get("base64")
                .and_then(Value::as_str)
                .filter(|_| value.get("__agentOSType").and_then(Value::as_str) == Some("bytes"))
                .ok_or_else(|| {
                    HostServiceError::new("EIO", "image read returned invalid encoded bytes")
                })?;
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| HostServiceError::new("EIO", "image read returned invalid base64"))
        }
        HostCallReply::Empty => Err(HostServiceError::new(
            "EIO",
            "image read returned an empty reply envelope",
        )),
    }
}

fn publish_worker_result(
    sender: &Sender<QueuedWasmtimeEvent>,
    notify: Option<&Arc<Notify>>,
    resources: &Arc<ResourceLedger>,
    result: Result<i32, HostServiceError>,
) {
    match result {
        Ok(code) => publish_worker_event(
            sender,
            notify,
            resources,
            Ok(WasmExecutionEvent::Exited(code)),
            0,
        ),
        Err(error) if error.code == "ERR_AGENTOS_EXEC_REPLACED" => {}
        Err(error) => {
            let message = format!("{}: {}\n", error.code, error.message);
            let message_bytes = message.into_bytes();
            let retained_bytes = message_bytes.len();
            publish_worker_event(
                sender,
                notify,
                resources,
                Ok(WasmExecutionEvent::Stderr(message_bytes)),
                retained_bytes,
            );
            publish_worker_event(
                sender,
                notify,
                resources,
                Ok(WasmExecutionEvent::Exited(1)),
                0,
            );
        }
    }
}

fn publish_worker_event(
    sender: &Sender<QueuedWasmtimeEvent>,
    notify: Option<&Arc<Notify>>,
    resources: &Arc<ResourceLedger>,
    event: Result<WasmExecutionEvent, WasmExecutionError>,
    retained_bytes: usize,
) {
    let event = match QueuedWasmtimeEvent::new(resources, event, retained_bytes) {
        Ok(event) => event,
        Err(error) => {
            eprintln!("{}: {}", error.code, error.message);
            return;
        }
    };
    if sender.send(event).is_err() {
        eprintln!("ERR_AGENTOS_WASMTIME_EVENT_CHANNEL: execution event receiver was dropped");
    } else if let Some(notify) = notify {
        notify.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{bounded_execution_event_channel, ExecutionEvent};
    use crate::wasm::{GuestRuntimeConfig, WasmExecutionLimits, WasmPermissionTier};
    use agentos_runtime::accounting::{ResourceLedger, ResourceLimit};
    use agentos_runtime::{RuntimeConfig, SidecarRuntime};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn queued_event_bytes_are_bounded_and_released_on_handoff() {
        let resources = Arc::new(ResourceLedger::root(
            "wasmtime-event-test",
            [(
                ResourceClass::AsyncCompletionBytes,
                ResourceLimit::new(4, "limits.reactor.maxAsyncCompletionBytes"),
            )],
        ));
        let event =
            QueuedWasmtimeEvent::new(&resources, Ok(WasmExecutionEvent::Stderr(vec![0; 4])), 4)
                .expect("admit exact-bound event");
        assert_eq!(resources.usage(ResourceClass::AsyncCompletionBytes).used, 4);
        let error =
            QueuedWasmtimeEvent::new(&resources, Ok(WasmExecutionEvent::Stderr(vec![0])), 1)
                .err()
                .expect("over-bound event must fail");
        assert_eq!(error.code, "ERR_AGENTOS_WASMTIME_EVENT_BYTES_LIMIT");
        drop(event);
        assert_eq!(resources.usage(ResourceClass::AsyncCompletionBytes).used, 0);
    }

    #[test]
    fn executes_kernel_supplied_module_without_v8_or_ambient_wasi() {
        let runtime = SidecarRuntime::process(&RuntimeConfig::default())
            .expect("test runtime")
            .context();
        let module = wat::parse_str("(module (func (export \"_start\")))").expect("test module");
        let request = StartWasmExecutionRequest {
            vm_id: String::from("vm-test"),
            context_id: String::from("ctx-test"),
            managed_kernel_host: true,
            argv: vec![String::from("/test.wasm")],
            env: BTreeMap::new(),
            cwd: PathBuf::from("/"),
            permission_tier: WasmPermissionTier::Full,
            limits: WasmExecutionLimits::default(),
            guest_runtime: GuestRuntimeConfig::default(),
        };
        let process = crate::host::HostProcessContext {
            generation: 7,
            pid: 42,
        };
        let (submission, host_events) = bounded_execution_event_channel(
            process,
            8,
            PayloadLimit::new("limits.process.pendingEventBytes", 1024 * 1024)
                .expect("event byte limit"),
            Arc::new(|| {}),
        )
        .expect("host event channel");
        let module_for_host = module.clone();
        let host_worker = std::thread::spawn(move || {
            let mut completed = 0;
            while completed < 5 {
                let Some(event) = host_events.try_recv().expect("host event poll") else {
                    std::thread::yield_now();
                    continue;
                };
                let ExecutionEvent::HostCall { operation, reply } = event else {
                    panic!("unexpected non-host event");
                };
                match operation {
                    HostOperation::Filesystem(FilesystemOperation::CanonicalPreopens) => reply
                        .succeed_json(Value::Null)
                        .expect("canonical-preopens reply"),
                    HostOperation::Process(ProcessOperation::OpenExecutableImage { .. }) => reply
                        .succeed_json(serde_json::json!({
                            "handle": "1",
                            "size": module_for_host.len(),
                        }))
                        .expect("open reply"),
                    HostOperation::Process(ProcessOperation::ReadExecutableImage {
                        handle,
                        offset,
                        max_bytes,
                    }) => {
                        assert_eq!(handle, 1);
                        let start = usize::try_from(offset).expect("test image offset");
                        let end = start
                            .saturating_add(max_bytes.get())
                            .min(module_for_host.len());
                        reply
                            .succeed_raw(module_for_host[start..end].to_vec())
                            .expect("read reply");
                    }
                    HostOperation::Process(ProcessOperation::CloseExecutableImage { handle }) => {
                        assert_eq!(handle, 1);
                        reply.succeed_json(Value::Null).expect("close reply");
                    }
                    HostOperation::Signal(crate::host::SignalOperation::UpdateMask { .. }) => {
                        reply
                            .succeed_json(serde_json::json!({ "signals": [] }))
                            .expect("signal-mask reply");
                    }
                    operation => panic!("unexpected host operation: {operation:?}"),
                }
                completed += 1;
            }
        });
        let execution = WasmtimeExecution::spawn(
            String::from("exec-test"),
            String::from("/test.wasm"),
            request,
            runtime,
            None,
            false,
        )
        .expect("spawn executor");
        execution
            .configure_host_services(ProcessHostCapabilitySet::from_event_submission(submission));
        assert_eq!(
            execution
                .poll_event_blocking(Duration::from_secs(10))
                .expect("execution event"),
            Some(WasmExecutionEvent::Exited(0))
        );
        host_worker.join().expect("host worker");
    }

    #[test]
    fn native_preview1_import_uses_owned_direct_waiter_event() {
        let runtime = SidecarRuntime::process(&RuntimeConfig::default())
            .expect("test runtime")
            .context();
        let module = wat::parse_str(
            r#"(module
                (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 32) "hello")
                (func (export "_start")
                    (i32.store (i32.const 8) (i32.const 32))
                    (i32.store (i32.const 12) (i32.const 5))
                    (drop (call $fd_write
                        (i32.const 1) (i32.const 8) (i32.const 1) (i32.const 16)))))"#,
        )
        .expect("test module");
        let request = StartWasmExecutionRequest {
            vm_id: String::from("vm-test"),
            context_id: String::from("ctx-test"),
            managed_kernel_host: true,
            argv: vec![String::from("/test.wasm")],
            env: BTreeMap::new(),
            cwd: PathBuf::from("/"),
            permission_tier: WasmPermissionTier::Full,
            limits: WasmExecutionLimits::default(),
            guest_runtime: GuestRuntimeConfig::default(),
        };
        let process = crate::host::HostProcessContext {
            generation: 8,
            pid: 43,
        };
        let (submission, host_events) = bounded_execution_event_channel(
            process,
            8,
            PayloadLimit::new("limits.process.pendingEventBytes", 1024 * 1024)
                .expect("event byte limit"),
            Arc::new(|| {}),
        )
        .expect("host event channel");
        let module_for_host = module.clone();
        let host_worker = std::thread::spawn(move || {
            let mut completed = 0;
            while completed < 5 {
                let Some(event) = host_events.try_recv().expect("host event poll") else {
                    std::thread::yield_now();
                    continue;
                };
                let ExecutionEvent::HostCall { operation, reply } = event else {
                    panic!("unexpected non-host event");
                };
                match operation {
                    HostOperation::Filesystem(FilesystemOperation::CanonicalPreopens) => reply
                        .succeed_json(Value::Null)
                        .expect("canonical-preopens reply"),
                    HostOperation::Process(ProcessOperation::OpenExecutableImage { .. }) => reply
                        .succeed_json(serde_json::json!({
                            "handle": "1",
                            "size": module_for_host.len(),
                        }))
                        .expect("open reply"),
                    HostOperation::Process(ProcessOperation::ReadExecutableImage {
                        offset,
                        max_bytes,
                        ..
                    }) => {
                        let start = offset as usize;
                        let end = start
                            .saturating_add(max_bytes.get())
                            .min(module_for_host.len());
                        reply
                            .succeed_raw(module_for_host[start..end].to_vec())
                            .expect("read reply");
                    }
                    HostOperation::Process(ProcessOperation::CloseExecutableImage { .. }) => {
                        reply.succeed_json(Value::Null).expect("close reply");
                    }
                    HostOperation::Signal(crate::host::SignalOperation::UpdateMask { .. }) => {
                        reply
                            .succeed_json(serde_json::json!({ "signals": [] }))
                            .expect("signal-mask reply");
                    }
                    operation => panic!("unexpected host operation: {operation:?}"),
                }
                completed += 1;
            }
        });
        let execution = WasmtimeExecution::spawn(
            String::from("exec-import-test"),
            String::from("/test.wasm"),
            request,
            runtime,
            None,
            false,
        )
        .expect("spawn executor");
        execution
            .configure_host_services(ProcessHostCapabilitySet::from_event_submission(submission));
        let event = execution
            .poll_event_blocking(Duration::from_secs(10))
            .expect("host-call event")
            .expect("host-call event present");
        let WasmExecutionEvent::HostCall { request, reply } = event else {
            panic!("unexpected execution event: {event:?}");
        };
        assert_eq!(request.method, "__kernel_stdio_write");
        assert_eq!(request.raw_bytes_args.get(&1), Some(&b"hello".to_vec()));
        reply
            .succeed_json(serde_json::json!(5))
            .expect("write reply");
        assert_eq!(
            execution
                .poll_event_blocking(Duration::from_secs(10))
                .expect("exit event"),
            Some(WasmExecutionEvent::Exited(0))
        );
        host_worker.join().expect("host worker");
    }

    #[test]
    fn caught_signal_runs_exact_trampoline_at_async_import_boundary() {
        let runtime = SidecarRuntime::process(&RuntimeConfig::default())
            .expect("test runtime")
            .context();
        let module = wat::parse_str(
            r#"(module
                (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 32) "h")
                (func (export "__wasi_signal_trampoline") (param i32)
                    (i32.store8 (i32.const 32) (i32.const 115)))
                (func (export "_start")
                    (i32.store (i32.const 8) (i32.const 32))
                    (i32.store (i32.const 12) (i32.const 1))
                    (drop (call $fd_write
                        (i32.const 1) (i32.const 8) (i32.const 1) (i32.const 16)))))"#,
        )
        .expect("signal test module");
        let request = StartWasmExecutionRequest {
            vm_id: String::from("vm-signal"),
            context_id: String::from("ctx-signal"),
            managed_kernel_host: true,
            argv: vec![String::from("/signal.wasm")],
            env: BTreeMap::new(),
            cwd: PathBuf::from("/"),
            permission_tier: WasmPermissionTier::Full,
            limits: WasmExecutionLimits::default(),
            guest_runtime: GuestRuntimeConfig::default(),
        };
        let identity = ExecutionWakeIdentity {
            generation: 11,
            pid: 51,
        };
        let process = crate::host::HostProcessContext {
            generation: identity.generation,
            pid: identity.pid,
        };
        let (submission, host_events) = bounded_execution_event_channel(
            process,
            8,
            PayloadLimit::new("limits.process.pendingEventBytes", 1024 * 1024)
                .expect("event byte limit"),
            Arc::new(|| {}),
        )
        .expect("host event channel");
        let execution_slot = Arc::new(std::sync::OnceLock::<Arc<WasmtimeExecution>>::new());
        let worker_slot = Arc::clone(&execution_slot);
        let module_for_host = module.clone();
        let host_worker = std::thread::spawn(move || {
            let mut completed = 0;
            while completed < 7 {
                let Some(event) = host_events.try_recv().expect("host event poll") else {
                    std::thread::yield_now();
                    continue;
                };
                let ExecutionEvent::HostCall { operation, reply } = event else {
                    panic!("unexpected non-host event");
                };
                match operation {
                    HostOperation::Filesystem(FilesystemOperation::CanonicalPreopens) => reply
                        .succeed_json(Value::Null)
                        .expect("canonical-preopens reply"),
                    HostOperation::Process(ProcessOperation::OpenExecutableImage { .. }) => reply
                        .succeed_json(serde_json::json!({
                            "handle": "1",
                            "size": module_for_host.len(),
                        }))
                        .expect("open reply"),
                    HostOperation::Process(ProcessOperation::ReadExecutableImage {
                        offset,
                        max_bytes,
                        ..
                    }) => {
                        let start = offset as usize;
                        let end = start
                            .saturating_add(max_bytes.get())
                            .min(module_for_host.len());
                        reply
                            .succeed_raw(module_for_host[start..end].to_vec())
                            .expect("read reply");
                    }
                    HostOperation::Process(ProcessOperation::CloseExecutableImage { .. }) => {
                        reply.succeed_json(Value::Null).expect("close reply");
                    }
                    HostOperation::Signal(crate::host::SignalOperation::UpdateMask { .. }) => {
                        let execution = loop {
                            if let Some(execution) = worker_slot.get() {
                                break execution;
                            }
                            std::thread::yield_now();
                        };
                        execution
                            .deliver_signal_checkpoint(identity, 10, 99, 0)
                            .expect("publish signal");
                        reply
                            .succeed_json(serde_json::json!({ "signals": [] }))
                            .expect("signal-mask reply");
                    }
                    HostOperation::Signal(crate::host::SignalOperation::TakePublishedDelivery) => {
                        let delivery = worker_slot
                            .get()
                            .expect("execution installed")
                            .take_signal_checkpoint(identity)
                            .expect("take signal")
                            .expect("published signal");
                        reply
                            .succeed_json(serde_json::json!({
                                "signal": delivery.signal,
                                "token": delivery.delivery_token,
                                "flags": delivery.flags,
                            }))
                            .expect("take reply");
                    }
                    HostOperation::Signal(crate::host::SignalOperation::EndDelivery { token }) => {
                        assert_eq!(token, 99);
                        reply.succeed_json(Value::Null).expect("signal-end reply");
                    }
                    operation => panic!("unexpected host operation: {operation:?}"),
                }
                completed += 1;
            }
        });
        let execution = Arc::new(
            WasmtimeExecution::spawn(
                String::from("exec-signal-test"),
                String::from("/signal.wasm"),
                request,
                runtime,
                None,
                false,
            )
            .expect("spawn executor"),
        );
        execution_slot
            .set(Arc::clone(&execution))
            .expect("install execution");
        execution
            .configure_host_services(ProcessHostCapabilitySet::from_event_submission(submission));
        let event = execution
            .poll_event_blocking(Duration::from_secs(10))
            .expect("write event")
            .expect("write event present");
        let WasmExecutionEvent::HostCall { request, reply } = event else {
            panic!("unexpected execution event: {event:?}");
        };
        assert_eq!(request.method, "__kernel_stdio_write");
        assert_eq!(request.raw_bytes_args.get(&1), Some(&b"s".to_vec()));
        reply
            .succeed_json(serde_json::json!(1))
            .expect("write reply");
        assert_eq!(
            execution
                .poll_event_blocking(Duration::from_secs(10))
                .expect("exit event"),
            Some(WasmExecutionEvent::Exited(0))
        );
        host_worker.join().expect("host worker");
    }
}
