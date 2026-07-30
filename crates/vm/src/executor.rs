//! Native executor composition and dispatch.
//!
//! Concrete executors remain independent crates. The sidecar owns the
//! only enum that selects between them because selection is composition policy,
//! not part of the runtime-neutral executor contract.

use agentos_driver_tokio::DriverHandle;
use agentos_executor_contract::backend::{
    DescendantOutputOwnership, DescendantWaitOwnership, ExecutionBackend, ExecutionBackendKind,
    ExecutionExit, ExecutionWakeHandle, ExecutionWakeIdentity, HostServiceError,
    PublishedSignalCheckpoint, ShutdownOutcome, ShutdownReason, SignalCheckpointOutcome,
    SynchronousFdWritePolicy,
};
use agentos_executor_contract::host::ProcessHostCapabilitySet;
#[cfg(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
))]
use agentos_executor_v8_runtime::adapter_host::V8SessionHandle;
#[cfg(feature = "wasm-v8")]
use agentos_executor_wasm_v8::{WasmV8Execution, WasmV8ExecutionEngine};
#[cfg(feature = "wasm-wasmtime")]
use agentos_executor_wasm_wasmtime::{WasmtimeExecution, WasmtimeExecutionEngine};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

pub use agentos_executor_contract::{backend, host, GuestRuntimeConfig, HostRpcRequest};
pub use agentos_executor_contract::{
    ExecutionSignalDispositionAction, ExecutionSignalHandlerRegistration,
};
#[cfg(feature = "python-v8-pyodide")]
pub use agentos_executor_python_v8_pyodide::*;
#[cfg(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
))]
pub use agentos_executor_v8_runtime::adapter_host as v8_host;
#[cfg(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
))]
pub use agentos_executor_v8_runtime::adapter_runtime as v8_runtime;
#[cfg(feature = "node-v8")]
pub use agentos_executor_v8_runtime::asset_cache::bundled_typescript_assets;
#[cfg(feature = "wasm-v8")]
pub use agentos_executor_wasm_v8 as wasm;
#[cfg(not(feature = "node-v8"))]
pub fn bundled_typescript_assets() -> &'static [(&'static str, &'static [u8])] {
    &[]
}
#[cfg(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
))]
pub use agentos_executor_v8_runtime::bridge::EMULATED_OPENSSL_VERSION;
#[cfg(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
))]
pub use agentos_executor_v8_runtime::execution::GuestModuleReader;
#[cfg(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
))]
pub use agentos_executor_v8_runtime::javascript;
#[cfg(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
))]
pub use agentos_executor_v8_runtime::javascript::JavascriptSyncRpcResponder;
#[cfg(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
))]
pub use agentos_executor_v8_runtime::javascript::*;
pub use agentos_executor_wasm_abi::abi;
pub use agentos_executor_wasm_abi::{
    detect_native_binary_format, CreateWasmContextRequest, NativeBinaryFormat,
    StandaloneWasmBackend, StartWasmExecutionRequest, WasmContext, WasmExecutionError,
    WasmExecutionEvent, WasmExecutionLimits, WasmExecutionResult, WasmPermissionTier,
    WasmtimeMetricsSnapshot,
};

pub const TRUSTED_INITIAL_MODULE_PREFIX: &str = "agentos-trusted-initial:";
#[cfg(not(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
)))]
#[derive(Debug, Clone)]
pub struct JavascriptSyncRpcResponder;

#[cfg(not(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
)))]
impl agentos_executor_contract::backend::DirectHostReplyTarget for JavascriptSyncRpcResponder {
    fn claim(&self, _call_id: u64) -> Result<bool, HostServiceError> {
        Err(HostServiceError::new(
            "ERR_AGENTOS_EXECUTOR_NOT_COMPILED",
            "the V8 compatibility reply lane is not compiled into this sidecar",
        ))
    }

