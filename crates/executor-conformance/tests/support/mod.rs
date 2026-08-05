#![allow(dead_code)]

use agentos_driver_tokio::{DriverConfig, DriverHandle, TokioDriver};
use agentos_executor_conformance::{
    JavascriptExecutionEngine, PythonExecutionEngine, WasmExecutionEngine,
};

pub fn runtime_context() -> DriverHandle {
    TokioDriver::process(&DriverConfig::default())
        .expect("construct execution-test process runtime")
        .handle()
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
