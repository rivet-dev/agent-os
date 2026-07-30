#![forbid(unsafe_code)]
#![cfg_attr(
    not(any(
        feature = "node-v8",
        feature = "python-v8-pyodide",
        feature = "wasm-v8",
        feature = "wasm-wasmtime"
    )),
    allow(dead_code, unused_imports, unused_variables)
)]

//! Embeddable agentOS VM orchestration and kernel composition.

pub(crate) mod bootstrap;
pub(crate) mod bridge;
// Pure-Rust AES cipher primitives (RustCrypto) replacing the OpenSSL `Crypter`.
pub(crate) mod bindings;
#[doc(hidden)]
pub mod core;
pub(crate) mod crypto_cipher;
mod embedded;
pub(crate) mod execution;
#[doc(hidden)]
pub mod executor;
mod executor_registry;
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
pub(crate) mod vm;
pub mod vm_sqlite;
pub use agentos_driver_tokio as driver;
pub use agentos_sidecar_protocol::{generated_protocol, protocol, wire};

pub use agentos_vm_config::CreateVmConfig;
pub use embedded::{VmConfig, VmHandle, VmKernel, VmManagerBuilder};
pub use executor_registry::{ExecutorKind, ExecutorRegistry};
pub use extension::{
    Extension, ExtensionContext, ExtensionFuture, ExtensionInterruptRequest,
    ExtensionInterruptResponse, ExtensionResponse,
};
pub use service::{DispatchResult, VmError, VmManager, VmManagerConfig};
pub use state::EventSinkTransport;
pub use state::SidecarRequestTransport;

use wire::{DEFAULT_MAX_FRAME_BYTES, PROTOCOL_NAME, PROTOCOL_VERSION};

pub trait VmManagerHost: agentos_vm_host_interface::VmHost {}

impl<T> VmManagerHost for T where T: agentos_vm_host_interface::VmHost {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmScaffold {
    pub package_name: &'static str,
    pub kernel_package: &'static str,
    pub execution_package: &'static str,
    pub protocol_name: &'static str,
    pub protocol_version: u16,
    pub max_frame_bytes: usize,
}

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