    fn respond(
        &self,
        _call_id: u64,
        _claimed: bool,
        _result: Result<agentos_executor_contract::backend::HostCallReply, HostServiceError>,
    ) -> Result<(), HostServiceError> {
        Err(HostServiceError::new(
            "ERR_AGENTOS_EXECUTOR_NOT_COMPILED",
            "the V8 compatibility reply lane is not compiled into this sidecar",
        ))
    }
}

#[cfg(not(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
)))]
pub fn record_sync_bridge_request_observed(_id: u64, _method: &str) {}

#[cfg(not(any(
    feature = "node-v8",
    feature = "python-v8-pyodide",
    feature = "wasm-v8"
)))]
#[derive(Debug, Clone)]
pub struct V8SessionHandle;

#[derive(Debug)]
pub enum WasmExecution {
    #[cfg(feature = "wasm-v8")]
    V8(Box<WasmV8Execution>),
    #[cfg(feature = "wasm-wasmtime")]
    Wasmtime(WasmtimeExecution),
    #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
    Disabled(std::convert::Infallible),
}

impl WasmExecution {
    pub fn standalone_backend(&self) -> StandaloneWasmBackend {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(_) => StandaloneWasmBackend::V8,
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) if execution.is_threaded() => {
                StandaloneWasmBackend::WasmtimeThreads
            }
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => StandaloneWasmBackend::Wasmtime,
        }
    }

    pub fn sync_rpc_responder(&self) -> Option<JavascriptSyncRpcResponder> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => Some(execution.sync_rpc_responder()),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => None,
        }
    }

    pub fn execution_id(&self) -> &str {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.execution_id(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => execution.execution_id(),
        }
    }

    pub fn native_process_id(&self) -> Option<u32> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.native_process_id(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => None,
        }
    }

    pub fn v8_session_handle(&self) -> Option<V8SessionHandle> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => Some(execution.v8_session_handle()),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => None,
        }
    }

    pub fn start_prepared(&mut self) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.start_prepared(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => execution.start_prepared(),
        }
    }

    pub fn is_prepared_for_start(&self) -> bool {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.is_prepared_for_start(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => execution.is_prepared_for_start(),
        }
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.write_stdin(chunk),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => Ok(()),
        }
    }

    pub fn write_stdin_kernel_only(&mut self, chunk: &[u8]) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.write_stdin_kernel_only(chunk),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => Ok(()),
        }
    }

    pub fn close_stdin(&mut self) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.close_stdin(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => Ok(()),
        }
    }

    pub fn send_stream_event(
        &self,
        event_type: &str,
        payload: Value,
    ) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.send_stream_event(event_type, payload),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => Err(wasm_host_error(
                "ENOTSUP",
                "native Wasmtime executions do not accept V8 stream events",
            )),
        }
    }

    pub fn terminate(&self) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.terminate(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => {
                execution.terminate();
                Ok(())
            }
        }
    }

    pub fn pause(&self) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.pause(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => {
                execution.set_paused(true);
                Ok(())
            }
        }
    }

    pub fn resume(&self) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.resume(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => {
                execution.set_paused(false);
                Ok(())
            }
        }
    }

    pub fn respond_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.respond_sync_rpc_success(id, result),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub fn claim_sync_rpc_response(&mut self, id: u64) -> Result<bool, WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.claim_sync_rpc_response(id),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub fn respond_claimed_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.respond_claimed_sync_rpc_success(id, result),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub fn respond_sync_rpc_raw_success(
        &mut self,
        id: u64,
        payload: Vec<u8>,
    ) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.respond_sync_rpc_raw_success(id, payload),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub fn respond_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.respond_sync_rpc_error(id, code, message),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub fn respond_claimed_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.respond_claimed_sync_rpc_error(id, code, message),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub async fn poll_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.poll_event(timeout).await,
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => execution.poll_event(timeout).await,
        }
    }

    pub fn try_poll_event(&mut self) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.try_poll_event(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => execution.try_poll_event(),
        }
    }

    pub fn poll_event_blocking(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.poll_event_blocking(timeout),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => execution.poll_event_blocking(timeout),
        }
    }

    pub fn wait(self) -> Result<WasmExecutionResult, WasmExecutionError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.wait(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => {
                let execution_id = execution.execution_id().to_owned();
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                loop {
                    match execution.next_event_blocking()? {
                        WasmExecutionEvent::Stdout(chunk) => stdout.extend_from_slice(&chunk),
                        WasmExecutionEvent::Stderr(chunk) => stderr.extend_from_slice(&chunk),
                        WasmExecutionEvent::Exited(exit_code) => {
                            return Ok(WasmExecutionResult {
                                execution_id,
                                exit_code,
                                stdout,
                                stderr,
                            });
                        }
                        WasmExecutionEvent::SyncRpcRequest(_) => {
                            return Err(no_native_sync_rpc());
                        }
                        WasmExecutionEvent::HostCall { .. } => {
                            return Err(wasm_host_error(
                                "ENOTCONN",
                                "native Wasmtime host calls require the sidecar host-event consumer",
                            ));
                        }
                        WasmExecutionEvent::SignalState { .. } => {}
                    }
                }
            }
        }
    }
}

