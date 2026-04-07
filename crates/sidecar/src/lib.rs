#![forbid(unsafe_code)]

//! Native sidecar scaffold that composes the kernel and execution crates.

pub(crate) mod bootstrap;
pub(crate) mod bridge;
pub(crate) mod execution;
pub(crate) mod filesystem;
pub(crate) mod plugins;
pub mod protocol;
pub mod service;
pub(crate) mod state;
pub(crate) mod vm;

pub use service::{DispatchResult, NativeSidecar, NativeSidecarConfig, SidecarError};
pub use state::SidecarRequestTransport;

use protocol::{DEFAULT_MAX_FRAME_BYTES, PROTOCOL_NAME, PROTOCOL_VERSION};

pub trait NativeSidecarBridge: agent_os_bridge::HostBridge {}

impl<T> NativeSidecarBridge for T where T: agent_os_bridge::HostBridge {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarScaffold {
    pub package_name: &'static str,
    pub binary_name: &'static str,
    pub kernel_package: &'static str,
    pub execution_package: &'static str,
    pub protocol_name: &'static str,
    pub protocol_version: u16,
    pub max_frame_bytes: usize,
}

pub fn scaffold() -> SidecarScaffold {
    let kernel = agent_os_kernel::scaffold();
    let execution = agent_os_execution::scaffold();

    SidecarScaffold {
        package_name: env!("CARGO_PKG_NAME"),
        binary_name: env!("CARGO_PKG_NAME"),
        kernel_package: kernel.package_name,
        execution_package: execution.package_name,
        protocol_name: PROTOCOL_NAME,
        protocol_version: PROTOCOL_VERSION,
        max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
    }
}
