#[path = "../../bridge/tests/support.rs"]
mod bridge_support;

use agentos_bridge::{
    CreateJavascriptContextRequest, CreateWasmContextRequest, ExecutionEvent, ExecutionExited,
    ExecutionSignal, ExecutionSignalState, GuestKernelCall, GuestRuntime, KillExecutionRequest,
    LifecycleState, PollExecutionEventRequest, SignalDispositionAction, SignalHandlerRegistration,
    StartExecutionRequest,
};
use agentos_kernel::kernel::KernelVmConfig;
use agentos_kernel::permissions::{
    NetworkAccessRequest, NetworkOperation, PermissionDecision, Permissions,
};
use agentos_kernel::resource_accounting::ResourceLimits;
use agentos_kernel::root_fs::FilesystemEntryKind;
use agentos_native_sidecar_browser::{
    BrowserSidecar, BrowserSidecarConfig, BrowserWorkerBridge, BrowserWorkerEntrypoint,
    BrowserWorkerHandle, BrowserWorkerHandleRequest, BrowserWorkerOsConfig,
    BrowserWorkerProcessConfig, BrowserWorkerSpawnRequest,
};
use agentos_sidecar_protocol::wire::{
    FindBoundUdpRequest, FindListenerRequest, GuestKernelCallRequest, WasmPermissionTier,
};
use agentos_vm_config::{
    RootFilesystemConfig, RootFilesystemEntry, RootFilesystemEntryEncoding,
    RootFilesystemEntryKind, RootFilesystemLowerDescriptor, RootFilesystemMode,
};
use bridge_support::RecordingBridge;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

impl BrowserWorkerBridge for RecordingBridge {
    fn create_worker(
        &mut self,
        request: BrowserWorkerSpawnRequest,
    ) -> Result<BrowserWorkerHandle, Self::Error> {
        if let Some(error) = self.next_worker_create_error() {
            return Err(error);
        }

        let kind = match request.runtime {
            GuestRuntime::JavaScript => "js",
            GuestRuntime::WebAssembly => "wasm",
        };
        self.browser_worker_spawns.push(BTreeMap::from([
            (String::from("vm_id"), request.vm_id.clone()),
            (String::from("context_id"), request.context_id.clone()),
            (String::from("execution_id"), request.execution_id.clone()),
            (
                String::from("process_platform"),
                request.process_config.platform.clone(),
            ),
            (
                String::from("process_arch"),
                request.process_config.arch.clone(),
            ),
            (
                String::from("process_cwd"),
                request.process_config.cwd.clone(),
            ),
            (
                String::from("process_pid"),
                request.process_config.pid.to_string(),
            ),
            (
                String::from("process_uid"),
                request.process_config.uid.to_string(),
            ),
            (
                String::from("process_gid"),
                request.process_config.gid.to_string(),
            ),
            (
                String::from("process_env_base"),
                request
                    .process_config
                    .env
                    .get("BASE_ENV")
                    .cloned()
                    .unwrap_or_default(),
            ),
            (
                String::from("process_env_exec"),
                request
                    .process_config
                    .env
                    .get("EXEC_ENV")
                    .cloned()
                    .unwrap_or_default(),
            ),
            (
                String::from("os_platform"),
                request.os_config.platform.clone(),
            ),
            (
                String::from("os_cpu_count"),
                request.os_config.cpu_count.to_string(),
            ),
            (
                String::from("os_totalmem"),
                request.os_config.totalmem.to_string(),
            ),
            (
                String::from("os_freemem"),
                request.os_config.freemem.to_string(),
            ),
            (String::from("os_user"), request.os_config.user.clone()),
            (String::from("os_uid"), request.os_config.uid.to_string()),
            (String::from("os_gid"), request.os_config.gid.to_string()),
            (
                String::from("os_homedir"),
                request.os_config.homedir.clone(),
            ),
            (
                String::from("os_hostname"),
                request.os_config.hostname.clone(),
            ),
            (String::from("os_type"), request.os_config.r#type.clone()),
            (
                String::from("os_release"),
                request.os_config.release.clone(),
            ),
            (
                String::from("os_version"),
                request.os_config.version.clone(),
            ),
            (String::from("os_tmpdir"), request.os_config.tmpdir.clone()),
            (
                String::from("os_machine"),
                request.os_config.machine.clone(),
            ),
            (
                String::from("wasm_permission_tier"),
                request
                    .wasm_permission_tier
                    .map(|tier| format!("{tier:?}"))
                    .unwrap_or_default(),
            ),
        ]));

        Ok(BrowserWorkerHandle {
            worker_id: format!("{kind}-worker-{}", request.context_id),
            runtime: request.runtime,
        })
    }

    fn terminate_worker(&mut self, request: BrowserWorkerHandleRequest) -> Result<(), Self::Error> {
        self.terminated_workers
            .push((request.vm_id, request.execution_id, request.worker_id));
        Ok(())
    }
}

fn permissive_config(vm_id: &str) -> KernelVmConfig {
    let mut config = KernelVmConfig::new(vm_id);
    config.permissions = Permissions::allow_all();
    config
}

fn test_process_config() -> BrowserWorkerProcessConfig {
    BrowserWorkerProcessConfig {
        cwd: String::from("/workspace"),
        env: BTreeMap::new(),
        argv: vec![String::from("node")],
        platform: String::from("linux"),
        arch: String::from("x64"),
        version: String::from("v22.0.0"),
        pid: 1,
        ppid: 0,
        uid: 1000,
        gid: 1000,
    }
}

fn test_os_config() -> BrowserWorkerOsConfig {
    BrowserWorkerOsConfig {
        platform: String::from("linux"),
        arch: String::from("x64"),
        r#type: String::from("Linux"),
        release: String::from("6.8.0-agentos"),
        version: String::from("#1 SMP PREEMPT_DYNAMIC agentos"),
        cpu_count: 1,
        totalmem: 1024 * 1024 * 1024,
        freemem: 512 * 1024 * 1024,
        hostname: String::from("agentos"),
        homedir: String::from("/home/user"),
        tmpdir: String::from("/tmp"),
        machine: String::from("x86_64"),
        user: String::from("user"),
        shell: String::from("/bin/sh"),
        uid: 1000,
        gid: 1000,
    }
}

#[test]
fn browser_sidecar_runs_guest_javascript_from_main_thread_workers() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    sidecar
        .create_vm(permissive_config("vm-browser"))
        .expect("create vm");