impl ExecutionBackend for WasmExecution {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::WebAssembly
    }

    fn synchronous_fd_write_policy(&self) -> SynchronousFdWritePolicy {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.synchronous_fd_write_policy(),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => SynchronousFdWritePolicy::Blocking,
        }
    }

    fn descendant_wait_ownership(&self) -> DescendantWaitOwnership {
        DescendantWaitOwnership::Guest
    }

    fn descendant_output_ownership(&self) -> DescendantOutputOwnership {
        DescendantOutputOwnership::GuestDescriptors
    }

    fn native_process_id(&self) -> Option<u32> {
        WasmExecution::native_process_id(self)
    }

    fn wake_handle(&self, identity: ExecutionWakeIdentity) -> Option<ExecutionWakeHandle> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.wake_handle(identity),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(_) => None,
        }
    }

    fn configure_host_services(&mut self, host: ProcessHostCapabilitySet) {
        #[cfg(feature = "wasm-wasmtime")]
        if let Self::Wasmtime(execution) = self {
            execution.configure_host_services(host);
        }
    }

    fn is_prepared_for_start(&self) -> bool {
        WasmExecution::is_prepared_for_start(self)
    }

    fn start_prepared(&mut self) -> Result<(), HostServiceError> {
        WasmExecution::start_prepared(self).map_err(wasm_execution_host_error)
    }

    fn begin_shutdown(
        &mut self,
        reason: ShutdownReason,
    ) -> Result<ShutdownOutcome, HostServiceError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.begin_shutdown(reason),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => {
                execution.terminate();
                Ok(match reason {
                    ShutdownReason::Signal(signal) => {
                        ShutdownOutcome::Exited(ExecutionExit::Signaled {
                            signal,
                            core_dumped: false,
                        })
                    }
                    ShutdownReason::RuntimeFault => {
                        ShutdownOutcome::Exited(ExecutionExit::Exited(1))
                    }
                    _ => ShutdownOutcome::AwaitExit,
                })
            }
        }
    }

    fn set_paused(&self, paused: bool) -> Result<(), HostServiceError> {
        let result = if paused { self.pause() } else { self.resume() };
        result.map_err(wasm_execution_host_error)
    }

    fn write_stdin(&mut self, _bytes: &[u8]) -> Result<(), HostServiceError> {
        Ok(())
    }

    fn close_stdin(&mut self) -> Result<(), HostServiceError> {
        WasmExecution::close_stdin(self).map_err(wasm_execution_host_error)
    }

    fn deliver_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
        signal: i32,
        delivery_token: u64,
        flags: u32,
        thread_id: u32,
    ) -> Result<SignalCheckpointOutcome, HostServiceError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.deliver_signal_checkpoint(
                identity,
                signal,
                delivery_token,
                flags,
                thread_id,
            ),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => {
                execution.deliver_signal_checkpoint(
                    identity,
                    signal,
                    delivery_token,
                    flags,
                    thread_id,
                )?;
                Ok(SignalCheckpointOutcome::Published)
            }
        }
    }

    fn take_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.take_signal_checkpoint(identity),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => execution.take_signal_checkpoint(identity),
        }
    }

    fn take_signal_checkpoint_for_thread(
        &self,
        identity: ExecutionWakeIdentity,
        thread_id: u32,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.take_signal_checkpoint_for_thread(identity, thread_id),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => {
                execution.take_signal_checkpoint_for_thread(identity, thread_id)
            }
        }
    }

    fn discard_signal_checkpoints(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<(), HostServiceError> {
        match self {
            #[cfg(not(any(feature = "wasm-v8", feature = "wasm-wasmtime")))]
            Self::Disabled(never) => match *never {},
            #[cfg(feature = "wasm-v8")]
            Self::V8(execution) => execution.discard_signal_checkpoints(identity),
            #[cfg(feature = "wasm-wasmtime")]
            Self::Wasmtime(execution) => execution.discard_signal_checkpoints(identity),
        }
    }
}

