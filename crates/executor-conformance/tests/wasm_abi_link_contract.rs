mod support;

use agentos_executor_conformance::{
    backend::{
        bounded_execution_event_channel, ExecutionBackend, ExecutionEvent, HostCallReply,
        PayloadLimit,
    },
    host::{
        FilesystemOperation, HostOperation, HostProcessContext, ProcessHostCapabilitySet,
        ProcessOperation, SignalOperation,
    },
    CreateWasmContextRequest, StandaloneWasmBackend, StartWasmExecutionRequest, WasmExecutionEvent,
    WasmExecutionResult, WasmPermissionTier,
};
use agentos_wasm_abi_generator::{
    imports_module, single_import_module, AbiImport, AbiManifest, CallArguments,
};
use std::{
    collections::BTreeMap,
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tempfile::tempdir;

const ABI_MANIFEST: &str = include_str!("../../wasm-common/assets/agentos-wasm-abi.json");

fn run_fixture(
    engine: &mut agentos_executor_conformance::WasmExecutionEngine,
    root: &std::path::Path,
    file_name: &str,
    bytes: &[u8],
    tier: WasmPermissionTier,
    backend: StandaloneWasmBackend,
) -> WasmExecutionResult {
    fs::write(root.join(file_name), bytes).expect("write generated ABI fixture");
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm-abi-link"),
        module_path: Some(format!("./{file_name}")),
    });
    let mut execution = engine
        .start_execution_for_backend(
            StartWasmExecutionRequest {
                guest_runtime: Default::default(),
                limits: Default::default(),
                vm_id: String::from("vm-wasm-abi-link"),
                context_id: context.context_id,
                managed_kernel_host: false,
                argv: Vec::new(),
                env: BTreeMap::new(),
                cwd: root.to_path_buf(),
                permission_tier: tier,
            },
            backend,
        )
        .expect("start generated ABI fixture");
    let process = HostProcessContext {
        generation: 1,
        pid: 1,
    };
    let (submission, host_events) = bounded_execution_event_channel(
        process,
        16,
        PayloadLimit::new("tests.wasmAbi.maxHostEventBytes", 2 * 1024 * 1024)
            .expect("host event byte limit"),
        Arc::new(|| {}),
    )
    .expect("host event channel");
    ExecutionBackend::configure_host_services(
        &mut execution,
        ProcessHostCapabilitySet::from_event_submission(submission),
    );
    let host_done = Arc::new(AtomicBool::new(false));
    let worker_done = Arc::clone(&host_done);
    let module = bytes.to_vec();
    let host_worker = std::thread::spawn(move || {
        while !worker_done.load(Ordering::Acquire) {
            let Some(event) = host_events.try_recv().expect("poll ABI host event") else {
                std::thread::yield_now();
                continue;
            };
            let ExecutionEvent::HostCall { operation, reply } = event else {
                panic!("unexpected non-host event");
            };
            match operation {
                HostOperation::Filesystem(FilesystemOperation::CanonicalPreopens) => reply
                    .succeed_json(serde_json::Value::Null)
                    .expect("canonical preopens"),
                HostOperation::Process(ProcessOperation::OpenExecutableImage { .. }) => reply
                    .succeed_json(serde_json::json!({
                        "handle": "1",
                        "size": module.len(),
                    }))
                    .expect("open executable image"),
                HostOperation::Process(ProcessOperation::ReadExecutableImage {
                    handle,
                    offset,
                    max_bytes,
                }) => {
                    assert_eq!(handle, 1);
                    let start = usize::try_from(offset).expect("module offset");
                    let end = start.saturating_add(max_bytes.get()).min(module.len());
                    reply
                        .succeed_raw(module[start..end].to_vec())
                        .expect("read executable image");
                }
                HostOperation::Process(ProcessOperation::CloseExecutableImage { handle }) => {
                    assert_eq!(handle, 1);
                    reply
                        .succeed_json(serde_json::Value::Null)
                        .expect("close executable image");
                }
                HostOperation::Signal(SignalOperation::UpdateMask { .. }) => reply
                    .succeed_json(serde_json::json!({ "signals": [] }))
                    .expect("initial signal mask"),
                operation => panic!("unexpected ABI host operation: {operation:?}"),
            }
        }
    });
    let execution_id = execution.execution_id().to_owned();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        let event = execution
            .poll_event_blocking(Duration::from_secs(10))
            .expect("poll generated ABI fixture")
            .expect("generated ABI fixture timed out");
        match event {
            WasmExecutionEvent::Stdout(chunk) => stdout.extend_from_slice(&chunk),
            WasmExecutionEvent::Stderr(chunk) => stderr.extend_from_slice(&chunk),
            WasmExecutionEvent::SignalState { .. } => {}
            WasmExecutionEvent::SyncRpcRequest(request) => {
                let result = signal_compatibility_reply(&request.method, &request.args)
                    .unwrap_or_else(|| panic!("unexpected V8-WASM host call: {}", request.method));
                execution
                    .respond_sync_rpc_success(request.id, result)
                    .expect("respond to V8-WASM signal bootstrap");
            }
            WasmExecutionEvent::HostCall { request, reply } => {
                let result = signal_compatibility_reply(&request.method, &request.args)
                    .unwrap_or_else(|| panic!("unexpected Wasmtime host call: {}", request.method));
                reply
                    .succeed(HostCallReply::Json(result))
                    .expect("respond to Wasmtime signal bootstrap");
            }
            WasmExecutionEvent::Exited(exit_code) => {
                let result = WasmExecutionResult {
                    execution_id,
                    exit_code,
                    stdout,
                    stderr,
                };
                host_done.store(true, Ordering::Release);
                host_worker.join().expect("join ABI host worker");
                return result;
            }
        }
    }
}

