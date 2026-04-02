#![forbid(unsafe_code)]

//! Browser-side sidecar scaffold for the Agent OS runtime migration.

mod service;

pub use service::{BrowserSidecar, BrowserSidecarConfig, BrowserSidecarError};

use agent_os_bridge::{BridgeTypes, GuestRuntime, HostBridge};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserWorkerEntrypoint {
    JavaScript { bootstrap_module: Option<String> },
    WebAssembly { module_path: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserWorkerSpawnRequest {
    pub vm_id: String,
    pub context_id: String,
    pub runtime: GuestRuntime,
    pub entrypoint: BrowserWorkerEntrypoint,
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
    let kernel = agent_os_kernel::scaffold();

    BrowserSidecarScaffold {
        package_name: env!("CARGO_PKG_NAME"),
        kernel_package: kernel.package_name,
        execution_host_thread: "main",
        guest_worker_owner_thread: "main",
    }
}