#[derive(Default)]
pub struct WasmExecutionEngine {
    runtime: Option<DriverHandle>,
    #[cfg(feature = "wasm-v8")]
    v8: WasmV8ExecutionEngine,
    contexts: BTreeMap<String, WasmContext>,
    #[cfg(not(feature = "wasm-v8"))]
    next_context_id: u64,
    #[cfg(feature = "wasm-wasmtime")]
    next_execution_id: u64,
    event_notify: Option<Arc<Notify>>,
}

impl WasmExecutionEngine {
    pub fn new(runtime: DriverHandle) -> Self {
        Self {
            #[cfg(feature = "wasm-v8")]
            v8: WasmV8ExecutionEngine::new(runtime.clone()),
            runtime: Some(runtime),
            contexts: BTreeMap::new(),
            #[cfg(not(feature = "wasm-v8"))]
            next_context_id: 0,
            #[cfg(feature = "wasm-wasmtime")]
            next_execution_id: 0,
            event_notify: None,
        }
    }

    pub fn set_runtime_context(&mut self, runtime: DriverHandle) {
        #[cfg(feature = "wasm-v8")]
        self.v8.set_runtime_context(runtime.clone());
        self.runtime = Some(runtime);
    }

    pub fn set_event_notify(&mut self, notify: Option<Arc<Notify>>) {
        #[cfg(feature = "wasm-v8")]
        self.v8.set_event_notify(notify.clone());
        self.event_notify = notify;
    }

    pub fn create_context(&mut self, request: CreateWasmContextRequest) -> WasmContext {
        #[cfg(feature = "wasm-v8")]
        let context = self.v8.create_context(request);
        #[cfg(not(feature = "wasm-v8"))]
        let context = {
            self.next_context_id = self.next_context_id.saturating_add(1);
            WasmContext {
                context_id: format!("wasm-ctx-{}", self.next_context_id),
                vm_id: request.vm_id,
                module_path: request.module_path,
            }
        };
        self.contexts
            .insert(context.context_id.clone(), context.clone());
        context
    }

    pub fn dispose_context(&mut self, context_id: &str) -> bool {
        let removed = self.contexts.remove(context_id).is_some();
        #[cfg(feature = "wasm-v8")]
        {
            self.v8.dispose_context(context_id) || removed
        }
        #[cfg(not(feature = "wasm-v8"))]
        {
            removed
        }
    }

    pub fn context_count_for_test(&self) -> usize {
        self.contexts.len()
    }

    pub fn javascript_context_count_for_test(&self) -> usize {
        #[cfg(feature = "wasm-v8")]
        {
            self.v8.javascript_context_count_for_test()
        }
        #[cfg(not(feature = "wasm-v8"))]
        {
            0
        }
    }