fn signal_compatibility_reply(
    method: &str,
    args: &[serde_json::Value],
) -> Option<serde_json::Value> {
    let method = if method == "process.wasm_sync_rpc" {
        args.first()?.as_str()?
    } else {
        method
    };
    match method {
        "process.signal_mask" => Some(serde_json::json!({ "signals": [] })),
        "process.signal_mask_scope_begin" => Some(serde_json::json!(1)),
        "process.signal_mask_scope_end"
        | "process.signal_end"
        | "process.take_signal"
        | "process.signal_begin"
        | "process.signal_state" => Some(serde_json::Value::Null),
        _ => None,
    }
}

#[test]
fn every_permitted_import_and_preview1_alias_links_at_every_tier() {
    let manifest = AbiManifest::parse(ABI_MANIFEST);
    let temp = tempdir().expect("create temp dir");
    let mut engine = support::wasm_engine();

    for backend in [StandaloneWasmBackend::V8, StandaloneWasmBackend::Wasmtime] {
        for (tier_name, tier) in [
            ("isolated", WasmPermissionTier::Isolated),
            ("read-only", WasmPermissionTier::ReadOnly),
            ("read-write", WasmPermissionTier::ReadWrite),
            ("full", WasmPermissionTier::Full),
        ] {
            let permitted = manifest.permitted_imports(tier_name);
            assert!(!permitted.is_empty(), "{tier_name} ABI must not be empty");
            let result = run_fixture(
                &mut engine,
                temp.path(),
                &format!("linkable-{backend:?}-{tier_name}.wasm"),
                &imports_module(&permitted, false, CallArguments::Zero),
                tier,
                backend,
            );
            assert_eq!(
                result.exit_code,
                0,
                "{backend:?} {tier_name} ABI failed to link: stdout={} stderr={}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
}

#[test]
fn preview1_proc_exit_and_compatibility_alias_are_terminal_calls() {
    let manifest = AbiManifest::parse(ABI_MANIFEST);
    let proc_exit = manifest
        .imports
        .iter()
        .find(|import| import.module == "wasi_snapshot_preview1" && import.name == "proc_exit")
        .expect("Preview1 proc_exit manifest entry");
    let temp = tempdir().expect("create temp dir");
    let mut engine = support::wasm_engine();

    for backend in [StandaloneWasmBackend::V8, StandaloneWasmBackend::Wasmtime] {
        for module in ["wasi_snapshot_preview1", "wasi_unstable"] {
            let mut import = proc_exit.clone();
            import.module = module.to_string();
            let result = run_fixture(
                &mut engine,
                temp.path(),
                &format!("proc-exit-{backend:?}-{module}.wasm"),
                &single_import_module(&import, true, CallArguments::Zero),
                WasmPermissionTier::Full,
                backend,
            );
            assert_eq!(
                result.exit_code,
                0,
                "{backend:?} {module}.proc_exit did not execute: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
}

#[test]
fn manifest_permission_tiers_omit_denied_and_undeclared_imports() {
    let manifest = AbiManifest::parse(ABI_MANIFEST);
    let temp = tempdir().expect("create temp dir");
    let mut engine = support::wasm_engine();

    let cases = [
        ("host_net", "net_socket", WasmPermissionTier::ReadWrite),
        (
            "host_process",
            "proc_spawn_v4",
            WasmPermissionTier::ReadWrite,
        ),
        ("host_process", "fd_getfd", WasmPermissionTier::Isolated),
    ];
    let undeclared = AbiImport {
        module: String::from("host_unknown"),
        name: String::from("ambient_escape"),
        params: Vec::new(),
        results: Vec::new(),
    };
    for backend in [StandaloneWasmBackend::V8, StandaloneWasmBackend::Wasmtime] {
        for (module, name, tier) in cases {
            let import = manifest
                .imports
                .iter()
                .find(|import| import.module == module && import.name == name)
                .unwrap_or_else(|| panic!("missing {module}.{name} manifest entry"));
            let result = run_fixture(
                &mut engine,
                temp.path(),
                &format!("denied-{backend:?}-{module}-{name}.wasm"),
                &single_import_module(import, false, CallArguments::Zero),
                tier,
                backend,
            );
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert_ne!(
                result.exit_code, 0,
                "{backend:?} {module}.{name} must not link at {tier:?}"
            );
            assert!(
                stderr.contains("ERR_AGENTOS_WASM_INSTANTIATION")
                    || stderr.contains("ERR_AGENTOS_WASM_UNSUPPORTED_IMPORT"),
                "unexpected {backend:?} denied-import error for {module}.{name}: {stderr}"
            );
        }

        let rejected = run_fixture(
            &mut engine,
            temp.path(),
            &format!("undeclared-import-{backend:?}.wasm"),
            &single_import_module(&undeclared, false, CallArguments::Zero),
            WasmPermissionTier::Full,
            backend,
        );
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert_ne!(
            rejected.exit_code, 0,
            "{backend:?} undeclared import must not link"
        );
        assert!(
            stderr.contains("ERR_AGENTOS_WASM_INSTANTIATION")
                || stderr.contains("ERR_AGENTOS_WASM_UNSUPPORTED_IMPORT"),
            "unexpected {backend:?} undeclared-import error: {stderr}"
        );
    }
}
