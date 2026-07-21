//! Per-execution Store state backed by AgentOS host capabilities.

use super::super::{guest_visible_wasm_env, StartWasmExecutionRequest, WasmExecutionEvent};
use super::diagnostics::ExecutionDiagnostics;
use super::engine::{WasmtimeEngineHandle, WasmtimeEngineProfile};
use super::lifecycle::QueuedWasmtimeEvent;
use super::limits;
use super::threads::ThreadGroup;
use super::worker::WorkerIpcClient;
use crate::backend::{
    direct_host_reply_channel, HostCallIdentity, HostCallReply, HostServiceError,
};
use crate::host::{HostOperation, HostProcessContext, ProcessHostCapabilitySet};
use agentos_runtime::accounting::{Reservation, ResourceClass, ResourceLedger};
use agentos_runtime::RuntimeContext;
use flume::Sender;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;
use wasmtime::{Store, StoreLimits, UpdateDeadline};

pub const DEFAULT_MAX_HOST_REPLY_BYTES: usize = 16 * 1024 * 1024;

pub struct PendingExecReplacement {
    pub module: Arc<wasmtime::Module>,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// One generation-bound direct waiter namespace shared by module loading and
/// every import issued by a Store. It owns no sidecar or kernel state.
#[derive(Clone)]
pub struct WasmtimeHostClient {
    process: HostProcessContext,
    host: Option<ProcessHostCapabilitySet>,
    worker: Option<WorkerIpcClient>,
    next_call_id: Arc<AtomicU64>,
    max_host_reply_bytes: usize,
    cancelled: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    signal_pending: Arc<AtomicBool>,
    resources: Arc<ResourceLedger>,
    events: Sender<QueuedWasmtimeEvent>,
    event_notify: Option<Arc<Notify>>,
    diagnostics: Option<Arc<ExecutionDiagnostics>>,
}

impl WasmtimeHostClient {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: ProcessHostCapabilitySet,
        max_host_reply_bytes: usize,
        cancelled: Arc<AtomicBool>,
        cancel_notify: Arc<Notify>,
        signal_pending: Arc<AtomicBool>,
        resources: Arc<ResourceLedger>,
        events: Sender<QueuedWasmtimeEvent>,
        event_notify: Option<Arc<Notify>>,
    ) -> Self {
        Self {
            process: host.process(),
            host: Some(host),
            worker: None,
            next_call_id: Arc::new(AtomicU64::new(1)),
            max_host_reply_bytes,
            cancelled,
            cancel_notify,
            signal_pending,
            resources,
            events,
            event_notify,
            diagnostics: None,
        }
    }

    pub(super) fn new_worker(
        worker: WorkerIpcClient,
        max_host_reply_bytes: usize,
        cancelled: Arc<AtomicBool>,
        cancel_notify: Arc<Notify>,
        signal_pending: Arc<AtomicBool>,
        resources: Arc<ResourceLedger>,
        events: Sender<QueuedWasmtimeEvent>,
    ) -> Self {
        Self {
            process: worker.process(),
            host: None,
            worker: Some(worker),
            next_call_id: Arc::new(AtomicU64::new(1)),
            max_host_reply_bytes,
            cancelled,
            cancel_notify,
            signal_pending,
            resources,
            events,
            event_notify: None,
            diagnostics: None,
        }
    }

    pub fn with_diagnostics(mut self, diagnostics: Arc<ExecutionDiagnostics>) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    pub fn process(&self) -> HostProcessContext {
        self.process
    }

    pub fn canceled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn signal_pending(&self) -> bool {
        self.worker
            .as_ref()
            .map(WorkerIpcClient::signal_pending)
            .unwrap_or_else(|| self.signal_pending.load(Ordering::Acquire))
    }

