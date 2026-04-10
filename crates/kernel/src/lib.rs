#![forbid(unsafe_code)]

//! Shared per-VM kernel plane for the Agent OS runtime migration.

pub use agent_os_bridge as bridge;
pub mod command_registry;
pub mod device_layer;
pub mod fd_table;
pub mod kernel;
pub mod mount_plugin;
pub mod mount_table;
pub mod overlay_fs;
pub mod permissions;
pub mod pipe_manager;
pub mod poll;
pub mod process_table;
pub mod pty;
pub mod resource_accounting;
pub mod root_fs;
pub mod socket_table;
pub mod user;
pub mod vfs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelScaffold {
    pub package_name: &'static str,
    pub supports_native_sidecar: bool,
    pub supports_browser_sidecar: bool,
}

pub fn scaffold() -> KernelScaffold {
    KernelScaffold {
        package_name: env!("CARGO_PKG_NAME"),
        supports_native_sidecar: true,
        supports_browser_sidecar: true,
    }
}
