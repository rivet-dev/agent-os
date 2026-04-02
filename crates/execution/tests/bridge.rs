#[path = "../../bridge/tests/support.rs"]
mod bridge_support;

use agent_os_bridge::{
    BridgeTypes, CreateJavascriptContextRequest, CreateWasmContextRequest, ExecutionEvent,
    ExecutionHandleRequest, ExecutionSignal, GuestKernelCall, GuestRuntime, KillExecutionRequest,
    PollExecutionEventRequest, StartExecutionRequest, WriteExecutionStdinRequest,
};
use agent_os_execution::NativeExecutionBridge;
use bridge_support::RecordingBridge;
use std::collections::BTreeMap;
use std::fmt::Debug;

fn assert_native_execution_bridge<B>(bridge: &mut B)
where
    B: NativeExecutionBridge,
    <B as BridgeTypes>::Error: Debug,
{
    let js = bridge
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-1"),
            bootstrap_module: None,
        })
        .expect("create js context");
    let wasm = bridge
        .create_wasm_context(CreateWasmContextRequest {
            vm_id: String::from("vm-1"),
            module_path: Some(String::from("/workspace/module.wasm")),
        })
        .expect("create wasm context");

    assert_eq!(js.runtime, GuestRuntime::JavaScript);
    assert_eq!(wasm.runtime, GuestRuntime::WebAssembly);

    let execution = bridge
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-1"),
            context_id: js.context_id,
            argv: vec![String::from("index.js")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("start execution");

    bridge
        .write_stdin(WriteExecutionStdinRequest {
            vm_id: String::from("vm-1"),
            execution_id: execution.execution_id.clone(),
            chunk: b"stdin".to_vec(),
        })
        .expect("write stdin");
    bridge
        .close_stdin(ExecutionHandleRequest {
            vm_id: String::from("vm-1"),
            execution_id: execution.execution_id.clone(),
        })
        .expect("close stdin");
    bridge
        .kill_execution(KillExecutionRequest {
            vm_id: String::from("vm-1"),
            execution_id: execution.execution_id,
            signal: ExecutionSignal::Interrupt,
        })
        .expect("kill execution");

    match bridge
        .poll_execution_event(PollExecutionEventRequest {
            vm_id: String::from("vm-1"),
        })
        .expect("poll event")
    {
        Some(ExecutionEvent::GuestRequest(event)) => {
            assert_eq!(event.operation, "stdio.flush");
        }
        other => panic!("unexpected execution event: {other:?}"),
    }
}

#[test]
fn execution_crate_compiles_against_method_oriented_execution_bridge() {
    let mut bridge = RecordingBridge::default();
    bridge.push_execution_event(ExecutionEvent::GuestRequest(GuestKernelCall {
        vm_id: String::from("vm-1"),
        execution_id: String::from("exec-queued"),
        operation: String::from("stdio.flush"),
        payload: Vec::new(),
    }));

    assert_native_execution_bridge(&mut bridge);
}
