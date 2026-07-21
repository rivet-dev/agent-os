#![forbid(unsafe_code)]

//! Shared per-VM kernel plane for the agentos runtime migration.

pub use agentos_bridge as bridge;
pub mod command_registry;
pub mod device_layer;
pub mod dns;
pub mod fd_table;
pub mod kernel;
pub mod network_policy;
pub mod permissions;
pub mod pipe_manager;
pub mod poll;
pub mod process_table;
pub mod pty;
pub mod resource_accounting;
pub mod socket_table;
pub mod user;

pub use ::vfs::posix as vfs;

pub mod mount_plugin {
    pub use ::vfs::posix::mount_plugin::*;
}

pub mod mount_table {
    pub use ::vfs::posix::mount_table::*;
}

pub mod overlay_fs {
    pub use ::vfs::posix::overlay_fs::*;
}

pub mod root_fs {
    pub use ::vfs::posix::root_fs::*;
}

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
