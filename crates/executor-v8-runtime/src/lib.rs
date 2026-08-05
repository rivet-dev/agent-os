extern crate self as agentos_executor_v8_runtime;

pub mod adapter_common;
pub mod adapter_host;
pub mod adapter_ipc;
pub mod adapter_runtime;
pub mod adapter_support;
pub mod asset_cache;
pub mod bridge;
pub mod embedded_runtime;
pub mod execution;
pub mod host_call;
pub mod host_node;
pub mod ipc;
pub mod ipc_binary;
pub mod isolate;
#[allow(dead_code, unused_imports)]
pub mod javascript;
pub mod runtime_protocol;
pub mod session;
pub mod snapshot;
pub mod stream;
pub mod timeout;

pub mod backend {
    pub use agentos_executor_contract::backend::*;
}

pub mod host {
    pub use agentos_executor_contract::host::*;
}

pub mod signal {
    pub use agentos_executor_contract::{
        ExecutionSignalDispositionAction, ExecutionSignalHandlerRegistration,
    };
}

pub const PYODIDE_AVAILABLE: bool = !cfg!(agentos_pyodide_unavailable);
pub const TYPESCRIPT_AVAILABLE: bool = !cfg!(agentos_typescript_unavailable);

#[cfg(test)]
pub(crate) fn test_runtime_context() -> agentos_driver_tokio::DriverHandle {
    // Rust runs this crate's unit tests in parallel, while `TokioDriver::process`
    // deliberately shares one process-wide executor admission counter. Give the
    // ordinary unit-test process enough aggregate capacity that unrelated test
    // SessionManagers do not contend with each other. Tests for configured and
    // cross-manager saturation use isolated subprocesses with explicit small
    // limits and must not call this helper.
    const TEST_PROCESS_VM_EXECUTOR_LIMIT: usize = 64;
    let config = agentos_driver_tokio::DriverConfig {
        max_active_vm_executors: TEST_PROCESS_VM_EXECUTOR_LIMIT,
        ..agentos_driver_tokio::DriverConfig::default()
    };
    let runtime = agentos_driver_tokio::TokioDriver::process(&config)
        .expect("test process runtime")
        .handle();
    assert_eq!(
        runtime.max_active_vm_executors(),
        TEST_PROCESS_VM_EXECUTOR_LIMIT,
        "ordinary V8 unit tests must share the explicit test process quota"
    );
    runtime
}