    pub async fn submit(
        &self,
        operation: HostOperation,
        retained_request_bytes: usize,
    ) -> Result<HostCallReply, HostServiceError> {
        if let Some(diagnostics) = self.diagnostics.as_ref() {
            diagnostics.first_host_call();
        }
        if self.canceled() {
            return Err(HostServiceError::new(
                "ECANCELED",
                "Wasmtime execution was canceled before host-call admission",
            ));
        }
        if let Some(worker) = self.worker.as_ref() {
            return worker.submit(operation).await;
        }
        let call_id = self
            .next_call_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map_err(|_| {
                HostServiceError::new(
                    "EOVERFLOW",
                    "Wasmtime host-call identity space is exhausted",
                )
            })?;
        let process = self.process;
        let identity = HostCallIdentity {
            generation: process.generation,
            pid: process.pid,
            call_id,
        };
        let host = self.host.as_ref().ok_or_else(|| {
            HostServiceError::new("EIO", "direct Wasmtime host capability is unavailable")
        })?;
        let admission = host.admit_request(retained_request_bytes)?;
        let (reply, receiver) = direct_host_reply_channel(identity, self.max_host_reply_bytes)?;
        host.submit(operation, reply, admission)?;
        tokio::select! {
            reply = receiver => reply,
            () = self.cancel_notify.notified() => Err(HostServiceError::new(
                "ECANCELED",
                "Wasmtime execution was canceled while awaiting a host operation",
            )),
        }
    }

    /// Route an owned native ABI request through the shared compatibility
    /// decoder and then the common typed host-operation dispatcher. The reply
    /// capability is direct and call-specific; this path never uses V8's
    /// synchronous line protocol or scans an event stream for a response.
    pub async fn submit_adapter_call(
        &self,
        method: String,
        args: Vec<Value>,
        raw_bytes_args: HashMap<usize, Vec<u8>>,
    ) -> Result<HostCallReply, HostServiceError> {
        if let Some(diagnostics) = self.diagnostics.as_ref() {
            diagnostics.first_host_call();
            diagnostics.first_guest_host_call();
            if matches!(method.as_str(), "__kernel_stdio_write" | "process.fd_write")
                && args
                    .first()
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|fd| matches!(fd, 1 | 2))
            {
                diagnostics.first_output();
            }
        }
        if self.canceled() {
            return Err(HostServiceError::new(
                "ECANCELED",
                "Wasmtime execution was canceled before adapter-call admission",
            ));
        }
        if let Some(worker) = self.worker.as_ref() {
            return worker
                .submit_adapter_call(method, args, raw_bytes_args)
                .await;
        }
        let call_id = self
            .next_call_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map_err(|_| {
                HostServiceError::new(
                    "EOVERFLOW",
                    "Wasmtime host-call identity space is exhausted",
                )
            })?;
        let process = self.process;
        let identity = HostCallIdentity {
            generation: process.generation,
            pid: process.pid,
            call_id,
        };
        let (reply, receiver) = direct_host_reply_channel(identity, self.max_host_reply_bytes)?;
        let request = crate::javascript::HostRpcRequest {
            id: call_id,
            method,
            args,
            raw_bytes_args,
        };
        let retained_event_bytes = request
            .method
            .len()
            .checked_add(
                serde_json::to_vec(&request.args)
                    .map_err(|error| {
                        HostServiceError::new(
                            "ERR_AGENTOS_WASMTIME_HOST_CALL_ENCODING",
                            error.to_string(),
                        )
                    })?
                    .len(),
            )
            .and_then(|total| {
                request
                    .raw_bytes_args
                    .values()
                    .try_fold(total, |total, bytes| total.checked_add(bytes.len()))
            })
            .ok_or_else(|| {
                HostServiceError::new(
                    "EOVERFLOW",
                    "Wasmtime host-call retained byte accounting overflowed",
                )
            })?;
        let event = QueuedWasmtimeEvent::new(
            &self.resources,
            Ok(WasmExecutionEvent::HostCall { request, reply }),
            retained_event_bytes,
        )?;
        tokio::select! {
            result = self.events.send_async(event) => result.map_err(|_| HostServiceError::new(
                "EPIPE",
                "Wasmtime host-call event receiver was dropped",
            ))?,
            () = self.cancel_notify.notified() => return Err(HostServiceError::new(
                "ECANCELED",
                "Wasmtime execution was canceled while admitting an adapter call",
            )),
        }
        if let Some(notify) = self.event_notify.as_ref() {
            notify.notify_one();
        }
        tokio::select! {
            reply = receiver => reply,
            () = self.cancel_notify.notified() => Err(HostServiceError::new(
                "ECANCELED",
                "Wasmtime execution was canceled while awaiting an adapter call",
            )),
        }
    }

    pub async fn publish_stderr(&self, bytes: Vec<u8>) -> Result<(), HostServiceError> {
        if let Some(worker) = self.worker.as_ref() {
            return worker.publish_stderr(bytes).await;
        }
        let retained_bytes = bytes.len();
        let event = QueuedWasmtimeEvent::new(
            &self.resources,
            Ok(WasmExecutionEvent::Stderr(bytes)),
            retained_bytes,
        )?;
        tokio::select! {
            result = self.events.send_async(event) => {
                result.map_err(|_| HostServiceError::new(
                    "EPIPE",
                    "Wasmtime stderr event receiver was dropped",
                ))?;
            }
            () = self.cancel_notify.notified() => return Err(HostServiceError::new(
                "ECANCELED",
                "Wasmtime execution was canceled while publishing stderr",
            )),
        }
        if let Some(notify) = self.event_notify.as_ref() {
            notify.notify_one();
        }
        Ok(())
    }

    pub(super) fn report_thread_group_failure(&self, error: HostServiceError) {
        if let Some(worker) = self.worker.as_ref() {
            if let Err(report_error) = worker.report_group_failure(error) {
                eprintln!(
                    "{}: failed to report pthread group failure to parent: {}",
                    report_error.code, report_error.message
                );
            }
        }
    }
}