    let context = sidecar
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: Some(String::from("@rivet-dev/agentos-runtime-browser")),
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

    assert_eq!(sidecar.sidecar_id(), "agentos-native-sidecar-browser");
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
fn browser_worker_spawn_receives_virtual_identity_config() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    let mut config = permissive_config("vm-browser");
    config
        .env
        .insert(String::from("BASE_ENV"), String::from("base"));
    config.user.username = Some(String::from("runner"));
    config.user.uid = Some(501);
    config.user.gid = Some(20);
    config.user.homedir = Some(String::from("/home/runner"));
    config.user.shell = Some(String::from("/bin/bash"));
    config.resources = ResourceLimits {
        virtual_cpu_count: Some(4),
        max_wasm_memory_bytes: Some(256 * 1024 * 1024),
        ..ResourceLimits::default()
    };
    sidecar.create_vm(config).expect("create vm");

    let context = sidecar
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: None,
        })
        .expect("create JavaScript context");
    sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id,
            argv: vec![String::from("node"), String::from("identity.js")],
            env: BTreeMap::from([(String::from("EXEC_ENV"), String::from("exec"))]),
            cwd: String::from("/workspace"),
        })
        .expect("start execution");

    let bridge = sidecar.into_bridge();
    let spawn = bridge
        .browser_worker_spawns
        .last()
        .expect("worker spawn should be recorded");
    assert_eq!(
        spawn.get("process_platform").map(String::as_str),
        Some("linux")
    );
    assert_eq!(spawn.get("process_arch").map(String::as_str), Some("x64"));
    assert_eq!(
        spawn.get("process_cwd").map(String::as_str),
        Some("/workspace")
    );
    assert_eq!(
        spawn.get("execution_id").map(String::as_str),
        Some("exec-1")
    );
    assert_eq!(
        spawn.get("process_env_base").map(String::as_str),
        Some("base")
    );
    assert_eq!(
        spawn.get("process_env_exec").map(String::as_str),
        Some("exec")
    );
    assert_eq!(spawn.get("os_platform").map(String::as_str), Some("linux"));
    assert_eq!(spawn.get("os_cpu_count").map(String::as_str), Some("4"));
    assert_eq!(
        spawn.get("os_totalmem").map(String::as_str),
        Some("268435456")
    );
    assert_eq!(
        spawn.get("os_freemem").map(String::as_str),
        Some("268435456")
    );
    assert_eq!(spawn.get("os_user").map(String::as_str), Some("runner"));
    assert_eq!(
        spawn.get("os_homedir").map(String::as_str),
        Some("/home/runner")
    );
    assert_eq!(
        spawn.get("os_hostname").map(String::as_str),
        Some("agentos")
    );
    assert_eq!(spawn.get("os_type").map(String::as_str), Some("Linux"));
    assert_eq!(
        spawn.get("os_release").map(String::as_str),
        Some("6.8.0-agentos")
    );
    assert_eq!(
        spawn.get("os_version").map(String::as_str),
        Some("#1 SMP PREEMPT_DYNAMIC agentos")
    );
    assert_eq!(spawn.get("os_tmpdir").map(String::as_str), Some("/tmp"));
    assert_eq!(spawn.get("os_machine").map(String::as_str), Some("x86_64"));
    assert!(
        spawn
            .get("process_pid")
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_some_and(|pid| pid > 0),
        "worker process pid should come from the kernel"
    );
    assert_eq!(spawn.get("process_uid").map(String::as_str), Some("501"));
    assert_eq!(spawn.get("process_gid").map(String::as_str), Some("20"));
    assert_eq!(spawn.get("os_uid").map(String::as_str), Some("501"));
    assert_eq!(spawn.get("os_gid").map(String::as_str), Some("20"));
}