    pub fn wasmtime_metrics(&self) -> Result<WasmtimeMetricsSnapshot, HostServiceError> {
        #[cfg(feature = "wasm-wasmtime")]
        {
            WasmtimeExecutionEngine::metrics()
        }
        #[cfg(not(feature = "wasm-wasmtime"))]
        {
            Ok(WasmtimeMetricsSnapshot::default())
        }
    }

    pub fn start_execution(
        &mut self,
        request: StartWasmExecutionRequest,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.start_execution_with_runtime_for_backend(request, runtime, StandaloneWasmBackend::V8)
    }

    pub fn start_execution_for_backend(
        &mut self,
        request: StartWasmExecutionRequest,
        backend: StandaloneWasmBackend,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.start_execution_with_runtime_for_backend(request, runtime, backend)
    }

    pub fn prepare_execution_with_runtime_for_backend(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: DriverHandle,
        backend: StandaloneWasmBackend,
    ) -> Result<WasmExecution, WasmExecutionError> {
        match backend {
            #[cfg(feature = "wasm-v8")]
            StandaloneWasmBackend::V8 => self
                .v8
                .prepare_execution_with_runtime(request, runtime)
                .map(|execution| WasmExecution::V8(Box::new(execution))),
            #[cfg(not(feature = "wasm-v8"))]
            StandaloneWasmBackend::V8 => Err(executor_not_compiled("wasm-v8", "wasm-v8")),
            #[cfg(feature = "wasm-wasmtime")]
            StandaloneWasmBackend::Wasmtime => self.spawn_wasmtime(request, runtime, true, false),
            #[cfg(not(feature = "wasm-wasmtime"))]
            StandaloneWasmBackend::Wasmtime => {
                Err(executor_not_compiled("wasm-wasmtime", "wasm-wasmtime"))
            }
            #[cfg(feature = "wasm-wasmtime-threads")]
            StandaloneWasmBackend::WasmtimeThreads => {
                self.spawn_wasmtime(request, runtime, true, true)
            }
            #[cfg(not(feature = "wasm-wasmtime-threads"))]
            StandaloneWasmBackend::WasmtimeThreads => Err(executor_not_compiled(
                "wasm-wasmtime-threads",
                "wasm-wasmtime-threads",
            )),
        }
    }

    pub fn start_execution_with_runtime_for_backend(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: DriverHandle,
        backend: StandaloneWasmBackend,
    ) -> Result<WasmExecution, WasmExecutionError> {
        match backend {
            #[cfg(feature = "wasm-v8")]
            StandaloneWasmBackend::V8 => self
                .v8
                .start_execution_with_runtime(request, runtime)
                .map(|execution| WasmExecution::V8(Box::new(execution))),
            #[cfg(not(feature = "wasm-v8"))]
            StandaloneWasmBackend::V8 => Err(executor_not_compiled("wasm-v8", "wasm-v8")),
            #[cfg(feature = "wasm-wasmtime")]
            StandaloneWasmBackend::Wasmtime => self.spawn_wasmtime(request, runtime, false, false),
            #[cfg(not(feature = "wasm-wasmtime"))]
            StandaloneWasmBackend::Wasmtime => {
                Err(executor_not_compiled("wasm-wasmtime", "wasm-wasmtime"))
            }
            #[cfg(feature = "wasm-wasmtime-threads")]
            StandaloneWasmBackend::WasmtimeThreads => {
                self.spawn_wasmtime(request, runtime, false, true)
            }
            #[cfg(not(feature = "wasm-wasmtime-threads"))]
            StandaloneWasmBackend::WasmtimeThreads => Err(executor_not_compiled(
                "wasm-wasmtime-threads",
                "wasm-wasmtime-threads",
            )),
        }
    }

