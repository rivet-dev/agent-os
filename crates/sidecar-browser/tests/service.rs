#[path = "../../bridge/tests/support.rs"]
mod bridge_support;

use agent_os_bridge::{
    CreateJavascriptContextRequest, CreateWasmContextRequest, ExecutionEvent, ExecutionExited,
    ExecutionSignal, GuestRuntime, KillExecutionRequest, LifecycleState, PollExecutionEventRequest,
    StartExecutionRequest,
};
use agent_os_kernel::kernel::KernelVmConfig;
use agent_os_kernel::permissions::Permissions;
use agent_os_sidecar_browser::{
    BrowserSidecar, BrowserSidecarConfig, BrowserWorkerBridge, BrowserWorkerEntrypoint,
    BrowserWorkerHandle, BrowserWorkerHandleRequest, BrowserWorkerSpawnRequest,
};
use bridge_support::RecordingBridge;
use std::collections::BTreeMap;

impl BrowserWorkerBridge for RecordingBridge {
    fn create_worker(
        &mut self,
        request: BrowserWorkerSpawnRequest,
    ) -> Result<BrowserWorkerHandle, Self::Error> {
        let kind = match request.runtime {
            GuestRuntime::JavaScript => "js",
            GuestRuntime::WebAssembly => "wasm",
        };

        Ok(BrowserWorkerHandle {
            worker_id: format!("{kind}-worker-{}", request.context_id),
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

#[test]
fn browser_sidecar_runs_guest_javascript_from_main_thread_workers() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    let mut config = KernelVmConfig::new("vm-browser");
    config.permissions = Permissions::allow_all();
    sidecar.create_vm(config).expect("create vm");

    let context = sidecar
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: Some(String::from("@rivet-dev/agent-os/browser")),
        })
        .expect("create JavaScript context");
    let started = sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id.clone(),
            argv: vec![String::from("node"), String::from("script.js")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("start JavaScript execution");

    assert_eq!(sidecar.sidecar_id(), "agent-os-sidecar-browser");
    assert_eq!(sidecar.vm_count(), 1);
    assert_eq!(sidecar.context_count("vm-browser"), 1);
    assert_eq!(sidecar.active_worker_count("vm-browser"), 1);

    sidecar
        .bridge_mut()
        .push_execution_event(ExecutionEvent::Exited(ExecutionExited {
            vm_id: String::from("vm-browser"),
            execution_id: started.execution_id.clone(),
            exit_code: 0,
        }));
    let event = sidecar
        .poll_execution_event(PollExecutionEventRequest {
            vm_id: String::from("vm-browser"),
        })
        .expect("poll execution event");

    assert!(matches!(
        event,
        Some(ExecutionEvent::Exited(ExecutionExited {
            execution_id,
            exit_code: 0,
            ..
        })) if execution_id == started.execution_id
    ));
    assert_eq!(sidecar.active_worker_count("vm-browser"), 0);

    let bridge = sidecar.into_bridge();
    let states = bridge
        .lifecycle_events
        .iter()
        .map(|event| event.state)
        .collect::<Vec<_>>();
    assert_eq!(
        states,
        vec![
            LifecycleState::Starting,
            LifecycleState::Ready,
            LifecycleState::Busy,
            LifecycleState::Ready,
        ]
    );
    let structured_names = bridge
        .structured_events
        .iter()
        .map(|event| event.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        structured_names,
        vec![
            "browser.context.created",
            "browser.worker.spawned",
            "browser.worker.reaped",
        ]
    );
}

#[test]
fn browser_sidecar_runs_guest_wasm_from_main_thread_workers() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    let mut config = KernelVmConfig::new("vm-browser");
    config.permissions = Permissions::allow_all();
    sidecar.create_vm(config).expect("create vm");

    let context = sidecar
        .create_wasm_context(CreateWasmContextRequest {
            vm_id: String::from("vm-browser"),
            module_path: Some(String::from("/workspace/app.wasm")),
        })
        .expect("create WebAssembly context");
    let started = sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id.clone(),
            argv: vec![String::from("wasm"), String::from("/workspace/app.wasm")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("start WebAssembly execution");

    assert_eq!(sidecar.context_count("vm-browser"), 1);
    assert_eq!(sidecar.active_worker_count("vm-browser"), 1);

    sidecar
        .kill_execution(KillExecutionRequest {
            vm_id: String::from("vm-browser"),
            execution_id: started.execution_id,
            signal: ExecutionSignal::Kill,
        })
        .expect("kill execution");
    sidecar.dispose_vm("vm-browser").expect("dispose vm");

    assert_eq!(sidecar.vm_count(), 0);

    let bridge = sidecar.into_bridge();
    assert_eq!(bridge.killed_executions.len(), 1);
    assert_eq!(
        bridge
            .lifecycle_events
            .last()
            .expect("final lifecycle event")
            .state,
        LifecycleState::Terminated
    );
    assert!(bridge.structured_events.iter().any(|event| {
        event.name == "browser.worker.spawned"
            && event.fields.get("runtime") == Some(&String::from("webassembly"))
    }));
}

#[test]
fn browser_worker_spawn_requests_preserve_browser_entrypoints() {
    let javascript = BrowserWorkerSpawnRequest {
        vm_id: String::from("vm-browser"),
        context_id: String::from("ctx-js"),
        runtime: GuestRuntime::JavaScript,
        entrypoint: BrowserWorkerEntrypoint::JavaScript {
            bootstrap_module: Some(String::from("@rivet-dev/agent-os/browser")),
        },
    };
    let wasm = BrowserWorkerSpawnRequest {
        vm_id: String::from("vm-browser"),
        context_id: String::from("ctx-wasm"),
        runtime: GuestRuntime::WebAssembly,
        entrypoint: BrowserWorkerEntrypoint::WebAssembly {
            module_path: Some(String::from("/workspace/app.wasm")),
        },
    };

    assert!(matches!(
        javascript.entrypoint,
        BrowserWorkerEntrypoint::JavaScript {
            bootstrap_module: Some(_)
        }
    ));
    assert!(matches!(
        wasm.entrypoint,
        BrowserWorkerEntrypoint::WebAssembly {
            module_path: Some(_)
        }
    ));
}
