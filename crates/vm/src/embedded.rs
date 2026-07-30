use crate::service::{VmError, VmManager, VmManagerConfig};
use crate::state::{ConnectionState, SessionState};
use crate::{wire, ExecutorRegistry};
use agentos_vm_host_interface::LocalVmHost;
use std::collections::{BTreeMap, BTreeSet};

/// The kernel owned by a directly embedded VM.
///
/// This is the same authoritative kernel used by executor-backed VMs. Direct
/// embedders can use its process, descriptor, signal, mount, socket, and
/// snapshot APIs without starting an execution engine.
pub type VmKernel = agentos_vm_kernel::kernel::KernelVm<agentos_vm_kernel::mount_table::MountTable>;

/// Configuration for one directly embedded VM.
#[derive(Debug, Clone)]
pub struct VmConfig {
    pub runtime: wire::GuestRuntimeKind,
    pub create: agentos_vm_config::CreateVmConfig,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            runtime: wire::GuestRuntimeKind::WebAssembly,
            create: agentos_vm_config::CreateVmConfig::default(),
        }
    }
}

impl VmConfig {
    /// Grant the directly embedded VM every guest permission.
    ///
    /// VM permissions remain deny-by-default. Embedders must opt into this
    /// explicitly when they want an unrestricted virtual OS.
    pub fn allow_all(mut self) -> Self {
        use agentos_vm_config::{
            FsPermissionScope, PatternPermissionScope, PermissionMode, PermissionsPolicy,
        };

        self.create.permissions = Some(PermissionsPolicy {
            fs: Some(FsPermissionScope::Mode(PermissionMode::Allow)),
            network: Some(PatternPermissionScope::Mode(PermissionMode::Allow)),
            child_process: Some(PatternPermissionScope::Mode(PermissionMode::Allow)),
            process: Some(PatternPermissionScope::Mode(PermissionMode::Allow)),
            env: Some(PatternPermissionScope::Mode(PermissionMode::Allow)),
            binding: Some(PatternPermissionScope::Mode(PermissionMode::Allow)),
        });
        self
    }
}

/// Builder for an in-process VM manager.
///
/// This path starts no sidecar process and requires no client transport.
pub struct VmManagerBuilder {
    config: VmManagerConfig,
    driver: Option<agentos_driver_tokio::DriverHandle>,
    executors: ExecutorRegistry,
}

impl Default for VmManagerBuilder {
    fn default() -> Self {
        Self {
            config: VmManagerConfig {
                instance_id: String::from("agentos-embedded-vm"),
                ..VmManagerConfig::default()
            },
            driver: None,
            executors: ExecutorRegistry::empty(),
        }
    }
}

impl VmManagerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn driver(mut self, driver: agentos_driver_tokio::DriverHandle) -> Self {
        self.driver = Some(driver);
        self
    }

    pub fn executors(mut self, executors: ExecutorRegistry) -> Self {
        self.executors = executors;
        self
    }

    pub fn config(mut self, config: VmManagerConfig) -> Self {
        self.config = config;
        self
    }

    pub fn build(self) -> Result<VmManager<LocalVmHost>, VmError> {
        let driver = match self.driver {
            Some(driver) => driver,
            None => agentos_driver_tokio::TokioDriver::process(&self.config.runtime)
                .map_err(|error| VmError::InvalidState(error.to_string()))?
                .handle(),
        };
        VmManager::with_driver_and_executors(
            LocalVmHost::default(),
            self.config,
            driver,
            self.executors,
        )
    }
}

impl VmManager<LocalVmHost> {
    pub fn builder() -> VmManagerBuilder {
        VmManagerBuilder::new()
    }
}

/// A directly embedded VM borrowed from its manager.
///
/// The handle deliberately exposes VM operations rather than sidecar protocol
/// ownership or transport concepts.
pub struct VmHandle<'manager> {
    manager: &'manager mut VmManager<LocalVmHost>,
    connection_id: String,
    session_id: String,
    vm_id: String,
}

