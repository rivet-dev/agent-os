mod support;

use agentos_execution::{
    GuestRuntimeConfig, JavascriptExecutionEngine, PythonExecutionEngine,
    StartWasmExecutionRequest, WasmExecutionEngine, WasmExecutionLimits, WasmPermissionTier,
};
use agentos_runtime::metrics::BufferMetricClass;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn execution_subsystems_do_not_lookup_or_build_runtime_topology() {
    let sources = [
        ("javascript.rs", include_str!("../src/javascript.rs")),
        ("python.rs", include_str!("../src/python.rs")),
        ("wasm.rs", include_str!("../src/wasm.rs")),
        (
            "node_import_cache.rs",
            include_str!("../src/node_import_cache.rs"),
        ),
        ("v8_host.rs", include_str!("../src/v8_host.rs")),
    ];

    for (name, source) in sources {
        assert!(
            !source.contains("SidecarRuntime::process_context"),
            "{name} must receive RuntimeContext from its owner"
        );
        assert!(
            !source.contains("tokio::runtime::Builder")
                && !source.contains("tokio::runtime::Runtime::new"),
            "{name} must not build a subsystem-owned Tokio runtime"
        );
    }
}

#[test]
fn blocking_execution_adapters_never_enter_the_shared_runtime() {
    for (name, source) in [
        ("javascript.rs", include_str!("../src/javascript.rs")),
        ("python.rs", include_str!("../src/python.rs")),
        ("wasm.rs", include_str!("../src/wasm.rs")),
    ] {
        assert!(
            !source.contains(".block_on("),
            "{name} blocking adapters must wait on dual-mode bounded mailboxes, not enter Tokio"
        );
    }
}

#[test]
fn supplied_vm_runtime_is_forwarded_to_per_execution_paths() {
    let javascript = include_str!("../src/javascript.rs");
    assert!(javascript.contains("pub fn start_execution_with_runtime("));
    assert!(javascript.contains("pub fn start_execution_with_module_reader_and_runtime("));
    assert!(javascript.contains("spawn_v8_event_bridge(\n            &runtime,"));
    assert!(javascript.contains(
        "create_session_from_command_with_runtime(\n                    command,\n                    &runtime,\n                    reactor_work_quantum,"
    ));
    assert!(javascript.contains(
        ".ensure_materialized_with_timeout_and_runtime(\n                &process_runtime,"
    ));

    let python = include_str!("../src/python.rs");
    assert!(python.contains("pub fn start_execution_with_runtime("));
    assert!(python.contains("pub async fn start_execution_with_runtime_async("));
    assert!(python.contains("pub async fn bundled_pyodide_dist_path_for_vm_async("));
    assert!(python.contains(
        "start_python_javascript_execution(\n            &mut self.javascript_engine,\n            &runtime,"
    ));
    assert!(python.contains(
        "prewarm_python_path(\n                import_cache,\n                &mut self.javascript_engine,"
    ));
    assert!(python.contains(
        ".ensure_materialized_with_timeout_and_runtime_async(\n                    &runtime,"
    ));
    assert!(python.contains("prewarm_python_path_async("));
    assert!(!python.contains("let process_runtime = self.runtime_context()?.clone();"));

    let wasm = include_str!("../src/wasm.rs");
    assert!(wasm.contains("pub fn start_execution_with_runtime("));
    assert!(wasm.contains("pub async fn start_execution_with_runtime_async("));
    assert!(wasm.contains(
        "start_wasm_javascript_execution(\n            &mut self.javascript_engine,\n            &runtime,"
    ));
    assert!(wasm.contains("runtime: &runtime,"));
    assert!(!wasm.contains("let process_runtime = self.runtime_context()?.clone();"));
}

#[test]
fn default_engine_does_not_silently_select_process_topology() {
    let mut javascript = JavascriptExecutionEngine::default();
    let error = javascript
        .materialize_import_cache_for_vm("vm-unbound")
        .expect_err("an unbound engine must reject runtime-dependent work");

    assert!(
        error
            .to_string()
            .contains("ERR_AGENTOS_RUNTIME_NOT_INJECTED"),
        "unexpected error: {error}"
    );

    let mut python = PythonExecutionEngine::default();
    let error = python
        .bundled_pyodide_dist_path_for_vm("vm-unbound")
        .expect_err("an unbound Python engine must reject runtime-dependent work");
    assert!(
        error
            .to_string()
            .contains("ERR_AGENTOS_RUNTIME_NOT_INJECTED"),
        "unexpected error: {error}"
    );

    let mut wasm = WasmExecutionEngine::default();
    let error = wasm
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-unbound"),
            context_id: String::from("missing-context"),
            managed_kernel_host: false,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: PathBuf::from("/"),
            permission_tier: WasmPermissionTier::Isolated,
            limits: WasmExecutionLimits::default(),
            guest_runtime: GuestRuntimeConfig::default(),
        })
        .expect_err("an unbound WASM engine must reject runtime-dependent work");
    assert!(
        error
            .to_string()
            .contains("ERR_AGENTOS_RUNTIME_NOT_INJECTED"),
        "unexpected error: {error}"
    );
}

#[test]
fn injected_runtime_admits_materialization_to_shared_blocking_executor() {
    let runtime = support::runtime_context();
    let cache_root = tempdir().expect("create cache root");
    let mut engine = JavascriptExecutionEngine::new(runtime.clone());
    engine.set_import_cache_base_dir("vm-injected", cache_root.path().to_path_buf());

    let cache_path = engine
        .materialize_import_cache_for_vm("vm-injected")
        .expect("materialize import cache on injected runtime");
    assert!(
        cache_path.parent().is_some_and(std::path::Path::is_dir),
        "materialization must create the cache root"
    );

    let metrics = runtime.metrics().snapshot();
    assert!(
        metrics.buffers[BufferMetricClass::Executor.index()].high_water >= 64 * 1024,
        "cache materialization must be charged to the bounded shared executor"
    );
}
