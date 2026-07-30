#![deny(unsafe_code)]

//! Test-only cross-executor conformance surface.
//!
//! Production code must depend on `agentos-executor-contract` and the selected
//! concrete executor crates. This package exists only to keep parity tests
//! expressed once across the native sidecar's composed backends.

pub mod benchmark;

pub use agentos_native_sidecar::executor::*;

pub trait NativeExecutionBridge: agentos_bridge::ExecutionBridge {}

impl<T> NativeExecutionBridge for T where T: agentos_bridge::ExecutionBridge {}