pub struct WasmtimeStoreState {
    pub host: WasmtimeHostClient,
    pub engine: Arc<WasmtimeEngineHandle>,
    pub argv: Vec<Vec<u8>>,
    pub env: Vec<Vec<u8>>,
    pub virtual_pid: u32,
    pub virtual_ppid: u32,
    pub thread_id: i32,
    pub thread_group: Option<Arc<ThreadGroup>>,
    pub limits: StoreLimits,
    pub exit_code: Option<i32>,
    pub exec_replaced: bool,
    pub pending_exec_replacement: Option<PendingExecReplacement>,
    pub max_module_file_bytes: usize,
    pub max_blocking_read_ms: u64,
    pub max_spawn_file_actions: usize,
    pub max_spawn_file_action_bytes: usize,
    pub warned_fixed_limits: HashSet<&'static str>,
    pub pending_open_mode: Option<u32>,
    pub pending_open_direct: bool,
    active_cpu_limit_ns: Option<u64>,
    active_cpu_started_ns: u64,
    paused: Arc<AtomicBool>,
    pause_notify: Arc<Notify>,
    _async_stack_reservation: Option<Reservation>,
    _guest_memory_reservation: Option<Reservation>,
}

impl std::fmt::Debug for WasmtimeStoreState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmtimeStoreState")
            .field("process", &self.host.process())
            .field("argv_count", &self.argv.len())
            .field("env_count", &self.env.len())
            .field("exit_code", &self.exit_code)
            .finish_non_exhaustive()
    }
}

