use agentos_vm_kernel::kernel::{KernelError, KernelVm, KernelVmConfig};
use agentos_vm_kernel::mount_table::MountTable;
use agentos_vm_kernel::permissions::Permissions;
use agentos_vm_kernel::root_fs::{RootFileSystem, RootFilesystemError};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

/// The authoritative kernel owned by a directly embedded VM.
pub type VmKernel = KernelVm<MountTable>;

/// A concrete execution engine that a full VM runtime may provide.
///
/// The executor-free build retains these names only so configuration can fail
/// with a stable typed error instead of silently accepting an unavailable
/// engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExecutorKind {
    NodeV8,
    PythonV8Pyodide,
    WasmV8,
    WasmWasmtime,
    WasmWasmtimeThreads,
}

impl ExecutorKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::NodeV8 => "node-v8",
            Self::PythonV8Pyodide => "python-v8-pyodide",
            Self::WasmV8 => "wasm-v8",
            Self::WasmWasmtime => "wasm-wasmtime",
            Self::WasmWasmtimeThreads => "wasm-wasmtime-threads",
        }
    }
}

/// Executor availability for an embedded VM manager.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecutorRegistry {
    available: BTreeSet<ExecutorKind>,
}

impl ExecutorRegistry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn with(mut self, executor: ExecutorKind) -> Self {
        self.available.insert(executor);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.available.is_empty()
    }
}

/// Error returned by the executor-free embedded VM facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmError {
    code: &'static str,
    message: String,
}

impl VmError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn executor_unavailable(executor: ExecutorKind) -> Self {
        Self::new(
            "ERR_AGENTOS_EXECUTOR_UNAVAILABLE",
            format!(
                "the {} executor is not compiled into this embedded VM",
                executor.name()
            ),
        )
    }
}

impl fmt::Display for VmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for VmError {}

impl From<KernelError> for VmError {
    fn from(error: KernelError) -> Self {
        Self::new(error.code(), error.message())
    }
}

impl From<RootFilesystemError> for VmError {
    fn from(error: RootFilesystemError) -> Self {
        Self::new("EIO", error.to_string())
    }
}

/// Process-level configuration for the embedded VM manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmManagerConfig {
    pub instance_id: String,
}

impl Default for VmManagerConfig {
    fn default() -> Self {
        Self {
            instance_id: String::from("agentos-embedded-vm"),
        }
    }
}

/// Configuration for one executor-free embedded VM.
#[derive(Clone)]
pub struct VmConfig {
    vm_id: Option<String>,
    permissions: Permissions,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            vm_id: None,
            permissions: Permissions::default(),
        }
    }
}

impl VmConfig {
    pub fn id(mut self, vm_id: impl Into<String>) -> Self {
        self.vm_id = Some(vm_id.into());
        self
    }

    /// Grants every kernel permission to the embedded caller.
    pub fn allow_all(mut self) -> Self {
        self.permissions = Permissions::allow_all();
        self
    }
}

/// Builder for an in-process, executor-free VM manager.
#[derive(Default)]
pub struct VmManagerBuilder {
    config: VmManagerConfig,
    executors: ExecutorRegistry,
}

impl VmManagerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn executors(mut self, executors: ExecutorRegistry) -> Self {
        self.executors = executors;
        self
    }

    pub fn config(mut self, config: VmManagerConfig) -> Self {
        self.config = config;
        self
    }

    pub fn build(self) -> Result<VmManager, VmError> {
        if let Some(executor) = self.executors.available.iter().next().copied() {
            return Err(VmError::executor_unavailable(executor));
        }
        Ok(VmManager {
            config: self.config,
            next_vm_id: 1,
        })
    }
}

/// Minimal in-process VM lifecycle owner.
#[derive(Debug)]
pub struct VmManager {
    config: VmManagerConfig,
    next_vm_id: u64,
}

impl VmManager {
    pub fn builder() -> VmManagerBuilder {
        VmManagerBuilder::new()
    }

    pub async fn create(&mut self, config: VmConfig) -> Result<VmHandle<'_>, VmError> {
        let vm_id = config.vm_id.unwrap_or_else(|| {
            let id = format!("{}-{}", self.config.instance_id, self.next_vm_id);
            self.next_vm_id = self.next_vm_id.saturating_add(1);
            id
        });
        let root = RootFileSystem::minimal_ephemeral()?;
        let mut kernel_config = KernelVmConfig::new(vm_id.clone());
        kernel_config.permissions = config.permissions;
        let mut kernel = KernelVm::new(MountTable::new(root), kernel_config);
        kernel.finish_root_filesystem_bootstrap()?;

        Ok(VmHandle {
            _manager: self,
            vm_id,
            kernel,
        })
    }
}

/// One directly embedded VM.
pub struct VmHandle<'manager> {
    _manager: &'manager mut VmManager,
    vm_id: String,
    kernel: VmKernel,
}

impl VmHandle<'_> {
    pub fn id(&self) -> &str {
        &self.vm_id
    }

    pub async fn write_file(&mut self, path: &str, contents: &[u8]) -> Result<(), VmError> {
        self.kernel
            .write_file(path, contents.to_vec())
            .map_err(VmError::from)
    }

    pub async fn read_file(&mut self, path: &str) -> Result<Vec<u8>, VmError> {
        self.kernel.read_file(path).map_err(VmError::from)
    }

    pub fn kernel(&self) -> Result<&VmKernel, VmError> {
        Ok(&self.kernel)
    }

    pub fn kernel_mut(&mut self) -> Result<&mut VmKernel, VmError> {
        Ok(&mut self.kernel)
    }

    pub async fn dispose(mut self) -> Result<(), VmError> {
        self.kernel.dispose().map_err(VmError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        struct ThreadWake(std::thread::Thread);

        impl std::task::Wake for ThreadWake {
            fn wake(self: std::sync::Arc<Self>) {
                self.0.unpark();
            }

            fn wake_by_ref(self: &std::sync::Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker = std::task::Waker::from(std::sync::Arc::new(ThreadWake(std::thread::current())));
        let mut context = std::task::Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(output) => return output,
                std::task::Poll::Pending => std::thread::park(),
            }
        }
    }

    #[test]
    fn embedded_vm_uses_only_the_authoritative_in_memory_kernel() {
        block_on(async {
            let mut manager = VmManager::builder().build().expect("build manager");
            let mut vm = manager
                .create(VmConfig::default().allow_all())
                .await
                .expect("create VM");
            vm.write_file("/workspace/minimal.txt", b"minimal")
                .await
                .expect("write");
            assert_eq!(
                vm.read_file("/workspace/minimal.txt").await.expect("read"),
                b"minimal"
            );
            assert!(vm.kernel().expect("kernel").list_processes().is_empty());
            vm.dispose().await.expect("dispose");
        });
    }

    #[test]
    fn executor_request_fails_with_typed_error() {
        let error = VmManager::builder()
            .executors(ExecutorRegistry::empty().with(ExecutorKind::WasmWasmtime))
            .build()
            .expect_err("executor-free build must reject an engine");
        assert_eq!(error.code(), "ERR_AGENTOS_EXECUTOR_UNAVAILABLE");
    }
}