    pub async fn start_execution_with_runtime_async_for_backend(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: DriverHandle,
        backend: StandaloneWasmBackend,
    ) -> Result<WasmExecution, WasmExecutionError> {
        match backend {
            #[cfg(feature = "wasm-v8")]
            StandaloneWasmBackend::V8 => self
                .v8
                .start_execution_with_runtime_async(request, runtime)
                .await
                .map(|execution| WasmExecution::V8(Box::new(execution))),
            #[cfg(not(feature = "wasm-v8"))]
            StandaloneWasmBackend::V8 => Err(executor_not_compiled("wasm-v8", "wasm-v8")),
            #[cfg(feature = "wasm-wasmtime")]
            StandaloneWasmBackend::Wasmtime => self.spawn_wasmtime(request, runtime, false, false),
            #[cfg(not(feature = "wasm-wasmtime"))]
            StandaloneWasmBackend::Wasmtime => {
                Err(executor_not_compiled("wasm-wasmtime", "wasm-wasmtime"))
            }
            #[cfg(feature = "wasm-wasmtime-threads")]
            StandaloneWasmBackend::WasmtimeThreads => {
                self.spawn_wasmtime(request, runtime, false, true)
            }
            #[cfg(not(feature = "wasm-wasmtime-threads"))]
            StandaloneWasmBackend::WasmtimeThreads => Err(executor_not_compiled(
                "wasm-wasmtime-threads",
                "wasm-wasmtime-threads",
            )),
        }
    }

    #[cfg(feature = "wasm-wasmtime")]
    fn spawn_wasmtime(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: DriverHandle,
        defer_execute: bool,
        threaded: bool,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let context = self
            .contexts
            .get(&request.context_id)
            .cloned()
            .ok_or_else(|| WasmExecutionError::MissingContext(request.context_id.clone()))?;
        if context.vm_id != request.vm_id {
            return Err(WasmExecutionError::VmMismatch {
                expected: context.vm_id,
                found: request.vm_id,
            });
        }
        let module_path = context
            .module_path
            .ok_or(WasmExecutionError::MissingModulePath)?;
        self.next_execution_id = self.next_execution_id.saturating_add(1);
        WasmtimeExecution::spawn(
            format!("exec-{}", self.next_execution_id),
            module_path,
            request,
            runtime,
            self.event_notify.clone(),
            defer_execute,
            threaded,
        )
        .map(WasmExecution::Wasmtime)
    }

    pub fn dispose_vm(&mut self, vm_id: &str) {
        self.contexts.retain(|_, context| context.vm_id != vm_id);
        #[cfg(feature = "wasm-v8")]
        self.v8.dispose_vm(vm_id);
    }

    fn runtime_context(&self) -> Result<&DriverHandle, WasmExecutionError> {
        self.runtime.as_ref().ok_or_else(|| {
            WasmExecutionError::Spawn(std::io::Error::other(
                "ERR_AGENTOS_RUNTIME_NOT_INJECTED: WasmExecutionEngine requires a process DriverHandle; construct it with WasmExecutionEngine::new(runtime)",
            ))
        })
    }
}

fn no_native_sync_rpc() -> WasmExecutionError {
    wasm_host_error(
        "ENOTSUP",
        "native Wasmtime imports use direct typed host waiters, not V8 sync RPC",
    )
}

fn wasm_host_error(code: &'static str, message: &'static str) -> WasmExecutionError {
    WasmExecutionError::Host(HostServiceError::new(code, message))
}

fn wasm_execution_host_error(error: WasmExecutionError) -> HostServiceError {
    match error {
        WasmExecutionError::Host(error) => error,
        error => HostServiceError::new("ERR_AGENTOS_WASM_EXECUTION", error.to_string()),
    }
}

#[cfg(any(
    not(feature = "wasm-v8"),
    not(feature = "wasm-wasmtime"),
    not(feature = "wasm-wasmtime-threads")
))]
fn executor_not_compiled(executor: &'static str, feature: &'static str) -> WasmExecutionError {
    WasmExecutionError::Host(
        HostServiceError::new(
            "ERR_AGENTOS_EXECUTOR_NOT_COMPILED",
            format!("the {executor} executor was not compiled into this binary"),
        )
        .with_details(serde_json::json!({
            "executor": executor,
            "feature": feature,
        })),
    )
}