#[test]
fn browser_sidecar_runs_guest_wasm_from_main_thread_workers() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    sidecar
        .create_vm(permissive_config("vm-browser"))
        .expect("create vm");

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
            .browser_worker_spawns
            .first()
            .and_then(|spawn| spawn.get("wasm_permission_tier"))
            .map(String::as_str),
        Some("Full")
    );
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
        execution_id: String::from("exec-js"),
        runtime: GuestRuntime::JavaScript,
        entrypoint: BrowserWorkerEntrypoint::JavaScript {
            bootstrap_module: Some(String::from("@rivet-dev/agentos-runtime-browser")),
        },
        wasm_permission_tier: None,
        process_config: test_process_config(),
        os_config: test_os_config(),
    };
    let wasm = BrowserWorkerSpawnRequest {
        vm_id: String::from("vm-browser"),
        context_id: String::from("ctx-wasm"),
        execution_id: String::from("exec-wasm"),
        runtime: GuestRuntime::WebAssembly,
        entrypoint: BrowserWorkerEntrypoint::WebAssembly {
            module_path: Some(String::from("/workspace/app.wasm")),
        },
        wasm_permission_tier: Some(WasmPermissionTier::ReadOnly),
        process_config: test_process_config(),
        os_config: test_os_config(),
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

#[test]
fn browser_sidecar_routes_kernel_filesystem_and_execution_state_through_vm_state() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    sidecar
        .create_vm(permissive_config("vm-browser"))
        .expect("create vm");

    assert_eq!(
        sidecar.kernel_state("vm-browser").expect("kernel ready"),
        LifecycleState::Ready
    );

    sidecar
        .mkdir("vm-browser", "/workspace", true)
        .expect("create workspace");
    sidecar
        .write_file("vm-browser", "/workspace/hello.txt", b"hello".to_vec())
        .expect("write kernel file");
    assert_eq!(
        sidecar
            .read_file("vm-browser", "/workspace/hello.txt")
            .expect("read kernel file"),
        b"hello".to_vec()
    );
    assert_eq!(
        sidecar
            .read_dir("vm-browser", "/workspace")
            .expect("read workspace"),
        vec![String::from("hello.txt")]
    );

    let context = sidecar
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: Some(String::from("@rivet-dev/agentos-runtime-browser")),
        })
        .expect("create JavaScript context");
    let started = sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id,
            argv: vec![String::from("node"), String::from("script.js")],
            env: BTreeMap::from([(String::from("MODE"), String::from("browser"))]),
            cwd: String::from("/workspace"),
        })
        .expect("start execution");

    assert_eq!(
        sidecar.kernel_state("vm-browser").expect("kernel busy"),
        LifecycleState::Busy
    );

    sidecar
        .write_stdin(agentos_bridge::WriteExecutionStdinRequest {
            vm_id: String::from("vm-browser"),
            execution_id: started.execution_id.clone(),
            chunk: b"input".to_vec(),
        })
        .expect("write stdin");
    assert_eq!(
        sidecar
            .read_execution_stdin(
                "vm-browser",
                &started.execution_id,
                16,
                Duration::from_millis(5),
            )
            .expect("read stdin"),
        Some(b"input".to_vec())
    );

    sidecar
        .bridge_mut()
        .push_execution_event(ExecutionEvent::GuestRequest(GuestKernelCall {
            vm_id: String::from("vm-browser"),
            execution_id: started.execution_id.clone(),
            operation: String::from("fs.read"),
            payload: b"{\"path\":\"/workspace/input.txt\"}".to_vec(),
        }));
    sidecar
        .poll_execution_event(PollExecutionEventRequest {
            vm_id: String::from("vm-browser"),
        })
        .expect("poll guest kernel call");
    let guest_call_events = sidecar
        .bridge()
        .structured_events
        .iter()
        .filter(|event| event.name == "guest.kernel_call.unsupported")
        .collect::<Vec<_>>();
    assert_eq!(guest_call_events.len(), 1);
    assert_eq!(
        guest_call_events[0].fields["execution_id"],
        started.execution_id
    );
    assert_eq!(guest_call_events[0].fields["operation"], "fs.read");
    assert_eq!(
        guest_call_events[0].fields["payload_size_bytes"],
        b"{\"path\":\"/workspace/input.txt\"}".len().to_string()
    );

    sidecar
        .bridge_mut()
        .push_execution_event(ExecutionEvent::SignalState(ExecutionSignalState {
            vm_id: String::from("vm-browser"),
            execution_id: started.execution_id.clone(),
            signal: 15,
            registration: SignalHandlerRegistration {
                action: SignalDispositionAction::User,
                mask: vec![2],
                flags: 0,
            },
        }));
    sidecar
        .poll_execution_event(PollExecutionEventRequest {
            vm_id: String::from("vm-browser"),
        })
        .expect("poll signal state");
    let signal_state = sidecar
        .signal_state("vm-browser", &started.execution_id)
        .expect("signal state");
    let sigterm = signal_state.get(&15).expect("SIGTERM handler");
    assert_eq!(
        sigterm.action,
        agentos_sidecar_protocol::wire::SignalDispositionAction::User
    );
    assert_eq!(sigterm.mask, vec![2]);

    sidecar
        .create_kernel_tcp_listener_for_execution(
            "vm-browser",
            &started.execution_id,
            "127.0.0.1",
            34567,
            16,
        )
        .expect("create listener owned by execution");
    sidecar
        .create_kernel_bound_udp_for_execution(
            "vm-browser",
            &started.execution_id,
            "127.0.0.1",
            34568,
        )
        .expect("create UDP binding owned by execution");
    assert!(sidecar
        .find_listener(
            "vm-browser",
            &FindListenerRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34567),
                path: None,
            },
        )
        .expect("find listener before exit")
        .is_some());
    assert!(sidecar
        .find_bound_udp(
            "vm-browser",
            &FindBoundUdpRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34568),
            },
        )
        .expect("find UDP binding before exit")
        .is_some());

    sidecar
        .bridge_mut()
        .push_execution_event(ExecutionEvent::Exited(ExecutionExited {
            vm_id: String::from("vm-browser"),
            execution_id: started.execution_id.clone(),
            exit_code: 0,
        }));
    sidecar
        .poll_execution_event(PollExecutionEventRequest {
            vm_id: String::from("vm-browser"),
        })
        .expect("poll exit");

    assert_eq!(
        sidecar
            .kernel_state("vm-browser")
            .expect("kernel ready after exit"),
        LifecycleState::Ready
    );
    assert!(sidecar
        .signal_state("vm-browser", &started.execution_id)
        .expect("signal state after exit")
        .is_empty());
    assert!(sidecar
        .find_listener(
            "vm-browser",
            &FindListenerRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34567),
                path: None,
            },
        )
        .expect("find listener after exit")
        .is_none());
    assert!(sidecar
        .find_bound_udp(
            "vm-browser",
            &FindBoundUdpRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34568),
            },
        )
        .expect("find UDP binding after exit")
        .is_none());
}

