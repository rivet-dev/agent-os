#[path = "../../bridge/tests/support.rs"]
mod bridge_support;

use agent_os_bridge::{
    BridgeTypes, ClockRequest, CreateJavascriptContextRequest, CreateWasmContextRequest,
    DiagnosticRecord, ExecutionEvent, ExecutionSignal, GuestKernelCall, GuestRuntime,
    KillExecutionRequest, LifecycleEventRecord, LifecycleState, LogLevel, LogRecord,
    PollExecutionEventRequest, RandomBytesRequest, ScheduleTimerRequest, StructuredEventRecord,
};
use agent_os_sidecar_browser::{
    BrowserSidecarBridge, BrowserWorkerBridge, BrowserWorkerHandle, BrowserWorkerHandleRequest,
    BrowserWorkerSpawnRequest,
};
use bridge_support::RecordingBridge;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::time::Duration;

impl BrowserWorkerBridge for RecordingBridge {
    fn create_worker(
        &mut self,
        request: BrowserWorkerSpawnRequest,
    ) -> Result<BrowserWorkerHandle, Self::Error> {
        Ok(BrowserWorkerHandle {
            worker_id: format!("bridge-worker-{}", request.context_id),
            runtime: request.runtime,
        })
    }

    fn terminate_worker(
        &mut self,
        _request: BrowserWorkerHandleRequest,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn assert_browser_sidecar_bridge<B>(bridge: &mut B)
where
    B: BrowserSidecarBridge,
    <B as BridgeTypes>::Error: Debug,
{
    assert_eq!(
        bridge
            .monotonic_clock(ClockRequest {
                vm_id: String::from("vm-browser"),
            })
            .expect("monotonic clock"),
        Duration::from_millis(42)
    );
    assert_eq!(
        bridge
            .fill_random_bytes(RandomBytesRequest {
                vm_id: String::from("vm-browser"),
                len: 3,
            })
            .expect("random bytes"),
        vec![0xA5; 3]
    );
    assert_eq!(
        bridge
            .schedule_timer(ScheduleTimerRequest {
                vm_id: String::from("vm-browser"),
                delay: Duration::from_millis(8),
            })
            .expect("schedule timer")
            .timer_id,
        "timer-1"
    );

    let js = bridge
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: Some(String::from("@rivet-dev/agent-os/browser")),
        })
        .expect("create js context");
    let wasm = bridge
        .create_wasm_context(CreateWasmContextRequest {
            vm_id: String::from("vm-browser"),
            module_path: Some(String::from("/workspace/main.wasm")),
        })
        .expect("create wasm context");

    assert_eq!(js.runtime, GuestRuntime::JavaScript);
    assert_eq!(wasm.runtime, GuestRuntime::WebAssembly);

    bridge
        .emit_log(LogRecord {
            vm_id: String::from("vm-browser"),
            level: LogLevel::Debug,
            message: String::from("worker online"),
        })
        .expect("emit log");
    bridge
        .emit_diagnostic(DiagnosticRecord {
            vm_id: String::from("vm-browser"),
            message: String::from("worker created"),
            fields: BTreeMap::new(),
        })
        .expect("emit diagnostic");
    bridge
        .emit_structured_event(StructuredEventRecord {
            vm_id: String::from("vm-browser"),
            name: String::from("worker.message"),
            fields: BTreeMap::from([(String::from("kind"), String::from("ready"))]),
        })
        .expect("emit structured event");
    bridge
        .emit_lifecycle(LifecycleEventRecord {
            vm_id: String::from("vm-browser"),
            state: LifecycleState::Starting,
            detail: Some(String::from("bootstrapping worker")),
        })
        .expect("emit lifecycle");

    bridge
        .kill_execution(KillExecutionRequest {
            vm_id: String::from("vm-browser"),
            execution_id: String::from("exec-browser"),
            signal: ExecutionSignal::Kill,
        })
        .expect("kill execution");

    match bridge
        .poll_execution_event(PollExecutionEventRequest {
            vm_id: String::from("vm-browser"),
        })
        .expect("poll event")
    {
        Some(ExecutionEvent::GuestRequest(event)) => {
            assert_eq!(event.operation, "module.load");
        }
        other => panic!("unexpected execution event: {other:?}"),
    }
}

#[test]
fn browser_sidecar_crate_compiles_against_composed_host_bridge() {
    let mut bridge = RecordingBridge::default();
    bridge.push_execution_event(ExecutionEvent::GuestRequest(GuestKernelCall {
        vm_id: String::from("vm-browser"),
        execution_id: String::from("exec-browser"),
        operation: String::from("module.load"),
        payload: Vec::new(),
    }));

    assert_browser_sidecar_bridge(&mut bridge);
}
