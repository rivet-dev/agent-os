use agentos_execution::backend::ExecutionBackend;
use agentos_execution::{JavascriptExecution, PythonExecution, WasmExecution};

fn assert_execution_backend<T: ExecutionBackend>() {}

#[test]
fn every_production_execution_adapter_implements_the_lifecycle_contract() {
    assert_execution_backend::<JavascriptExecution>();
    assert_execution_backend::<PythonExecution>();
    assert_execution_backend::<WasmExecution>();
}