#[test]
fn browser_sidecar_routes_guest_kernel_call_net_loopback_through_vm_state() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    sidecar
        .create_vm(permissive_config("vm-browser"))
        .expect("create vm");
    let context = sidecar
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: Some(String::from("@rivet-dev/agentos-runtime-browser")),
        })
        .expect("create JavaScript context");
    let started = sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id,
            argv: vec![String::from("node"), String::from("script.js")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("start execution");

    // The synchronous guest kernel-call path (`GuestKernelCallRequest` ->
    // shared `handle_guest_kernel_call` -> kernel sockets) is the converged
    // replacement for the fire-and-forget `GuestRequest` bail. Drive a full
    // loopback TCP exchange through it to prove the wiring end to end.
    let call = |sidecar: &mut BrowserSidecar<RecordingBridge>,
                operation: &str,
                request: serde_json::Value|
     -> serde_json::Value {
        let result = sidecar
            .guest_kernel_call(
                "vm-browser",
                GuestKernelCallRequest {
                    execution_id: started.execution_id.clone(),
                    operation: String::from(operation),
                    payload: serde_json::to_vec(&request).expect("encode request"),
                },
            )
            .expect("guest kernel call");
        serde_json::from_slice(&result.payload).expect("decode response")
    };

    let listener = call(
        &mut sidecar,
        "net.listen",
        serde_json::json!({ "host": "127.0.0.1", "port": 39221 }),
    );
    let listener_id = listener["socketId"].as_u64().expect("listener socket id");

    let client = call(
        &mut sidecar,
        "net.connect",
        serde_json::json!({ "host": "127.0.0.1", "port": 39221 }),
    );
    let client_id = client["socketId"].as_u64().expect("client socket id");

    let accepted = call(
        &mut sidecar,
        "net.accept",
        serde_json::json!({ "socketId": listener_id }),
    );
    let accepted_id = accepted["socketId"].as_u64().expect("accepted socket id");

    // base64("hello") == "aGVsbG8=" (asserted directly so no decode is needed).
    let write = call(
        &mut sidecar,
        "net.write",
        serde_json::json!({ "socketId": client_id, "data": "aGVsbG8=" }),
    );
    assert_eq!(write["written"].as_u64(), Some(5));

    let read = call(
        &mut sidecar,
        "net.read",
        serde_json::json!({ "socketId": accepted_id }),
    );
    assert_eq!(read["closed"].as_bool(), Some(false));
    assert_eq!(read["data"].as_str(), Some("aGVsbG8="));

    // Unknown operations surface as a rejected guest kernel call rather than a
    // silent fail-open.
    let error = sidecar
        .guest_kernel_call(
            "vm-browser",
            GuestKernelCallRequest {
                execution_id: started.execution_id.clone(),
                operation: String::from("net.teleport"),
                payload: b"{}".to_vec(),
            },
        )
        .expect_err("unsupported operation rejected");
    assert!(error
        .to_string()
        .contains("unsupported guest kernel call operation: net.teleport"));
}

