#![forbid(unsafe_code)]

//! Native sidecar scaffold that composes the kernel and execution crates.

pub(crate) mod bootstrap;
pub(crate) mod bridge;
// Pure-Rust AES cipher primitives (RustCrypto) replacing the OpenSSL `Crypter`.
pub(crate) mod bindings;
pub(crate) mod crypto_cipher;
pub(crate) mod execution;
pub mod extension;
pub(crate) mod filesystem;
#[allow(dead_code)]
pub(crate) mod json_rpc;
pub(crate) mod language_execution;
pub mod limits;
pub(crate) mod metadata;
pub mod package_projection;
pub(crate) mod plugins;
pub mod service;
pub(crate) mod state;
pub mod stdio;
pub(crate) mod vm;
pub mod vm_sqlite;
pub use agentos_sidecar_protocol::{generated_protocol, protocol, wire};

pub use extension::{
    Extension, ExtensionContext, ExtensionFuture, ExtensionInterruptRequest,
    ExtensionInterruptResponse, ExtensionResponse,
};
pub use service::{DispatchResult, NativeSidecar, NativeSidecarConfig, SidecarError};
pub use state::EventSinkTransport;
pub use state::SidecarRequestTransport;

use wire::{DEFAULT_MAX_FRAME_BYTES, PROTOCOL_NAME, PROTOCOL_VERSION};

pub trait NativeSidecarBridge: agentos_bridge::HostBridge {}

impl<T> NativeSidecarBridge for T where T: agentos_bridge::HostBridge {}

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
    let kernel = agentos_kernel::scaffold();
    let execution = agentos_execution::scaffold();

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