impl WasmtimeStoreState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime: &RuntimeContext,
        host: WasmtimeHostClient,
        engine: Arc<WasmtimeEngineHandle>,
        request: &StartWasmExecutionRequest,
        profile: WasmtimeEngineProfile,
        active_cpu_started_ns: u64,
        paused: Arc<AtomicBool>,
        pause_notify: Arc<Notify>,
        group_memory_pre_reserved: bool,
        thread_group: Option<Arc<ThreadGroup>>,
        thread_id: i32,
    ) -> Result<Self, HostServiceError> {
        let async_stack_bytes = profile.async_stack_bytes()?;
        let async_stack_reservation = if group_memory_pre_reserved {
            None
        } else {
            Some(
                runtime
                    .resources()
                    .reserve(ResourceClass::WasmMemoryBytes, async_stack_bytes)
                    .map_err(|error| {
                        HostServiceError::new(
                            "ERR_AGENTOS_WASMTIME_ASYNC_STACK_LIMIT",
                            error.to_string(),
                        )
                        .with_details(serde_json::json!({
                            "limitName": "limits.resources.maxWasmStackBytes",
                            "observed": async_stack_bytes,
                        }))
                    })?,
            )
        };
        let linear_memory_bytes = limits::max_memory_bytes(&request.limits)?;
        let aggregate_memory_bytes = limits::aggregate_store_memory_bytes(&request.limits)?;
        let guest_memory_reservation = if group_memory_pre_reserved {
            None
        } else {
            Some(
                runtime
                    .resources()
                    .reserve(ResourceClass::WasmMemoryBytes, aggregate_memory_bytes)
                    .map_err(|error| {
                        HostServiceError::new(
                            "ERR_AGENTOS_WASMTIME_AGGREGATE_MEMORY_LIMIT",
                            error.to_string(),
                        )
                        .with_details(serde_json::json!({
                            "limitName": "limits.resources.maxWasmMemoryBytes",
                            "observed": aggregate_memory_bytes,
                            "linearMemoryBytes": linear_memory_bytes,
                            "exceptionGcHeapBytes": linear_memory_bytes,
                            "tableAccountingBytes": limits::DEFAULT_TABLE_ACCOUNTING_BYTES,
                            "resource": "wasmtimeGuestMemory",
                        }))
                    })?,
            )
        };
        let argv = nul_terminated_strings(request.argv.iter().map(String::as_str), "argv")?;
        let guest_env = guest_visible_wasm_env(&request.env);
        let env = nul_terminated_strings(
            guest_env
                .iter()
                .map(|(key, value)| format!("{key}={value}")),
            "environment",
        )?;
        let virtual_pid = request
            .guest_runtime
            .virtual_pid
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_else(|| host.process().pid);
        let virtual_ppid = request
            .guest_runtime
            .virtual_ppid
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_default();
        Ok(Self {
            host,
            engine,
            argv,
            env,
            virtual_pid,
            virtual_ppid,
            thread_id,
            thread_group,
            limits: limits::store_limits(&request.limits)?,
            exit_code: None,
            exec_replaced: false,
            pending_exec_replacement: None,
            max_module_file_bytes: request
                .limits
                .max_module_file_bytes
                .map(usize::try_from)
                .transpose()
                .map_err(|_| {
                    HostServiceError::new("EFBIG", "module byte limit does not fit this platform")
                })?
                .unwrap_or(256 * 1024 * 1024),
            max_blocking_read_ms: request.limits.max_blocking_read_ms.unwrap_or(30_000),
            max_spawn_file_actions: request
                .limits
                .max_spawn_file_actions
                .map(usize::try_from)
                .transpose()
                .map_err(|_| {
                    HostServiceError::new(
                        "E2BIG",
                        "spawn file-action count limit does not fit this platform",
                    )
                })?
                .unwrap_or(4096),
            max_spawn_file_action_bytes: request
                .limits
                .max_spawn_file_action_bytes
                .map(usize::try_from)
                .transpose()
                .map_err(|_| {
                    HostServiceError::new(
                        "E2BIG",
                        "spawn file-action byte limit does not fit this platform",
                    )
                })?
                .unwrap_or(1024 * 1024),
            warned_fixed_limits: HashSet::new(),
            pending_open_mode: None,
            pending_open_direct: false,
            active_cpu_limit_ns: request
                .limits
                .active_cpu_time_limit_ms
                .map(|value| u64::from(value).saturating_mul(1_000_000)),
            active_cpu_started_ns,
            paused,
            pause_notify,
            _async_stack_reservation: async_stack_reservation,
            _guest_memory_reservation: guest_memory_reservation,
        })
    }

    pub fn canceled(&self) -> bool {
        self.host.canceled()
    }

    fn active_cpu_exhausted(&self) -> bool {
        self.active_cpu_limit_ns.is_some_and(|limit| {
            thread_cpu_time_ns().saturating_sub(self.active_cpu_started_ns) >= limit
        })
    }

    fn pause_waiter(&self) -> Option<(Arc<AtomicBool>, Arc<Notify>)> {
        self.paused
            .load(Ordering::Acquire)
            .then(|| (Arc::clone(&self.paused), Arc::clone(&self.pause_notify)))
    }
}

