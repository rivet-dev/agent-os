#![forbid(unsafe_code)]

//! Native execution plane scaffold for the Agent OS runtime migration.

mod common;
mod node_import_cache;
mod node_process;
mod runtime_support;

pub mod benchmark;
pub mod javascript;
pub mod python;
pub mod wasm;

pub use agent_os_bridge::GuestRuntime;
pub use javascript::{
    CreateJavascriptContextRequest, JavascriptContext, JavascriptExecution,
    JavascriptExecutionEngine, JavascriptExecutionError, JavascriptExecutionEvent,
    JavascriptExecutionResult, JavascriptSyncRpcRequest, StartJavascriptExecutionRequest,
};
pub use node_process::{NodeSignalDispositionAction, NodeSignalHandlerRegistration};
pub use python::{
    CreatePythonContextRequest, PythonContext, PythonExecution, PythonExecutionEngine,
    PythonExecutionError, PythonExecutionEvent, PythonExecutionResult, PythonVfsRpcMethod,
    PythonVfsRpcRequest, PythonVfsRpcResponsePayload, PythonVfsRpcStat,
    StartPythonExecutionRequest,
};
pub use wasm::{
    CreateWasmContextRequest, StartWasmExecutionRequest, WasmContext, WasmExecution,
    WasmExecutionEngine, WasmExecutionError, WasmExecutionEvent, WasmExecutionResult,
    WasmPermissionTier,
};

pub trait NativeExecutionBridge: agent_os_bridge::ExecutionBridge {}

impl<T> NativeExecutionBridge for T where T: agent_os_bridge::ExecutionBridge {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionScaffold {
    pub package_name: &'static str,
    pub kernel_package: &'static str,
    pub target: &'static str,
    pub planned_guest_runtimes: [GuestRuntime; 2],
}

pub fn scaffold() -> ExecutionScaffold {
    ExecutionScaffold {
        package_name: env!("CARGO_PKG_NAME"),
        kernel_package: "agent-os-kernel",
        target: "native",
        planned_guest_runtimes: [GuestRuntime::JavaScript, GuestRuntime::WebAssembly],
    }
}
