use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Owned compatibility host-call envelope used by legacy synchronous guest
/// adapters. Native adapters should prefer typed [`crate::host::HostOperation`]
/// values, but both forms remain engine independent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRpcRequest {
    pub id: u64,
    pub method: String,
    pub args: Vec<Value>,
    pub raw_bytes_args: HashMap<usize, Vec<u8>>,
}

/// Per-execution guest identity and operating-system projection supplied by
/// the sidecar. Concrete executors translate these owned values into their
/// guest-specific bootstrap representation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestRuntimeConfig {
    pub virtual_pid: Option<u64>,
    pub virtual_ppid: Option<u64>,
    pub virtual_uid: Option<u64>,
    pub virtual_gid: Option<u64>,
    pub virtual_exec_path: Option<String>,
    pub os_cpu_count: Option<u64>,
    pub os_totalmem: Option<u64>,
    pub os_freemem: Option<u64>,
    pub os_homedir: Option<String>,
    pub os_hostname: Option<String>,
    pub os_tmpdir: Option<String>,
    pub os_type: Option<String>,
    pub os_release: Option<String>,
    pub os_version: Option<String>,
    pub os_machine: Option<String>,
    pub os_shell: Option<String>,
    pub os_user: Option<String>,
    pub high_resolution_time: bool,
    /// Optional code evaluated by V8-family adapters when creating a reusable
    /// snapshot. Non-V8 executors preserve but otherwise ignore this field.
    pub snapshot_userland_code: Option<String>,
}