pub fn max_host_reply_bytes(
    request: &StartWasmExecutionRequest,
) -> Result<usize, HostServiceError> {
    request
        .limits
        .max_sync_rpc_response_line_bytes
        .map(usize::try_from)
        .transpose()
        .map_err(|_| {
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_LIMIT_CONFIG",
                "limits.reactor.maxBridgeResponseBytes does not fit this platform",
            )
        })
        .map(|value| value.unwrap_or(DEFAULT_MAX_HOST_REPLY_BYTES))
}

#[allow(clippy::too_many_arguments)]
pub fn create_store(
    engine: Arc<WasmtimeEngineHandle>,
    runtime: &RuntimeContext,
    host: WasmtimeHostClient,
    request: &StartWasmExecutionRequest,
    profile: WasmtimeEngineProfile,
    active_cpu_started_ns: u64,
    paused: Arc<AtomicBool>,
    pause_notify: Arc<Notify>,
    group_memory_pre_reserved: bool,
    thread_group: Option<Arc<ThreadGroup>>,
    thread_id: i32,
) -> Result<Store<WasmtimeStoreState>, HostServiceError> {
    let deterministic_fuel = request.limits.deterministic_fuel;
    let mut store = Store::new(
        engine.engine(),
        WasmtimeStoreState::new(
            runtime,
            host,
            Arc::clone(&engine),
            request,
            profile,
            active_cpu_started_ns,
            paused,
            pause_notify,
            group_memory_pre_reserved,
            thread_group,
            thread_id,
        )?,
    );
    store.limiter(|state| &mut state.limits);
    // Fuel instrumentation is enabled on every Engine so the optional policy
    // is Store-local. A Store with no requested deterministic budget receives
    // effectively unbounded fuel while active CPU time remains epoch-managed.
    store
        .set_fuel(deterministic_fuel.unwrap_or(u64::MAX))
        .map_err(|error| {
            eprintln!("ERR_AGENTOS_WASMTIME_FUEL_CONFIG: private Store diagnostic: {error:#}");
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_FUEL_CONFIG",
                "failed to configure the deterministic execution budget",
            )
        })?;
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(|context| {
        if context.data().canceled() {
            return Err(wasmtime::format_err!(
                "ERR_AGENTOS_WASMTIME_CANCELED: execution canceled"
            ));
        }
        if context.data().active_cpu_exhausted() {
            return Err(wasmtime::format_err!(
                "ERR_AGENTOS_WASMTIME_ACTIVE_CPU_LIMIT: active CPU budget exhausted"
            ));
        }
        if let Some((paused, notify)) = context.data().pause_waiter() {
            return Ok(UpdateDeadline::YieldCustom(
                1,
                Box::pin(async move {
                    loop {
                        let notified = notify.notified();
                        if !paused.load(Ordering::Acquire) {
                            break;
                        }
                        notified.await;
                    }
                }),
            ));
        }
        Ok(UpdateDeadline::Yield(1))
    });
    Ok(store)
}

#[cfg(unix)]
pub(super) fn thread_cpu_time_ns() -> u64 {
    let value = match nix::time::clock_gettime(nix::time::ClockId::CLOCK_THREAD_CPUTIME_ID) {
        Ok(value) => value,
        Err(error) => {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_CPU_CLOCK: failed to read guest executor CPU clock: {error}"
            );
            return 0;
        }
    };
    u64::try_from(value.tv_sec())
        .unwrap_or_default()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(value.tv_nsec()).unwrap_or_default())
}