impl VmManager<LocalVmHost> {
    pub async fn create(&mut self, config: VmConfig) -> Result<VmHandle<'_>, VmError> {
        // This is a local lifecycle owner, not a sidecar connection. It is
        // inserted directly so the embedded API never authenticates, frames,
        // serializes through a transport, or constructs a client.
        let owner = uuid::Uuid::new_v4();
        let connection_id = format!("embedded-owner-{owner}");
        let session_id = format!("embedded-vms-{owner}");
        self.connections.insert(
            connection_id.clone(),
            ConnectionState {
                auth_token: String::new(),
                sessions: BTreeSet::from([session_id.clone()]),
            },
        );
        self.sessions.insert(
            session_id.clone(),
            SessionState {
                connection_id: connection_id.clone(),
                placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                    crate::protocol::SidecarPlacementShared { pool: None },
                ),
                metadata: BTreeMap::new(),
                vm_ids: BTreeSet::new(),
            },
        );

        config
            .create
            .validate(self.config.max_frame_bytes)
            .map_err(|error| VmError::InvalidState(format!("invalid create VM config: {error}")))?;
        let (vm_id, _) = self
            .create_vm_owned(
                connection_id.clone(),
                session_id.clone(),
                config.runtime,
                config.create,
            )
            .await?;

        Ok(VmHandle {
            manager: self,
            connection_id,
            session_id,
            vm_id,
        })
    }
}

impl VmHandle<'_> {
    pub fn id(&self) -> &str {
        &self.vm_id
    }

    pub async fn write_file(&mut self, path: &str, contents: &[u8]) -> Result<(), VmError> {
        self.vm_mut()?
            .kernel
            .write_file(path, contents.to_vec())
            .map_err(embedded_kernel_error)
    }

    pub async fn read_file(&mut self, path: &str) -> Result<Vec<u8>, VmError> {
        self.vm_mut()?
            .kernel
            .read_file(path)
            .map_err(embedded_kernel_error)
    }

    /// Returns the authoritative virtual kernel for direct OS operations.
    pub fn kernel(&self) -> Result<&VmKernel, VmError> {
        Ok(&self.vm()?.kernel)
    }

    /// Returns the authoritative virtual kernel for mutating OS operations.
    pub fn kernel_mut(&mut self) -> Result<&mut VmKernel, VmError> {
        Ok(&mut self.vm_mut()?.kernel)
    }

    /// Disposes this VM and releases all of its kernel, storage, and resource
    /// state.
    pub async fn dispose(self) -> Result<(), VmError> {
        let Self {
            manager,
            connection_id,
            session_id,
            vm_id,
        } = self;
        let result = manager
            .dispose_vm_internal(
                &connection_id,
                &session_id,
                &vm_id,
                crate::protocol::DisposeReason::Requested,
            )
            .await
            .map(|_| ());
        manager.sessions.remove(&session_id);
        manager.connections.remove(&connection_id);
        result
    }

    fn vm(&self) -> Result<&crate::state::VmState, VmError> {
        let vm = self
            .manager
            .vms
            .get(&self.vm_id)
            .ok_or_else(|| VmError::InvalidState(format!("unknown VM {}", self.vm_id)))?;
        if vm.connection_id != self.connection_id || vm.session_id != self.session_id {
            return Err(VmError::InvalidState(format!(
                "VM {} is no longer owned by this embedded handle",
                self.vm_id
            )));
        }
        Ok(vm)
    }

    fn vm_mut(&mut self) -> Result<&mut crate::state::VmState, VmError> {
        let vm = self
            .manager
            .vms
            .get_mut(&self.vm_id)
            .ok_or_else(|| VmError::InvalidState(format!("unknown VM {}", self.vm_id)))?;
        if vm.connection_id != self.connection_id || vm.session_id != self.session_id {
            return Err(VmError::InvalidState(format!(
                "VM {} is no longer owned by this embedded handle",
                self.vm_id
            )));
        }
        Ok(vm)
    }
}

fn embedded_kernel_error(error: agentos_vm_kernel::kernel::KernelError) -> VmError {
    VmError::Host(agentos_executor_contract::backend::HostServiceError::new(
        error.code(),
        error.message(),
    ))
}
