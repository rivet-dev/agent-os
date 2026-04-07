// IPC message types used by the execution engine.
//
// The binary header wire format (ipc_binary.rs) handles serialization;
// these types are used in-process only (no serde needed).

use std::collections::HashMap;

/// Process configuration injected into the V8 global as _processConfig
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessConfig {
    pub cwd: String,
    pub env: HashMap<String, String>,
    pub timing_mitigation: String,
    pub frozen_time_ms: Option<f64>,
}

/// OS configuration injected into the V8 global as _osConfig
#[derive(Debug, Clone, PartialEq)]
pub struct OsConfig {
    pub homedir: String,
    pub tmpdir: String,
    pub platform: String,
    pub arch: String,
}

/// Structured error information from V8 execution
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionError {
    pub error_type: String,
    pub message: String,
    pub stack: String,
    pub code: Option<String>,
}
