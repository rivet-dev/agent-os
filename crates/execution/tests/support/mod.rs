#![allow(dead_code)]

use agentos_execution::{JavascriptExecutionEngine, PythonExecutionEngine, WasmExecutionEngine};
use agentos_runtime_tokio::{RuntimeConfig, RuntimeContext, SidecarRuntime};

pub fn runtime_context() -> RuntimeContext {
    SidecarRuntime::process(&RuntimeConfig::default())
        .expect("construct execution-test process runtime")
        .context()
}

pub fn javascript_engine() -> JavascriptExecutionEngine {
    JavascriptExecutionEngine::new(runtime_context())
}

pub fn python_engine() -> PythonExecutionEngine {
    PythonExecutionEngine::new(runtime_context())
}

pub fn wasm_engine() -> WasmExecutionEngine {
    WasmExecutionEngine::new(runtime_context())
}
