#![deny(unsafe_code)]

//! Engine-neutral WebAssembly support shared by the agentOS V8 and Wasmtime
//! executors.

pub mod abi;
mod execution;
pub mod profile;

pub use execution::*;