#[test]
fn browser_sidecar_keeps_kernel_sockets_scoped_per_execution() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    sidecar
        .create_vm(permissive_config("vm-browser"))
        .expect("create vm");

    let context = sidecar
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: Some(String::from("@rivet-dev/agentos-runtime-browser")),
        })
        .expect("create JavaScript context");

    let first = sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id.clone(),
            argv: vec![String::from("node"), String::from("first.js")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("start first execution");
    let second = sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id,
            argv: vec![String::from("node"), String::from("second.js")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("start second execution");

    sidecar
        .create_kernel_tcp_listener_for_execution(
            "vm-browser",
            &first.execution_id,
            "127.0.0.1",
            34601,
            16,
        )
        .expect("create first listener");
    sidecar
        .create_kernel_tcp_listener_for_execution(
            "vm-browser",
            &second.execution_id,
            "127.0.0.1",
            34602,
            16,
        )
        .expect("create second listener");
    sidecar
        .create_kernel_bound_udp_for_execution(
            "vm-browser",
            &first.execution_id,
            "127.0.0.1",
            34611,
        )
        .expect("create first UDP binding");
    sidecar
        .create_kernel_bound_udp_for_execution(
            "vm-browser",
            &second.execution_id,
            "127.0.0.1",
            34612,
        )
        .expect("create second UDP binding");

    let second_listener = sidecar
        .find_listener(
            "vm-browser",
            &FindListenerRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34602),
                path: None,
            },
        )
        .expect("find second listener before exit")
        .expect("second listener exists");
    assert_eq!(second_listener.process_id, second.execution_id);

    sidecar
        .bridge_mut()
        .push_execution_event(ExecutionEvent::Exited(ExecutionExited {
            vm_id: String::from("vm-browser"),
            execution_id: first.execution_id.clone(),
            exit_code: 0,
        }));
    sidecar
        .poll_execution_event(PollExecutionEventRequest {
            vm_id: String::from("vm-browser"),
        })
        .expect("poll first exit");

    assert!(sidecar
        .find_listener(
            "vm-browser",
            &FindListenerRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34601),
                path: None,
            },
        )
        .expect("find first listener after exit")
        .is_none());
    assert!(sidecar
        .find_bound_udp(
            "vm-browser",
            &FindBoundUdpRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34611),
            },
        )
        .expect("find first UDP binding after exit")
        .is_none());

    let second_listener = sidecar
        .find_listener(
            "vm-browser",
            &FindListenerRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34602),
                path: None,
            },
        )
        .expect("find second listener after first exit")
        .expect("second listener remains");
    assert_eq!(second_listener.process_id, second.execution_id);
    let second_udp = sidecar
        .find_bound_udp(
            "vm-browser",
            &FindBoundUdpRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34612),
            },
        )
        .expect("find second UDP binding after first exit")
        .expect("second UDP binding remains");
    assert_eq!(second_udp.process_id, second.execution_id);
    assert_eq!(
        sidecar.kernel_state("vm-browser").expect("kernel state"),
        LifecycleState::Busy
    );
}

