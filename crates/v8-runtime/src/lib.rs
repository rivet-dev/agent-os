pub mod bridge;
pub mod embedded_runtime;
pub mod execution;
pub mod host_call;
pub mod ipc;
pub mod ipc_binary;
pub mod isolate;
pub mod runtime_protocol;
pub mod session;
pub mod snapshot;
pub mod stream;
pub mod timeout;

#[cfg(test)]
pub(crate) fn test_runtime_context() -> agentos_runtime::RuntimeContext {
    // Rust runs this crate's unit tests in parallel, while `SidecarRuntime::process`
    // deliberately shares one process-wide executor admission counter. Give the
    // ordinary unit-test process enough aggregate capacity that unrelated test
    // SessionManagers do not contend with each other. Tests for configured and
    // cross-manager saturation use isolated subprocesses with explicit small
    // limits and must not call this helper.
    const TEST_PROCESS_VM_EXECUTOR_LIMIT: usize = 64;
    let config = agentos_runtime::RuntimeConfig {
        max_active_vm_executors: TEST_PROCESS_VM_EXECUTOR_LIMIT,
        ..agentos_runtime::RuntimeConfig::default()
    };
    let runtime = agentos_runtime::SidecarRuntime::process(&config)
        .expect("test process runtime")
        .context();
    assert_eq!(
        runtime.max_active_vm_executors(),
        TEST_PROCESS_VM_EXECUTOR_LIMIT,
        "ordinary V8 unit tests must share the explicit test process quota"
    );
    runtime
}
