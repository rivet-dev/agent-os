#![forbid(unsafe_code)]

// AGENTOS_BROWSER_SUPPORT_DISABLED: retained for reference, but AgentOS is native-only.
/*
//! Browser-side sidecar scaffold for the agentos runtime migration.

mod service;
#[cfg(target_arch = "wasm32")]
mod wasm;
pub mod wire_dispatch;

pub use service::{
    BrowserExecutionOptions, BrowserExtension, BrowserExtensionContext, BrowserExtensionHost,
    BrowserExtensionRequest, BrowserExtensionResponse, BrowserSidecar, BrowserSidecarConfig,
    BrowserSidecarError,
};
#[cfg(target_arch = "wasm32")]
pub use wasm::{BrowserJsBridge, BrowserSidecarWasm};

use agentos_bridge::{BridgeTypes, GuestRuntime, HostBridge};
use agentos_sidecar_protocol::wire::WasmPermissionTier;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserWorkerEntrypoint {
    JavaScript { bootstrap_module: Option<String> },
    WebAssembly { module_path: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserWorkerSpawnRequest {
    pub vm_id: String,
    pub context_id: String,
    pub execution_id: String,
    pub runtime: GuestRuntime,
    pub entrypoint: BrowserWorkerEntrypoint,
    pub wasm_permission_tier: Option<WasmPermissionTier>,
    pub process_config: BrowserWorkerProcessConfig,
    pub os_config: BrowserWorkerOsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserWorkerProcessConfig {
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub platform: String,
    pub arch: String,
    pub version: String,
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserWorkerOsConfig {
    pub platform: String,
    pub arch: String,
    pub r#type: String,
    pub release: String,
    pub version: String,
    pub cpu_count: u64,
    pub totalmem: u64,
    pub freemem: u64,
    pub hostname: String,
    pub homedir: String,
    pub tmpdir: String,
    pub machine: String,
    pub user: String,
    pub shell: String,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserWorkerHandle {
    pub worker_id: String,
    pub runtime: GuestRuntime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserWorkerHandleRequest {
    pub vm_id: String,
    pub execution_id: String,
    pub worker_id: String,
}

pub trait BrowserHostBridge: HostBridge {}

impl<T> BrowserHostBridge for T where T: HostBridge {}

pub trait BrowserWorkerBridge: BridgeTypes {
    fn create_worker(
        &mut self,
        request: BrowserWorkerSpawnRequest,
    ) -> Result<BrowserWorkerHandle, Self::Error>;

    fn terminate_worker(&mut self, request: BrowserWorkerHandleRequest) -> Result<(), Self::Error>;
}

pub trait BrowserSidecarBridge: BrowserHostBridge + BrowserWorkerBridge {}

impl<T> BrowserSidecarBridge for T where T: BrowserHostBridge + BrowserWorkerBridge {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserSidecarScaffold {
    pub package_name: &'static str,
    pub kernel_package: &'static str,
    pub execution_host_thread: &'static str,
    pub guest_worker_owner_thread: &'static str,
}

pub fn scaffold() -> BrowserSidecarScaffold {
    let kernel = agentos_kernel::scaffold();

    BrowserSidecarScaffold {
        package_name: env!("CARGO_PKG_NAME"),
        kernel_package: kernel.package_name,
        execution_host_thread: "main",
        guest_worker_owner_thread: "main",
    }
}
*/