#[test]
fn browser_sidecar_kernel_tcp_listener_obeys_vm_network_policy() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_callback = Arc::clone(&requests);
    let mut config = KernelVmConfig::new("vm-browser");
    config.permissions = Permissions {
        network: Some(Arc::new(move |request: &NetworkAccessRequest| {
            requests_for_callback
                .lock()
                .expect("request log lock")
                .push(request.clone());
            PermissionDecision::deny("network disabled")
        })),
        ..Permissions::allow_all()
    };

    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    sidecar.create_vm(config).expect("create vm");

    let context = sidecar
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: Some(String::from("@rivet-dev/agentos-runtime-browser")),
        })
        .expect("create JavaScript context");
    let started = sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id,
            argv: vec![String::from("node"), String::from("server.js")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("start execution");

    let error = sidecar
        .create_kernel_tcp_listener_for_execution(
            "vm-browser",
            &started.execution_id,
            "127.0.0.1",
            34621,
            16,
        )
        .expect_err("TCP listener should be denied by network policy");

    assert!(
        error.to_string().contains("EACCES"),
        "unexpected error: {error}"
    );
    assert!(sidecar
        .find_listener(
            "vm-browser",
            &FindListenerRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34621),
                path: None,
            },
        )
        .expect("find listener after denied bind")
        .is_none());
    assert_eq!(
        *requests.lock().expect("request log lock"),
        vec![NetworkAccessRequest {
            vm_id: String::from("vm-browser"),
            op: NetworkOperation::Listen,
            resource: String::from("tcp://127.0.0.1:34621"),
        }]
    );
}

#[test]
fn browser_sidecar_reaps_kernel_process_after_normal_execution_exit() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    let mut config = KernelVmConfig::new("vm-browser");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_processes: Some(1),
        ..ResourceLimits::default()
    };
    sidecar.create_vm(config).expect("create vm");

    let context = sidecar
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: Some(String::from("@rivet-dev/agentos-runtime-browser")),
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
        .expect("start first execution");

    sidecar
        .bridge_mut()
        .push_execution_event(ExecutionEvent::Exited(ExecutionExited {
            vm_id: String::from("vm-browser"),
            execution_id: started.execution_id,
            exit_code: 0,
        }));
    sidecar
        .poll_execution_event(PollExecutionEventRequest {
            vm_id: String::from("vm-browser"),
        })
        .expect("poll exit");

    sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id,
            argv: vec![String::from("node"), String::from("script.js")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("completed execution should not leak the one-process limit");
}

#[test]
fn browser_sidecar_preserves_default_deny_kernel_permissions() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    sidecar
        .create_vm(KernelVmConfig::new("vm-browser"))
        .expect("create vm");

    let error = sidecar
        .write_file("vm-browser", "/workspace/denied.txt", b"denied".to_vec())
        .expect_err("default permissions should deny filesystem writes");

    // Under default deny-all the write's parent-directory resolution is the
    // first denied operation, so the error reports the read denial on the
    // parent; the write itself never proceeds.
    assert_eq!(
        error.to_string(),
        "EACCES: permission denied, read '/workspace'"
    );
}

