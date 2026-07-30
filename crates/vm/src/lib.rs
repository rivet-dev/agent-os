#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "runtime"), allow(dead_code))]

//! Embeddable agentOS VM orchestration and kernel composition.

#[cfg(feature = "runtime")]
pub(crate) mod bindings;
#[cfg(feature = "runtime")]
pub(crate) mod bootstrap;
#[cfg(feature = "runtime")]
pub(crate) mod bridge;
#[cfg(feature = "runtime")]
#[doc(hidden)]
pub mod core;
#[cfg(feature = "runtime")]
// Pure-Rust AES cipher primitives (RustCrypto) replacing the OpenSSL `Crypter`.
pub(crate) mod crypto_cipher;
#[cfg(feature = "runtime")]
mod embedded;
#[cfg(not(feature = "runtime"))]
mod embedded_minimal;
#[cfg(feature = "runtime")]
pub(crate) mod execution;
#[cfg(feature = "runtime")]
#[doc(hidden)]
pub mod executor;
#[cfg(feature = "runtime")]
mod executor_registry;
#[cfg(feature = "runtime")]
pub mod extension;
#[cfg(feature = "runtime")]
pub(crate) mod filesystem;
#[cfg(feature = "runtime")]
#[allow(dead_code)]
pub(crate) mod json_rpc;
#[cfg(feature = "runtime")]
pub(crate) mod language_execution;
#[cfg(feature = "runtime")]
pub mod limits;
#[cfg(feature = "runtime")]
pub(crate) mod metadata;
#[cfg(feature = "runtime")]
pub mod package_projection;
#[cfg(feature = "runtime")]
pub(crate) mod plugins;
#[cfg(feature = "runtime")]
pub mod service;
#[cfg(feature = "runtime")]
pub(crate) mod state;
#[cfg(feature = "runtime")]
pub(crate) mod vm;
#[cfg(feature = "runtime")]
pub mod vm_sqlite;
#[cfg(all(feature = "runtime", not(feature = "wasm-api")))]
mod wasm_disabled;
#[cfg(feature = "runtime")]
pub use agentos_driver_tokio as driver;
#[cfg(feature = "runtime")]
pub use agentos_sidecar_protocol::{generated_protocol, protocol, wire};

#[cfg(feature = "runtime")]
pub use agentos_vm_config::CreateVmConfig;
#[cfg(feature = "runtime")]
pub use embedded::{VmConfig, VmHandle, VmKernel, VmManagerBuilder};
#[cfg(not(feature = "runtime"))]
pub use embedded_minimal::{
    ExecutorKind, ExecutorRegistry, VmConfig, VmError, VmHandle, VmKernel, VmManager,
    VmManagerBuilder, VmManagerConfig,
};
#[cfg(feature = "runtime")]
pub use executor_registry::{ExecutorKind, ExecutorRegistry};
#[cfg(feature = "runtime")]
pub use extension::{
    Extension, ExtensionContext, ExtensionFuture, ExtensionInterruptRequest,
    ExtensionInterruptResponse, ExtensionResponse,
};
#[cfg(feature = "runtime")]
pub use service::{DispatchResult, VmError, VmManager, VmManagerConfig};
#[cfg(feature = "runtime")]
pub use state::EventSinkTransport;
#[cfg(feature = "runtime")]
pub use state::SidecarRequestTransport;

#[cfg(feature = "runtime")]
use wire::{DEFAULT_MAX_FRAME_BYTES, PROTOCOL_NAME, PROTOCOL_VERSION};

#[cfg(feature = "runtime")]
pub trait VmManagerHost: agentos_vm_host_interface::VmHost {}

#[cfg(feature = "runtime")]
impl<T> VmManagerHost for T where T: agentos_vm_host_interface::VmHost {}

#[cfg(feature = "runtime")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmScaffold {
    pub package_name: &'static str,
    pub kernel_package: &'static str,
    pub execution_package: &'static str,
    pub protocol_name: &'static str,
    pub protocol_version: u16,
    pub max_frame_bytes: usize,
}

#[cfg(feature = "runtime")]
pub fn scaffold() -> VmScaffold {
    let kernel = agentos_vm_kernel::scaffold();

    VmScaffold {
        package_name: "agentos-vm",
        kernel_package: kernel.package_name,
        execution_package: "agentos-executor-contract",
        protocol_name: PROTOCOL_NAME,
        protocol_version: PROTOCOL_VERSION,
        max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
    }
}