#[cfg(not(unix))]
pub(super) fn thread_cpu_time_ns() -> u64 {
    0
}

fn nul_terminated_strings<I, S>(
    values: I,
    field: &'static str,
) -> Result<Vec<Vec<u8>>, HostServiceError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    values
        .into_iter()
        .map(|value| {
            let value = value.as_ref();
            if value.as_bytes().contains(&0) {
                return Err(HostServiceError::new(
                    "EINVAL",
                    format!("WebAssembly {field} value contains an interior NUL"),
                ));
            }
            let mut bytes = Vec::with_capacity(value.len().saturating_add(1));
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
            Ok(bytes)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{bounded_execution_event_channel, PayloadLimit};
    use crate::wasm::{GuestRuntimeConfig, WasmExecutionLimits, WasmPermissionTier};
    use agentos_runtime::accounting::{ResourceLedger, ResourceLimit};
    use agentos_runtime::{RuntimeConfig, SidecarRuntime};
    use std::path::PathBuf;

    #[test]
    fn store_reservations_are_released_on_teardown() {
        let runtime = SidecarRuntime::process(&RuntimeConfig::default()).expect("test runtime");
        let process = crate::host::HostProcessContext {
            generation: 97,
            pid: 41,
        };
        let resources = Arc::new(ResourceLedger::child(
            "wasmtime-store-teardown-test",
            [(
                ResourceClass::WasmMemoryBytes,
                ResourceLimit::new(1024 * 1024 * 1024, "runtime.resources.maxWasmMemoryBytes"),
            )],
            Arc::clone(runtime.context().resources()),
        ));
        let scoped = runtime
            .context()
            .scoped_for_vm(Arc::clone(&resources), process.generation);
        let (submission, _host_events) = bounded_execution_event_channel(
            process,
            4,
            PayloadLimit::new("limits.process.pendingEventBytes", 1024).expect("event byte limit"),
            Arc::new(|| {}),
        )
        .expect("host event channel");
        let (events, _event_receiver) = flume::bounded(4);
        let host = WasmtimeHostClient::new(
            ProcessHostCapabilitySet::from_event_submission(submission),
            DEFAULT_MAX_HOST_REPLY_BYTES,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Notify::new()),
            Arc::new(AtomicBool::new(false)),
            Arc::clone(&resources),
            events,
            None,
        );
        let request = StartWasmExecutionRequest {
            vm_id: String::from("vm-store-teardown"),
            context_id: String::from("ctx-store-teardown"),
            managed_kernel_host: true,
            argv: vec![String::from("/test.wasm")],
            env: BTreeMap::new(),
            cwd: PathBuf::from("/"),
            permission_tier: WasmPermissionTier::Full,
            limits: WasmExecutionLimits::default(),
            guest_runtime: GuestRuntimeConfig::default(),
        };
        let profile =
            WasmtimeEngineProfile::new(request.limits.max_stack_bytes).expect("engine profile");
        let engine = super::super::engine::WasmtimeEngineRegistry::new(1)
            .get_or_create(profile)
            .expect("engine");
        let expected = profile
            .async_stack_bytes()
            .expect("async stack")
            .saturating_add(
                limits::aggregate_store_memory_bytes(&request.limits)
                    .expect("aggregate store bytes"),
            );

        let state = WasmtimeStoreState::new(
            &scoped,
            host,
            engine,
            &request,
            profile,
            thread_cpu_time_ns(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Notify::new()),
            false,
            None,
            0,
        )
        .expect("store state");
        assert_eq!(
            resources.usage(ResourceClass::WasmMemoryBytes).used,
            expected
        );
        drop(state);
        assert_eq!(resources.usage(ResourceClass::WasmMemoryBytes).used, 0);
        assert!(resources.integrity_ok());
    }
}