#[test]
fn browser_sidecar_builds_mount_table_root_from_root_filesystem_config() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    sidecar
        .create_vm_with_root_filesystem(
            permissive_config("vm-browser"),
            RootFilesystemConfig {
                disable_default_base_layer: true,
                lowers: vec![
                    RootFilesystemLowerDescriptor::Snapshot {
                        entries: vec![
                            RootFilesystemEntry {
                                path: String::from("/workspace/shared.txt"),
                                kind: RootFilesystemEntryKind::File,
                                mode: None,
                                uid: None,
                                gid: None,
                                content: Some(String::from("higher")),
                                encoding: Some(RootFilesystemEntryEncoding::Utf8),
                                target: None,
                                executable: false,
                            },
                            RootFilesystemEntry {
                                path: String::from("/workspace/higher-only.txt"),
                                kind: RootFilesystemEntryKind::File,
                                mode: None,
                                uid: None,
                                gid: None,
                                content: Some(String::from("higher-only")),
                                encoding: Some(RootFilesystemEntryEncoding::Utf8),
                                target: None,
                                executable: false,
                            },
                        ],
                    },
                    RootFilesystemLowerDescriptor::Snapshot {
                        entries: vec![
                            RootFilesystemEntry {
                                path: String::from("/workspace/shared.txt"),
                                kind: RootFilesystemEntryKind::File,
                                mode: None,
                                uid: None,
                                gid: None,
                                content: Some(String::from("lower")),
                                encoding: Some(RootFilesystemEntryEncoding::Utf8),
                                target: None,
                                executable: false,
                            },
                            RootFilesystemEntry {
                                path: String::from("/workspace/lower-only.txt"),
                                kind: RootFilesystemEntryKind::File,
                                mode: None,
                                uid: None,
                                gid: None,
                                content: Some(String::from("lower-only")),
                                encoding: Some(RootFilesystemEntryEncoding::Utf8),
                                target: None,
                                executable: false,
                            },
                        ],
                    },
                ],
                bootstrap_entries: vec![
                    RootFilesystemEntry {
                        path: String::from("/workspace/shared.txt"),
                        kind: RootFilesystemEntryKind::File,
                        mode: None,
                        uid: None,
                        gid: None,
                        content: Some(String::from("upper")),
                        encoding: Some(RootFilesystemEntryEncoding::Utf8),
                        target: None,
                        executable: false,
                    },
                    RootFilesystemEntry {
                        path: String::from("/workspace/upper-only.txt"),
                        kind: RootFilesystemEntryKind::File,
                        mode: None,
                        uid: None,
                        gid: None,
                        content: Some(String::from("upper-only")),
                        encoding: Some(RootFilesystemEntryEncoding::Utf8),
                        target: None,
                        executable: false,
                    },
                ],
                ..RootFilesystemConfig::default()
            },
        )
        .expect("create vm with root filesystem config");

    for (path, expected) in [
        ("/workspace/shared.txt", b"upper".as_slice()),
        ("/workspace/higher-only.txt", b"higher-only".as_slice()),
        ("/workspace/lower-only.txt", b"lower-only".as_slice()),
        ("/workspace/upper-only.txt", b"upper-only".as_slice()),
    ] {
        assert_eq!(
            sidecar
                .read_file("vm-browser", path)
                .unwrap_or_else(|error| panic!("read {path}: {error}")),
            expected.to_vec()
        );
    }
    sidecar
        .write_file("vm-browser", "/workspace/new.txt", b"new-upper".to_vec())
        .expect("write upper entry");

    let snapshot = sidecar
        .snapshot_root_filesystem("vm-browser")
        .expect("snapshot root filesystem");
    assert!(snapshot.entries.iter().any(|entry| {
        entry.path == "/workspace/new.txt"
            && entry.kind == FilesystemEntryKind::File
            && entry.content.as_deref() == Some(b"new-upper".as_slice())
    }));
}

#[test]
fn browser_sidecar_locks_read_only_root_after_bootstrap() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    sidecar
        .create_vm_with_root_filesystem(
            permissive_config("vm-browser"),
            RootFilesystemConfig {
                mode: RootFilesystemMode::ReadOnly,
                disable_default_base_layer: true,
                bootstrap_entries: vec![RootFilesystemEntry {
                    path: String::from("/workspace/bootstrap.txt"),
                    kind: RootFilesystemEntryKind::File,
                    mode: None,
                    uid: None,
                    gid: None,
                    content: Some(String::from("bootstrapped")),
                    encoding: Some(RootFilesystemEntryEncoding::Utf8),
                    target: None,
                    executable: false,
                }],
                ..RootFilesystemConfig::default()
            },
        )
        .expect("create read-only VM with bootstrap entries");

    assert_eq!(
        sidecar
            .read_file("vm-browser", "/workspace/bootstrap.txt")
            .expect("read bootstrap entry"),
        b"bootstrapped".to_vec()
    );

    let error = sidecar
        .write_file("vm-browser", "/workspace/new.txt", b"new".to_vec())
        .expect_err("read-only root should reject writes after bootstrap");
    assert_eq!(
        error.to_string(),
        "EROFS: read-only filesystem: /workspace/new.txt"
    );
}

