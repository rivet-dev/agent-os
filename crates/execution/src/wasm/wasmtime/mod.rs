//! Wasmtime standalone-WebAssembly backend.
//!
//! This module is an ABI adapter over AgentOS host capabilities. It never owns
//! filesystem, descriptor, socket, process, terminal, signal, identity, or
//! permission semantics, and it deliberately does not construct a
//! `wasmtime-wasi` context.

mod cache;
mod engine;
mod error;
mod lifecycle;
mod limits;
mod linker;
mod memory;
mod module;
mod store;

pub use engine::{
    WasmtimeEngineHandle, WasmtimeEngineProfile, WasmtimeEngineRegistry, WasmtimeFeatureProfile,
    WasmtimeMetricsSnapshot, DEFAULT_WASM_STACK_BYTES, HOST_CALL_STACK_HEADROOM_BYTES,
};
pub use lifecycle::{WasmtimeExecution, WasmtimeExecutionEngine};
pub use limits::DEFAULT_TABLE_ACCOUNTING_BYTES;

pub const PINNED_WASMTIME_VERSION: &str = "46.0.0";
pub const TRUSTED_INITIAL_MODULE_PREFIX: &str = "agentos-trusted-initial:";