#[test]
fn browser_sidecar_reaps_pending_kernel_process_when_worker_startup_fails() {
    let mut bridge = RecordingBridge::default();
    bridge.push_worker_create_error("worker startup failed");

    let mut sidecar = BrowserSidecar::new(bridge, BrowserSidecarConfig::default());
    let mut config = KernelVmConfig::new("vm-browser");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_processes: Some(1),
        ..ResourceLimits::default()
    };
    sidecar.create_vm(config).expect("create vm");

    let context = sidecar
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: Some(String::from("@rivet-dev/agentos-runtime-browser")),
        })
        .expect("create JavaScript context");

    let failed = sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id.clone(),
            argv: vec![String::from("node"), String::from("script.js")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect_err("worker creation should fail");

    assert!(failed.to_string().contains("worker startup failed"));
    assert_eq!(sidecar.active_worker_count("vm-browser"), 0);
    assert_eq!(sidecar.bridge().killed_executions.len(), 1);
    assert_eq!(sidecar.bridge().killed_executions[0].execution_id, "exec-1");
    assert_eq!(
        sidecar.kernel_state("vm-browser").expect("kernel ready"),
        LifecycleState::Ready
    );

    let started = sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id,
            argv: vec![String::from("node"), String::from("script.js")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("leaked pending process would exhaust the one-process limit");

    assert_eq!(started.execution_id, "exec-2");
    assert_eq!(sidecar.active_worker_count("vm-browser"), 1);
}

#[test]
fn browser_sidecar_reaps_pending_kernel_process_when_stdio_setup_fails() {
    let mut sidecar =
        BrowserSidecar::new(RecordingBridge::default(), BrowserSidecarConfig::default());
    let mut config = KernelVmConfig::new("vm-browser");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_processes: Some(1),
        max_pipes: Some(0),
        ..ResourceLimits::default()
    };
    sidecar.create_vm(config).expect("create vm");

    let context = sidecar
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-browser"),
            bootstrap_module: Some(String::from("@rivet-dev/agentos-runtime-browser")),
        })
        .expect("create JavaScript context");

    for _ in 0..2 {
        let failed = sidecar
            .start_execution(StartExecutionRequest {
                vm_id: String::from("vm-browser"),
                context_id: context.context_id.clone(),
                argv: vec![String::from("node"), String::from("script.js")],
                env: BTreeMap::new(),
                cwd: String::from("/workspace"),
            })
            .expect_err("stdio setup should fail before worker creation");

        assert!(failed.to_string().contains("maximum pipe count reached"));
        assert_eq!(sidecar.active_worker_count("vm-browser"), 0);
        assert_eq!(
            sidecar.kernel_state("vm-browser").expect("kernel ready"),
            LifecycleState::Ready
        );
    }
}

#[test]
fn browser_sidecar_reaps_pending_kernel_process_when_bridge_execution_start_fails() {
    let mut bridge = RecordingBridge::default();
    bridge.push_execution_start_error("execution start failed");

    let mut sidecar = BrowserSidecar::new(bridge, BrowserSidecarConfig::default());
    let mut config = KernelVmConfig::new("vm-browser");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_processes: Some(1),
        ..ResourceLimits::default()
    };
    sidecar.create_vm(config).expect("create vm");

    let context = sidecar
        .create_wasm_context(CreateWasmContextRequest {
            vm_id: String::from("vm-browser"),
            module_path: Some(String::from("/workspace/app.wasm")),
        })
        .expect("create WebAssembly context");

    let failed = sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id.clone(),
            argv: vec![String::from("wasm"), String::from("/workspace/app.wasm")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect_err("execution start should fail");

    assert!(failed.to_string().contains("execution start failed"));
    assert_eq!(sidecar.active_worker_count("vm-browser"), 0);
    assert_eq!(
        sidecar.kernel_state("vm-browser").expect("kernel ready"),
        LifecycleState::Ready
    );
    assert!(sidecar.bridge().terminated_workers.is_empty());

    sidecar
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm-browser"),
            context_id: context.context_id,
            argv: vec![String::from("wasm"), String::from("/workspace/app.wasm")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("leaked pending process would exhaust the one-process limit");
}
