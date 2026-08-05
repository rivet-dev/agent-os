use crate::{
    BrowserSidecarBridge, BrowserWorkerEntrypoint, BrowserWorkerHandle, BrowserWorkerHandleRequest,
    BrowserWorkerOsConfig, BrowserWorkerProcessConfig, BrowserWorkerSpawnRequest,
};
use agentos_bridge::{
    BridgeTypes, CreateJavascriptContextRequest, CreateWasmContextRequest, ExecutionEvent,
    ExecutionHandleRequest, GuestContextHandle, GuestRuntime, KillExecutionRequest,
    LifecycleEventRecord, LifecycleState, PollExecutionEventRequest, SignalDispositionAction,
    SignalHandlerRegistration, StartExecutionRequest, StartedExecution, StructuredEventRecord,
    WriteExecutionStdinRequest,
};
use agentos_kernel::bridge::LifecycleState as KernelLifecycleState;
use agentos_kernel::kernel::{KernelError, KernelVm, KernelVmConfig, VirtualProcessOptions};
use agentos_kernel::mount_table::MountTable;
use agentos_kernel::poll::{PollTargetEntry, POLLERR, POLLHUP, POLLIN};
use agentos_kernel::process_table::ProcessStatus;
use agentos_kernel::root_fs::{
    RootFilesystemMode as KernelRootFilesystemMode, RootFilesystemSnapshot,
};
use agentos_kernel::socket_table::{
    InetSocketAddress, SocketId, SocketSpec, SocketState, SocketType,
};
use agentos_native_sidecar_core::{
    apply_process_signal_state_update, build_root_mount_table,
    ensure_vm_fetch_raw_response_buffer_within_limit, ensure_vm_fetch_response_within_limit,
    handle_guest_filesystem_call, handle_guest_kernel_call, parse_kernel_http_fetch_response,
    process_snapshot_entry_from_kernel, serialize_kernel_http_fetch_request,
    unsupported_guest_kernel_call_detail, SharedProcessSnapshotEntry, VmLayerStore,
    VM_FETCH_BUFFER_LIMIT_BYTES,
};
use agentos_native_sidecar_core::{
    ensure_binding_registry_capacity, ensure_collection_name_available,
    ensure_command_aliases_available, registered_binding_command_names,
    shared_guest_runtime_identity, validate_bindings_registration,
};
use agentos_sidecar_protocol::protocol::{
    FindBoundUdpRequest, FindListenerRequest, GuestFilesystemCallRequest,
    GuestFilesystemResultResponse, GuestKernelCallRequest, GuestKernelResultResponse,
    ProjectedModuleDescriptor, RegisterHostCallbacksRequest, RootFilesystemEntry,
    SignalDispositionAction as ProtocolSignalDispositionAction,
    SignalHandlerRegistration as ProtocolSignalHandlerRegistration, SocketStateEntry,
    WasmPermissionTier,
};
use agentos_vm_config::RootFilesystemConfig;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::time::{Duration, Instant};

type BridgeError<B> = <B as BridgeTypes>::Error;
type BrowserKernel = KernelVm<MountTable>;
const BROWSER_WORKER_DRIVER: &str = "browser.worker";
const BROWSER_VM_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(target_arch = "wasm32"))]
const BROWSER_VM_FETCH_TIMEOUT_MS_ENV: &str = "AGENTOS_TEST_BROWSER_VM_FETCH_TIMEOUT_MS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSidecarConfig {
    pub sidecar_id: String,
}

impl Default for BrowserSidecarConfig {
    fn default() -> Self {
        Self {
            sidecar_id: String::from("agentos-native-sidecar-browser"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserSidecarError {
    InvalidState(String),
    Kernel(String),
    Bridge(String),
}

impl fmt::Display for BrowserSidecarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(message) | Self::Kernel(message) | Self::Bridge(message) => {
                f.write_str(message)
            }
        }
    }
}

impl Error for BrowserSidecarError {}

struct VmState {
    kernel: BrowserKernel,
    configuration: BrowserVmConfiguration,
    layers: VmLayerStore,
    bindings: BTreeMap<String, RegisterHostCallbacksRequest>,
    signal_states: BTreeMap<String, BTreeMap<u32, ProtocolSignalHandlerRegistration>>,
    contexts: BTreeSet<String>,
    active_executions: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BrowserVmConfiguration {
    instructions: Vec<String>,
    projected_modules: Vec<ProjectedModuleDescriptor>,
    command_permissions: BTreeMap<String, WasmPermissionTier>,
    create_loopback_exempt_ports: BTreeSet<u16>,
    loopback_exempt_ports: Vec<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserExecutionOptions {
    pub command_name: Option<String>,
    pub wasm_permission_tier: Option<WasmPermissionTier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextState {
    vm_id: String,
    runtime: GuestRuntime,
    entrypoint: BrowserWorkerEntrypoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutionState {
    vm_id: String,
    worker: BrowserWorkerHandle,
    kernel_pid: u32,
    stdin_write_fd: u32,
    cwd: String,
}

pub trait BrowserExtension: Send + Sync {
    fn namespace(&self) -> &str;

    fn handle_request(
        &self,
        context: &mut BrowserExtensionContext<'_>,
        payload: &[u8],
    ) -> Result<Vec<u8>, BrowserSidecarError> {
        let _ = context;
        let _ = payload;
        Err(BrowserSidecarError::InvalidState(format!(
            "browser extension {} does not handle requests",
            self.namespace()
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserExtensionRequest {
    pub namespace: String,
    pub payload: Vec<u8>,
    /// VM the request is scoped to (from the wire ownership scope), so extensions
    /// that drive guest executions know which VM to target. `None` for connection/
    /// session-scoped requests that carry no VM.
    pub vm_id: Option<String>,
    /// Owning connection (from the wire ownership scope), for per-connection
    /// ownership enforcement inside extensions.
    pub connection_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserExtensionResponse {
    pub namespace: String,
    pub payload: Vec<u8>,
}

pub trait BrowserExtensionHost {
    fn write_file(
        &mut self,
        vm_id: &str,
        path: &str,
        contents: Vec<u8>,
    ) -> Result<(), BrowserSidecarError>;

    fn read_file(&mut self, vm_id: &str, path: &str) -> Result<Vec<u8>, BrowserSidecarError>;

    fn mkdir(
        &mut self,
        vm_id: &str,
        path: &str,
        recursive: bool,
    ) -> Result<(), BrowserSidecarError>;

    fn read_dir(&mut self, vm_id: &str, path: &str) -> Result<Vec<String>, BrowserSidecarError>;

    fn create_javascript_context(
        &mut self,
        request: CreateJavascriptContextRequest,
    ) -> Result<GuestContextHandle, BrowserSidecarError>;

    fn create_wasm_context(
        &mut self,
        request: CreateWasmContextRequest,
    ) -> Result<GuestContextHandle, BrowserSidecarError>;

    fn start_execution(
        &mut self,
        request: StartExecutionRequest,
    ) -> Result<StartedExecution, BrowserSidecarError>;

    fn write_stdin(
        &mut self,
        request: WriteExecutionStdinRequest,
    ) -> Result<(), BrowserSidecarError>;

    fn close_stdin(&mut self, request: ExecutionHandleRequest) -> Result<(), BrowserSidecarError>;

    fn kill_execution(&mut self, request: KillExecutionRequest) -> Result<(), BrowserSidecarError>;

    fn poll_execution_event(
        &mut self,
        request: PollExecutionEventRequest,
    ) -> Result<Option<ExecutionEvent>, BrowserSidecarError>;
}

pub struct BrowserExtensionContext<'a> {
    host: &'a mut dyn BrowserExtensionHost,
    vm_id: Option<String>,
    connection_id: Option<String>,
}

impl<'a> BrowserExtensionContext<'a> {
    pub fn new(host: &'a mut dyn BrowserExtensionHost) -> Self {
        Self {
            host,
            vm_id: None,
            connection_id: None,
        }
    }

    /// Construct with the wire ownership scope threaded in (VM + connection), so
    /// extensions can target the right VM and enforce per-connection ownership.
    pub fn with_ownership(
        host: &'a mut dyn BrowserExtensionHost,
        vm_id: Option<String>,
        connection_id: Option<String>,
    ) -> Self {
        Self {
            host,
            vm_id,
            connection_id,
        }
    }

    /// VM this request is scoped to, if any.
    pub fn vm_id(&self) -> Option<&str> {
        self.vm_id.as_deref()
    }

    /// Owning connection of this request, if any.
    pub fn connection_id(&self) -> Option<&str> {
        self.connection_id.as_deref()
    }

    pub fn write_file(
        &mut self,
        vm_id: &str,
        path: &str,
        contents: impl Into<Vec<u8>>,
    ) -> Result<(), BrowserSidecarError> {
        self.host.write_file(vm_id, path, contents.into())
    }

    pub fn read_file(&mut self, vm_id: &str, path: &str) -> Result<Vec<u8>, BrowserSidecarError> {
        self.host.read_file(vm_id, path)
    }

    pub fn mkdir(
        &mut self,
        vm_id: &str,
        path: &str,
        recursive: bool,
    ) -> Result<(), BrowserSidecarError> {
        self.host.mkdir(vm_id, path, recursive)
    }

    pub fn read_dir(
        &mut self,
        vm_id: &str,
        path: &str,
    ) -> Result<Vec<String>, BrowserSidecarError> {
        self.host.read_dir(vm_id, path)
    }

    pub fn create_javascript_context(
        &mut self,
        request: CreateJavascriptContextRequest,
    ) -> Result<GuestContextHandle, BrowserSidecarError> {
        self.host.create_javascript_context(request)
    }

    pub fn create_wasm_context(
        &mut self,
        request: CreateWasmContextRequest,
    ) -> Result<GuestContextHandle, BrowserSidecarError> {
        self.host.create_wasm_context(request)
    }

    pub fn start_execution(
        &mut self,
        request: StartExecutionRequest,
    ) -> Result<StartedExecution, BrowserSidecarError> {
        self.host.start_execution(request)
    }

    pub fn write_stdin(
        &mut self,
        request: WriteExecutionStdinRequest,
    ) -> Result<(), BrowserSidecarError> {
        self.host.write_stdin(request)
    }

    pub fn close_stdin(
        &mut self,
        request: ExecutionHandleRequest,
    ) -> Result<(), BrowserSidecarError> {
        self.host.close_stdin(request)
    }

    pub fn kill_execution(
        &mut self,
        request: KillExecutionRequest,
    ) -> Result<(), BrowserSidecarError> {
        self.host.kill_execution(request)
    }

    pub fn poll_execution_event(
        &mut self,
        request: PollExecutionEventRequest,
    ) -> Result<Option<ExecutionEvent>, BrowserSidecarError> {
        self.host.poll_execution_event(request)
    }
}

pub struct BrowserSidecar<B> {
    bridge: B,
    config: BrowserSidecarConfig,
    vms: BTreeMap<String, VmState>,
    contexts: BTreeMap<String, ContextState>,
    executions: BTreeMap<String, ExecutionState>,
    extensions: BTreeMap<String, Box<dyn BrowserExtension>>,
}

impl<B> BrowserSidecar<B>
where
    B: BrowserSidecarBridge,
    BridgeError<B>: fmt::Debug,
{
    pub fn new(bridge: B, config: BrowserSidecarConfig) -> Self {
        Self::with_extensions(bridge, config, Vec::new())
            .expect("empty browser extension registry should be valid")
    }

    pub fn with_extensions(
        bridge: B,
        config: BrowserSidecarConfig,
        extensions: Vec<Box<dyn BrowserExtension>>,
    ) -> Result<Self, BrowserSidecarError> {
        let mut sidecar = Self {
            bridge,
            config,
            vms: BTreeMap::new(),
            contexts: BTreeMap::new(),
            executions: BTreeMap::new(),
            extensions: BTreeMap::new(),
        };
        for extension in extensions {
            sidecar.register_extension(extension)?;
        }
        Ok(sidecar)
    }

    pub fn register_extension(
        &mut self,
        extension: Box<dyn BrowserExtension>,
    ) -> Result<(), BrowserSidecarError> {
        let namespace = extension.namespace();
        if namespace.is_empty() {
            return Err(BrowserSidecarError::InvalidState(String::from(
                "browser extension namespace must not be empty",
            )));
        }
        if self.extensions.contains_key(namespace) {
            return Err(BrowserSidecarError::InvalidState(format!(
                "browser extension namespace already registered: {namespace}",
            )));
        }
        self.extensions.insert(namespace.to_string(), extension);
        Ok(())
    }

    pub fn extension_count(&self) -> usize {
        self.extensions.len()
    }

    pub fn has_extension(&self, namespace: &str) -> bool {
        self.extensions.contains_key(namespace)
    }

    pub fn dispatch_extension_request(
        &mut self,
        request: BrowserExtensionRequest,
    ) -> Result<BrowserExtensionResponse, BrowserSidecarError> {
        let Some(extension) = self.extensions.remove(&request.namespace) else {
            return Err(BrowserSidecarError::InvalidState(format!(
                "no browser extension registered for namespace {}",
                request.namespace
            )));
        };
        let payload = {
            let mut context = BrowserExtensionContext::with_ownership(
                self,
                request.vm_id.clone(),
                request.connection_id.clone(),
            );
            extension.handle_request(&mut context, &request.payload)
        };
        self.extensions.insert(request.namespace.clone(), extension);
        let payload = payload?;
        Ok(BrowserExtensionResponse {
            namespace: request.namespace,
            payload,
        })
    }

    pub fn sidecar_id(&self) -> &str {
        &self.config.sidecar_id
    }

    pub fn bridge(&self) -> &B {
        &self.bridge
    }

    pub fn bridge_mut(&mut self) -> &mut B {
        &mut self.bridge
    }

    pub fn into_bridge(self) -> B {
        self.bridge
    }

    pub fn vm_count(&self) -> usize {
        self.vms.len()
    }

    pub fn context_count(&self, vm_id: &str) -> usize {
        self.vms
            .get(vm_id)
            .map(|vm| vm.contexts.len())
            .unwrap_or_default()
    }

    pub fn active_worker_count(&self, vm_id: &str) -> usize {
        self.vms
            .get(vm_id)
            .map(|vm| vm.active_executions.len())
            .unwrap_or_default()
    }

    pub fn create_vm(&mut self, config: KernelVmConfig) -> Result<(), BrowserSidecarError> {
        self.create_vm_with_root_filesystem(config, RootFilesystemConfig::default())
    }

    pub fn configure_vm(
        &mut self,
        vm_id: &str,
        permissions: Option<agentos_kernel::permissions::Permissions>,
        instructions: Vec<String>,
        projected_modules: Vec<ProjectedModuleDescriptor>,
        command_permissions: BTreeMap<String, WasmPermissionTier>,
        loopback_exempt_ports: impl IntoIterator<Item = u16>,
    ) -> Result<(), BrowserSidecarError> {
        let vm = self
            .vms
            .get_mut(vm_id)
            .ok_or_else(|| BrowserSidecarError::InvalidState(format!("unknown VM {vm_id}")))?;
        if let Some(permissions) = permissions {
            vm.kernel.set_permissions(permissions);
        }
        let loopback_exempt_ports: Vec<u16> = loopback_exempt_ports.into_iter().collect();
        vm.configuration.instructions = instructions;
        vm.configuration.projected_modules = projected_modules;
        vm.configuration.command_permissions = command_permissions;
        vm.configuration.loopback_exempt_ports = loopback_exempt_ports.clone();
        let mut effective_loopback_exempt_ports =
            vm.configuration.create_loopback_exempt_ports.clone();
        effective_loopback_exempt_ports.extend(loopback_exempt_ports);
        vm.kernel
            .set_loopback_exempt_ports(effective_loopback_exempt_ports);
        Ok(())
    }

    pub fn create_vm_with_root_filesystem(
        &mut self,
        config: KernelVmConfig,
        root_filesystem: RootFilesystemConfig,
    ) -> Result<(), BrowserSidecarError> {
        let vm_id = config.vm_id.clone();
        if self.vms.contains_key(&vm_id) {
            return Err(BrowserSidecarError::InvalidState(format!(
                "browser sidecar VM already exists: {vm_id}"
            )));
        }

        self.emit_lifecycle(
            &vm_id,
            LifecycleState::Starting,
            Some(String::from(
                "browser sidecar booting kernel on main thread",
            )),
        )?;
        let create_loopback_exempt_ports = config.loopback_exempt_ports.clone();
        self.vms.insert(
            vm_id.clone(),
            VmState {
                kernel: KernelVm::new(
                    build_root_mount_table(&root_filesystem, &config.resources)
                        .map_err(Self::sidecar_core_error)?,
                    config,
                ),
                configuration: BrowserVmConfiguration {
                    create_loopback_exempt_ports,
                    ..BrowserVmConfiguration::default()
                },
                layers: VmLayerStore::default(),
                bindings: BTreeMap::new(),
                signal_states: BTreeMap::new(),
                contexts: BTreeSet::new(),
                active_executions: BTreeSet::new(),
            },
        );
        if let Some(root) = self
            .vms
            .get_mut(&vm_id)
            .and_then(|vm| vm.kernel.root_filesystem_mut())
        {
            root.finish_bootstrap();
        }
        self.emit_lifecycle(
            &vm_id,
            LifecycleState::Ready,
            Some(String::from(
                "browser sidecar kernel is ready on the main thread",
            )),
        )?;
        Ok(())
    }

    pub fn write_file(
        &mut self,
        vm_id: &str,
        path: &str,
        contents: impl Into<Vec<u8>>,
    ) -> Result<(), BrowserSidecarError> {
        let vm = self.vm_mut(vm_id)?;
        vm.kernel
            .write_file(path, contents)
            .map_err(Self::kernel_error)
    }

    pub fn read_file(&mut self, vm_id: &str, path: &str) -> Result<Vec<u8>, BrowserSidecarError> {
        let vm = self.vm_mut(vm_id)?;
        vm.kernel.read_file(path).map_err(Self::kernel_error)
    }

    pub fn mkdir(
        &mut self,
        vm_id: &str,
        path: &str,
        recursive: bool,
    ) -> Result<(), BrowserSidecarError> {
        let vm = self.vm_mut(vm_id)?;
        vm.kernel.mkdir(path, recursive).map_err(Self::kernel_error)
    }

    pub fn read_dir(
        &mut self,
        vm_id: &str,
        path: &str,
    ) -> Result<Vec<String>, BrowserSidecarError> {
        let vm = self.vm_mut(vm_id)?;
        vm.kernel.read_dir(path).map_err(Self::kernel_error)
    }

    pub fn snapshot_root_filesystem(
        &mut self,
        vm_id: &str,
    ) -> Result<RootFilesystemSnapshot, BrowserSidecarError> {
        let vm = self.vm_mut(vm_id)?;
        vm.kernel
            .snapshot_root_filesystem()
            .map_err(Self::kernel_error)
    }

    pub fn create_layer(&mut self, vm_id: &str) -> Result<String, BrowserSidecarError> {
        self.vm_mut(vm_id)?
            .layers
            .create_writable_layer()
            .map_err(Self::sidecar_core_error)
    }

    pub fn seal_layer(
        &mut self,
        vm_id: &str,
        layer_id: &str,
    ) -> Result<String, BrowserSidecarError> {
        self.vm_mut(vm_id)?
            .layers
            .seal_layer(layer_id)
            .map_err(Self::sidecar_core_error)
    }

    pub fn import_snapshot(
        &mut self,
        vm_id: &str,
        entries: &[RootFilesystemEntry],
    ) -> Result<String, BrowserSidecarError> {
        let snapshot = root_snapshot_from_entries(entries)?;
        self.vm_mut(vm_id)?
            .layers
            .import_snapshot(snapshot)
            .map_err(Self::sidecar_core_error)
    }

    pub fn export_snapshot(
        &mut self,
        vm_id: &str,
        layer_id: &str,
    ) -> Result<RootFilesystemSnapshot, BrowserSidecarError> {
        self.vm_mut(vm_id)?
            .layers
            .export_snapshot(layer_id)
            .map_err(Self::sidecar_core_error)
    }

    pub fn create_overlay(
        &mut self,
        vm_id: &str,
        mode: KernelRootFilesystemMode,
        upper_layer_id: Option<String>,
        lower_layer_ids: Vec<String>,
    ) -> Result<String, BrowserSidecarError> {
        self.vm_mut(vm_id)?
            .layers
            .create_overlay_layer(mode, upper_layer_id, lower_layer_ids)
            .map_err(Self::sidecar_core_error)
    }

    pub fn register_host_callbacks(
        &mut self,
        vm_id: &str,
        payload: RegisterHostCallbacksRequest,
    ) -> Result<(String, u32), BrowserSidecarError> {
        validate_bindings_registration(&payload)
            .map_err(|error| BrowserSidecarError::InvalidState(error.to_string()))?;
        let vm = self.vm_mut(vm_id)?;
        ensure_collection_name_available(&vm.bindings, &payload.name)
            .map_err(|error| BrowserSidecarError::InvalidState(error.to_string()))?;
        ensure_command_aliases_available(&vm.bindings, &payload)
            .map_err(|error| BrowserSidecarError::InvalidState(error.to_string()))?;
        ensure_binding_registry_capacity(&vm.bindings, &payload)
            .map_err(|error| BrowserSidecarError::InvalidState(error.to_string()))?;

        let registration = payload.name.clone();
        vm.bindings.insert(registration.clone(), payload);
        let command_count = u32::try_from(registered_binding_command_names(&vm.bindings).len())
            .map_err(|_| {
                BrowserSidecarError::InvalidState(String::from(
                    "registered host callback command count exceeds u32",
                ))
            })?;
        Ok((registration, command_count))
    }

    pub fn bootstrap_root_filesystem_entries(
        &mut self,
        vm_id: &str,
        entries: &[RootFilesystemEntry],
    ) -> Result<u32, BrowserSidecarError> {
        for entry in entries {
            self.apply_root_filesystem_entry(vm_id, entry)?;
        }
        u32::try_from(entries.len()).map_err(|_| {
            BrowserSidecarError::InvalidState(String::from(
                "root filesystem bootstrap entry count exceeds u32",
            ))
        })
    }

    pub fn guest_filesystem_call(
        &mut self,
        vm_id: &str,
        payload: GuestFilesystemCallRequest,
    ) -> Result<GuestFilesystemResultResponse, BrowserSidecarError> {
        let vm = self.vm_mut(vm_id)?;
        handle_guest_filesystem_call(&mut vm.kernel, payload).map_err(Self::sidecar_core_error)
    }

    pub fn guest_kernel_call(
        &mut self,
        vm_id: &str,
        payload: GuestKernelCallRequest,
    ) -> Result<GuestKernelResultResponse, BrowserSidecarError> {
        let execution = self.ensure_execution_state(vm_id, &payload.execution_id)?;
        let kernel_pid = execution.kernel_pid;
        let vm = self.vm_mut(vm_id)?;
        let response = handle_guest_kernel_call(
            &mut vm.kernel,
            kernel_pid,
            BROWSER_WORKER_DRIVER,
            &payload.operation,
            &payload.payload,
        )
        .map_err(Self::sidecar_core_error)?;
        Ok(GuestKernelResultResponse { payload: response })
    }

    pub fn kernel_state(&self, vm_id: &str) -> Result<LifecycleState, BrowserSidecarError> {
        let vm = self.vm(vm_id)?;
        Ok(match vm.kernel.state() {
            KernelLifecycleState::Starting => LifecycleState::Starting,
            KernelLifecycleState::Ready => LifecycleState::Ready,
            KernelLifecycleState::Busy => LifecycleState::Busy,
            KernelLifecycleState::Terminated => LifecycleState::Terminated,
        })
    }

    pub fn zombie_timer_count(&self, vm_id: &str) -> Result<u64, BrowserSidecarError> {
        let vm = self.vm(vm_id)?;
        Ok(vm.kernel.zombie_timer_count() as u64)
    }

    pub fn process_snapshot_entries(
        &self,
        vm_id: &str,
    ) -> Result<Vec<SharedProcessSnapshotEntry>, BrowserSidecarError> {
        let vm = self.vm(vm_id)?;
        let process_table = vm.kernel.list_processes();
        Ok(self
            .executions
            .iter()
            .filter(|(_, execution)| execution.vm_id == vm_id)
            .filter_map(|(execution_id, execution)| {
                process_table.get(&execution.kernel_pid).map(|info| {
                    process_snapshot_entry_from_kernel(
                        execution_id,
                        info,
                        execution.cwd.clone(),
                        None,
                    )
                })
            })
            .collect())
    }

    pub fn signal_state(
        &self,
        vm_id: &str,
        execution_id: &str,
    ) -> Result<BTreeMap<u32, ProtocolSignalHandlerRegistration>, BrowserSidecarError> {
        let vm = self.vm(vm_id)?;
        Ok(vm
            .signal_states
            .get(execution_id)
            .cloned()
            .unwrap_or_default())
    }

    pub fn find_listener(
        &self,
        vm_id: &str,
        request: &FindListenerRequest,
    ) -> Result<Option<SocketStateEntry>, BrowserSidecarError> {
        let vm = self.vm(vm_id)?;
        for (execution_id, execution) in &self.executions {
            if execution.vm_id != vm_id {
                continue;
            }
            for record in vm.kernel.socket_records_for_pid(execution.kernel_pid) {
                if let Some(path) = request.path.as_deref() {
                    if record.state() == SocketState::Listening
                        && record.spec().socket_type == SocketType::Stream
                        && record.local_unix_path() == Some(path)
                    {
                        return Ok(Some(SocketStateEntry {
                            process_id: execution_id.clone(),
                            host: None,
                            port: None,
                            path: Some(path.to_string()),
                        }));
                    }
                    continue;
                }

                let Some(address) = record.local_address() else {
                    continue;
                };
                if record.state() != SocketState::Listening
                    || record.spec().socket_type != SocketType::Stream
                    || !socket_host_matches(request.host.as_deref(), address.host())
                    || request.port.is_some_and(|port| address.port() != port)
                {
                    continue;
                }
                return Ok(Some(SocketStateEntry {
                    process_id: execution_id.clone(),
                    host: Some(address.host().to_string()),
                    port: Some(address.port()),
                    path: None,
                }));
            }
        }
        Ok(None)
    }

    pub fn find_bound_udp(
        &self,
        vm_id: &str,
        request: &FindBoundUdpRequest,
    ) -> Result<Option<SocketStateEntry>, BrowserSidecarError> {
        let vm = self.vm(vm_id)?;
        for (execution_id, execution) in &self.executions {
            if execution.vm_id != vm_id {
                continue;
            }
            for record in vm.kernel.socket_records_for_pid(execution.kernel_pid) {
                let Some(address) = record.local_address() else {
                    continue;
                };
                if record.state() != SocketState::Bound
                    || record.spec().socket_type != SocketType::Datagram
                    || !socket_host_matches(request.host.as_deref(), address.host())
                    || request.port.is_some_and(|port| address.port() != port)
                {
                    continue;
                }
                return Ok(Some(SocketStateEntry {
                    process_id: execution_id.clone(),
                    host: Some(address.host().to_string()),
                    port: Some(address.port()),
                    path: None,
                }));
            }
        }
        Ok(None)
    }

    pub fn vm_fetch(
        &mut self,
        vm_id: &str,
        request: &agentos_sidecar_protocol::protocol::VmFetchRequest,
    ) -> Result<String, BrowserSidecarError> {
        let target_path = if request.path.starts_with('/') {
            request.path.clone()
        } else {
            format!("/{}", request.path)
        };
        let listener = self
            .find_listener(
                vm_id,
                &FindListenerRequest {
                    host: Some(String::from("127.0.0.1")),
                    port: Some(request.port),
                    path: None,
                },
            )?
            .ok_or_else(|| {
                BrowserSidecarError::InvalidState(format!(
                    "vm.fetch could not find a guest HTTP listener on port {}",
                    request.port
                ))
            })?;
        let target_execution_id = listener.process_id;
        let target = self.ensure_execution_state(vm_id, &target_execution_id)?;
        let request_bytes = serialize_kernel_http_fetch_request(
            request.port,
            &target_path,
            &request.method,
            &request.headers_json,
            request.body.as_deref(),
        )
        .map_err(|error| BrowserSidecarError::InvalidState(error.to_string()))?;
        let url = format!("http://127.0.0.1:{}{}", request.port, target_path);

        let socket_id = {
            let vm = self.vm_mut(vm_id)?;
            let socket_id = vm
                .kernel
                .socket_create(BROWSER_WORKER_DRIVER, target.kernel_pid, SocketSpec::tcp())
                .map_err(Self::kernel_error)?;
            let result = vm
                .kernel
                .socket_connect_inet_loopback(
                    BROWSER_WORKER_DRIVER,
                    target.kernel_pid,
                    socket_id,
                    InetSocketAddress::new("127.0.0.1", request.port),
                )
                .map_err(Self::kernel_error)
                .and_then(|()| {
                    vm.kernel
                        .socket_write(
                            BROWSER_WORKER_DRIVER,
                            target.kernel_pid,
                            socket_id,
                            &request_bytes,
                        )
                        .map(|_| ())
                        .map_err(Self::kernel_error)
                });
            if let Err(error) = result {
                let _ = vm
                    .kernel
                    .socket_close(BROWSER_WORKER_DRIVER, target.kernel_pid, socket_id);
                return Err(error);
            }
            socket_id
        };

        let result = self.read_vm_fetch_response(
            vm_id,
            target.kernel_pid,
            socket_id,
            &url,
            VM_FETCH_BUFFER_LIMIT_BYTES,
        );
        let close_result = {
            let vm = self.vm_mut(vm_id)?;
            vm.kernel
                .socket_close(BROWSER_WORKER_DRIVER, target.kernel_pid, socket_id)
                .map_err(Self::kernel_error)
        };

        match (result, close_result) {
            (Ok(response), Ok(())) => Ok(response),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn read_vm_fetch_response(
        &mut self,
        vm_id: &str,
        kernel_pid: u32,
        socket_id: SocketId,
        url: &str,
        max_fetch_response_bytes: usize,
    ) -> Result<String, BrowserSidecarError> {
        let mut response_buffer = Vec::new();
        let mut peer_closed = false;
        let deadline = Instant::now() + browser_vm_fetch_timeout();

        loop {
            if let Some(response) =
                parse_kernel_http_fetch_response(&response_buffer, peer_closed, url)
                    .map_err(|error| BrowserSidecarError::InvalidState(error.to_string()))?
            {
                ensure_vm_fetch_response_within_limit(
                    &response,
                    "vm.fetch",
                    max_fetch_response_bytes,
                )
                .map_err(|error| BrowserSidecarError::InvalidState(error.to_string()))?;
                return Ok(response);
            }
            if Instant::now() >= deadline {
                let preview = String::from_utf8_lossy(&response_buffer);
                return Err(BrowserSidecarError::InvalidState(format!(
                    "vm.fetch timed out waiting for kernel TCP HTTP response ({} buffered bytes: {:?})",
                    response_buffer.len(),
                    preview.chars().take(200).collect::<String>()
                )));
            }

            let poll = {
                let vm = self.vm_mut(vm_id)?;
                vm.kernel
                    .poll_targets(
                        BROWSER_WORKER_DRIVER,
                        kernel_pid,
                        vec![PollTargetEntry::socket(
                            socket_id,
                            POLLIN | POLLHUP | POLLERR,
                        )],
                        5,
                    )
                    .map_err(Self::kernel_error)?
            };
            let revents = poll
                .targets
                .first()
                .map(|entry| entry.revents)
                .unwrap_or_default();
            if revents.intersects(POLLERR) {
                return Err(BrowserSidecarError::InvalidState(String::from(
                    "vm.fetch kernel TCP socket reported POLLERR",
                )));
            }
            if revents.intersects(POLLIN) {
                let read_result = {
                    let vm = self.vm_mut(vm_id)?;
                    vm.kernel
                        .socket_read(BROWSER_WORKER_DRIVER, kernel_pid, socket_id, 64 * 1024)
                };
                match read_result {
                    Ok(Some(bytes)) if !bytes.is_empty() => {
                        response_buffer.extend(bytes);
                        ensure_vm_fetch_raw_response_buffer_within_limit(
                            response_buffer.len(),
                            "vm.fetch",
                        )
                        .map_err(|error| BrowserSidecarError::InvalidState(error.to_string()))?;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => peer_closed = true,
                    Err(error) if error.code() == "EAGAIN" => {}
                    Err(error) => return Err(Self::kernel_error(error)),
                }
            }
            if revents.intersects(POLLHUP) {
                peer_closed = true;
            }
        }
    }

    pub fn create_kernel_tcp_listener_for_execution(
        &mut self,
        vm_id: &str,
        execution_id: &str,
        host: &str,
        port: u16,
        backlog: usize,
    ) -> Result<SocketId, BrowserSidecarError> {
        let execution = self.ensure_execution_state(vm_id, execution_id)?;
        let vm = self.vm_mut(vm_id)?;
        let socket_id = vm
            .kernel
            .socket_create(
                BROWSER_WORKER_DRIVER,
                execution.kernel_pid,
                SocketSpec::tcp(),
            )
            .map_err(Self::kernel_error)?;
        vm.kernel
            .socket_bind_inet(
                BROWSER_WORKER_DRIVER,
                execution.kernel_pid,
                socket_id,
                InetSocketAddress::new(host, port),
            )
            .map_err(Self::kernel_error)?;
        vm.kernel
            .socket_listen(
                BROWSER_WORKER_DRIVER,
                execution.kernel_pid,
                socket_id,
                backlog,
            )
            .map_err(Self::kernel_error)?;
        Ok(socket_id)
    }

    pub fn create_kernel_bound_udp_for_execution(
        &mut self,
        vm_id: &str,
        execution_id: &str,
        host: &str,
        port: u16,
    ) -> Result<SocketId, BrowserSidecarError> {
        let execution = self.ensure_execution_state(vm_id, execution_id)?;
        let vm = self.vm_mut(vm_id)?;
        let socket_id = vm
            .kernel
            .socket_create(
                BROWSER_WORKER_DRIVER,
                execution.kernel_pid,
                SocketSpec::udp(),
            )
            .map_err(Self::kernel_error)?;
        vm.kernel
            .socket_bind_inet(
                BROWSER_WORKER_DRIVER,
                execution.kernel_pid,
                socket_id,
                InetSocketAddress::new(host, port),
            )
            .map_err(Self::kernel_error)?;
        Ok(socket_id)
    }

    pub fn read_execution_stdin(
        &mut self,
        vm_id: &str,
        execution_id: &str,
        length: usize,
        timeout: Duration,
    ) -> Result<Option<Vec<u8>>, BrowserSidecarError> {
        let execution = self.ensure_execution_state(vm_id, execution_id)?;
        let vm = self.vm_mut(vm_id)?;
        vm.kernel
            .read_process_stdin(
                BROWSER_WORKER_DRIVER,
                execution.kernel_pid,
                length,
                Some(timeout),
            )
            .map_err(Self::kernel_error)
    }

    pub fn dispose_vm(&mut self, vm_id: &str) -> Result<(), BrowserSidecarError> {
        // Remove the VM bookkeeping up front and take ownership of its state, so
        // that EVERY exit path below — including a mid-dispose `?` failure while
        // releasing executions or emitting lifecycle events — reclaims the
        // VmState (and the BrowserKernel it owns) instead of stranding it in the
        // `vms` map for the process lifetime.
        let Some(vm_state) = self.vms.remove(vm_id) else {
            return Err(BrowserSidecarError::InvalidState(format!(
                "unknown browser sidecar VM: {vm_id}"
            )));
        };

        // Dropping per-context bookkeeping is infallible, so do it
        // unconditionally; `contexts` can never retain an entry for a VM that
        // has already been removed from `vms`.
        for context_id in &vm_state.contexts {
            self.contexts.remove(context_id);
        }

        // Release every execution, attempting all of them and retaining only the
        // first error. A single worker-termination failure must not abandon the
        // remaining executions (their `ExecutionState`s would otherwise leak),
        // and `release_execution` already removes each entry from `executions`
        // before doing fallible bridge work, so the maps stay drained even when
        // the bridge reports an error.
        let mut first_error: Option<BrowserSidecarError> = None;
        for execution_id in &vm_state.active_executions {
            if let Err(error) = self.release_execution(execution_id, "browser.worker.disposed") {
                first_error.get_or_insert(error);
            }
        }

        // Emit the terminal lifecycle event regardless of the outcome above; the
        // VM is already gone from the registry either way.
        let terminated = self.emit_lifecycle(
            vm_id,
            LifecycleState::Terminated,
            Some(String::from(
                "browser sidecar VM disposed on the main thread",
            )),
        );

        match first_error {
            Some(error) => Err(error),
            None => terminated,
        }
    }

    pub fn create_javascript_context(
        &mut self,
        request: CreateJavascriptContextRequest,
    ) -> Result<GuestContextHandle, BrowserSidecarError> {
        self.ensure_vm(&request.vm_id)?;

        let vm_id = request.vm_id.clone();
        let entrypoint = BrowserWorkerEntrypoint::JavaScript {
            bootstrap_module: request.bootstrap_module.clone(),
        };
        let handle = self
            .bridge
            .create_javascript_context(request)
            .map_err(Self::bridge_error)?;

        self.register_context(vm_id, handle.clone(), entrypoint)?;
        Ok(handle)
    }

    pub fn create_wasm_context(
        &mut self,
        request: CreateWasmContextRequest,
    ) -> Result<GuestContextHandle, BrowserSidecarError> {
        self.ensure_vm(&request.vm_id)?;

        let vm_id = request.vm_id.clone();
        let entrypoint = BrowserWorkerEntrypoint::WebAssembly {
            module_path: request.module_path.clone(),
        };
        let handle = self
            .bridge
            .create_wasm_context(request)
            .map_err(Self::bridge_error)?;

        self.register_context(vm_id, handle.clone(), entrypoint)?;
        Ok(handle)
    }

    pub fn start_execution(
        &mut self,
        request: StartExecutionRequest,
    ) -> Result<StartedExecution, BrowserSidecarError> {
        self.start_execution_with_options(request, BrowserExecutionOptions::default())
    }

    pub fn start_execution_with_options(
        &mut self,
        request: StartExecutionRequest,
        options: BrowserExecutionOptions,
    ) -> Result<StartedExecution, BrowserSidecarError> {
        self.ensure_vm(&request.vm_id)?;

        let context = self
            .contexts
            .get(&request.context_id)
            .cloned()
            .ok_or_else(|| {
                BrowserSidecarError::InvalidState(format!(
                    "unknown browser sidecar context: {}",
                    request.context_id
                ))
            })?;

        if context.vm_id != request.vm_id {
            return Err(BrowserSidecarError::InvalidState(format!(
                "browser sidecar context {} belongs to vm {}, not {}",
                request.context_id, context.vm_id, request.vm_id
            )));
        }

        let guest_cwd = request.cwd.clone();
        let (kernel_pid, stdin_write_fd) = {
            let vm = self.vm_mut(&request.vm_id)?;
            let kernel_handle = vm
                .kernel
                .create_virtual_process(
                    BROWSER_WORKER_DRIVER,
                    BROWSER_WORKER_DRIVER,
                    request
                        .argv
                        .first()
                        .map(String::as_str)
                        .unwrap_or("browser-worker"),
                    request.argv.clone(),
                    VirtualProcessOptions {
                        env: request.env.clone(),
                        cwd: Some(guest_cwd.clone()),
                        ..VirtualProcessOptions::default()
                    },
                )
                .map_err(Self::kernel_error)?;
            let kernel_pid = kernel_handle.pid();
            match Self::configure_process_stdio(&mut vm.kernel, kernel_pid) {
                Ok(stdin_write_fd) => (kernel_pid, stdin_write_fd),
                Err(error) => {
                    Self::cleanup_pending_kernel_process(&mut vm.kernel, kernel_pid)?;
                    return Err(error);
                }
            }
        };

        let (process_config, os_config) = {
            let vm = self.vm(&request.vm_id)?;
            browser_worker_identity(&vm.kernel, &request, kernel_pid)
        };
        let wasm_permission_tier = match context.runtime {
            GuestRuntime::JavaScript => None,
            GuestRuntime::WebAssembly => Some(self.resolve_wasm_permission_tier(
                &request.vm_id,
                options.command_name.as_deref(),
                options.wasm_permission_tier,
                &context.entrypoint,
            )?),
        };

        let started = match self.bridge.start_execution(request.clone()) {
            Ok(started) => started,
            Err(error) => {
                let vm = self.vm_mut(&request.vm_id)?;
                Self::cleanup_pending_kernel_process(&mut vm.kernel, kernel_pid)?;
                return Err(Self::bridge_error(error));
            }
        };

        let worker = match self.bridge.create_worker(BrowserWorkerSpawnRequest {
            vm_id: request.vm_id.clone(),
            context_id: request.context_id.clone(),
            execution_id: started.execution_id.clone(),
            runtime: context.runtime,
            entrypoint: context.entrypoint.clone(),
            wasm_permission_tier,
            process_config,
            os_config,
        }) {
            Ok(worker) => worker,
            Err(error) => {
                let vm = self.vm_mut(&request.vm_id)?;
                Self::cleanup_pending_kernel_process(&mut vm.kernel, kernel_pid)?;
                self.bridge
                    .kill_execution(KillExecutionRequest {
                        vm_id: request.vm_id,
                        execution_id: started.execution_id,
                        signal: agentos_bridge::ExecutionSignal::Kill,
                    })
                    .map_err(Self::bridge_error)?;
                return Err(Self::bridge_error(error));
            }
        };

        let worker_id = worker.worker_id.clone();
        self.executions.insert(
            started.execution_id.clone(),
            ExecutionState {
                vm_id: request.vm_id.clone(),
                worker: worker.clone(),
                kernel_pid,
                stdin_write_fd,
                cwd: guest_cwd,
            },
        );
        let vm_state = self
            .vms
            .get_mut(&request.vm_id)
            .expect("VM should exist after validation");
        vm_state
            .active_executions
            .insert(started.execution_id.clone());

        self.emit_structured(
            &request.vm_id,
            "browser.worker.spawned",
            BTreeMap::from([
                (String::from("context_id"), request.context_id),
                (String::from("execution_id"), started.execution_id.clone()),
                (
                    String::from("runtime"),
                    runtime_label(context.runtime).to_string(),
                ),
                (String::from("worker_id"), worker_id),
            ]),
        )?;
        self.emit_lifecycle(
            &request.vm_id,
            LifecycleState::Busy,
            Some(String::from(
                "browser sidecar is coordinating guest execution on the main thread",
            )),
        )?;

        Ok(started)
    }

    fn resolve_wasm_permission_tier(
        &self,
        vm_id: &str,
        command_name: Option<&str>,
        explicit_tier: Option<WasmPermissionTier>,
        entrypoint: &BrowserWorkerEntrypoint,
    ) -> Result<WasmPermissionTier, BrowserSidecarError> {
        let vm = self.vm(vm_id)?;
        Ok(explicit_tier
            .or_else(|| {
                command_name
                    .and_then(|command| vm.configuration.command_permissions.get(command).copied())
            })
            .or_else(|| {
                let BrowserWorkerEntrypoint::WebAssembly {
                    module_path: Some(module_path),
                } = entrypoint
                else {
                    return None;
                };
                module_path
                    .rsplit('/')
                    .next()
                    .and_then(|name| vm.configuration.command_permissions.get(name).copied())
            })
            .unwrap_or(WasmPermissionTier::Full))
    }

    fn configure_process_stdio(
        kernel: &mut BrowserKernel,
        kernel_pid: u32,
    ) -> Result<u32, BrowserSidecarError> {
        let (stdin_read_fd, stdin_write_fd) = kernel
            .open_pipe(BROWSER_WORKER_DRIVER, kernel_pid)
            .map_err(Self::kernel_error)?;
        kernel
            .fd_dup2(BROWSER_WORKER_DRIVER, kernel_pid, stdin_read_fd, 0)
            .map_err(Self::kernel_error)?;
        let (_stdout_read_fd, stdout_write_fd) = kernel
            .open_pipe(BROWSER_WORKER_DRIVER, kernel_pid)
            .map_err(Self::kernel_error)?;
        kernel
            .fd_dup2(BROWSER_WORKER_DRIVER, kernel_pid, stdout_write_fd, 1)
            .map_err(Self::kernel_error)?;
        let (_stderr_read_fd, stderr_write_fd) = kernel
            .open_pipe(BROWSER_WORKER_DRIVER, kernel_pid)
            .map_err(Self::kernel_error)?;
        kernel
            .fd_dup2(BROWSER_WORKER_DRIVER, kernel_pid, stderr_write_fd, 2)
            .map_err(Self::kernel_error)?;
        Ok(stdin_write_fd)
    }

    fn cleanup_pending_kernel_process(
        kernel: &mut BrowserKernel,
        kernel_pid: u32,
    ) -> Result<(), BrowserSidecarError> {
        kernel
            .exit_process(BROWSER_WORKER_DRIVER, kernel_pid, 1)
            .map_err(Self::kernel_error)?;
        kernel.waitpid(kernel_pid).map_err(Self::kernel_error)?;
        Ok(())
    }

    fn reap_execution_kernel_process(
        &mut self,
        vm_id: &str,
        kernel_pid: u32,
    ) -> Result<(), BrowserSidecarError> {
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let Some(process) = vm.kernel.list_processes().get(&kernel_pid).cloned() else {
            return Ok(());
        };

        if process.status != ProcessStatus::Exited {
            vm.kernel
                .exit_process(BROWSER_WORKER_DRIVER, kernel_pid, 1)
                .map_err(Self::kernel_error)?;
        }
        vm.kernel.waitpid(kernel_pid).map_err(Self::kernel_error)?;
        Ok(())
    }

    pub fn write_stdin(
        &mut self,
        request: WriteExecutionStdinRequest,
    ) -> Result<(), BrowserSidecarError> {
        self.ensure_execution(&request.vm_id, &request.execution_id)?;
        let execution = self.ensure_execution_state(&request.vm_id, &request.execution_id)?;
        {
            let vm = self.vm_mut(&request.vm_id)?;
            vm.kernel
                .fd_write(
                    BROWSER_WORKER_DRIVER,
                    execution.kernel_pid,
                    execution.stdin_write_fd,
                    &request.chunk,
                )
                .map_err(Self::kernel_error)?;
        }
        self.bridge.write_stdin(request).map_err(Self::bridge_error)
    }

    pub fn close_stdin(
        &mut self,
        request: ExecutionHandleRequest,
    ) -> Result<(), BrowserSidecarError> {
        self.ensure_execution(&request.vm_id, &request.execution_id)?;
        let execution = self.ensure_execution_state(&request.vm_id, &request.execution_id)?;
        {
            let vm = self.vm_mut(&request.vm_id)?;
            vm.kernel
                .fd_close(
                    BROWSER_WORKER_DRIVER,
                    execution.kernel_pid,
                    execution.stdin_write_fd,
                )
                .map_err(Self::kernel_error)?;
        }
        self.bridge.close_stdin(request).map_err(Self::bridge_error)
    }

    pub fn kill_execution(
        &mut self,
        request: KillExecutionRequest,
    ) -> Result<(), BrowserSidecarError> {
        self.ensure_execution(&request.vm_id, &request.execution_id)?;
        self.signal_execution_kernel_process(
            &request.vm_id,
            &request.execution_id,
            agentos_native_sidecar_core::execution_signal_to_kernel(request.signal),
        )?;
        self.bridge
            .kill_execution(request)
            .map_err(Self::bridge_error)
    }

    pub fn signal_execution_kernel_process(
        &mut self,
        vm_id: &str,
        execution_id: &str,
        signal: i32,
    ) -> Result<(), BrowserSidecarError> {
        self.ensure_execution(vm_id, execution_id)?;
        let execution = self.ensure_execution_state(vm_id, execution_id)?;
        {
            let vm = self.vm_mut(vm_id)?;
            vm.kernel
                .kill_process(BROWSER_WORKER_DRIVER, execution.kernel_pid, signal)
                .map_err(Self::kernel_error)?;
        }
        Ok(())
    }

    pub fn poll_execution_event(
        &mut self,
        request: PollExecutionEventRequest,
    ) -> Result<Option<ExecutionEvent>, BrowserSidecarError> {
        self.ensure_vm(&request.vm_id)?;

        let event = self
            .bridge
            .poll_execution_event(request)
            .map_err(Self::bridge_error)?;

        match &event {
            Some(ExecutionEvent::Stdout(chunk)) => {
                let execution = self.ensure_execution_state(&chunk.vm_id, &chunk.execution_id)?;
                let vm = self.vm_mut(&chunk.vm_id)?;
                vm.kernel
                    .write_process_stdout(BROWSER_WORKER_DRIVER, execution.kernel_pid, &chunk.chunk)
                    .map_err(Self::kernel_error)?;
            }
            Some(ExecutionEvent::Stderr(chunk)) => {
                let execution = self.ensure_execution_state(&chunk.vm_id, &chunk.execution_id)?;
                let vm = self.vm_mut(&chunk.vm_id)?;
                vm.kernel
                    .write_process_stderr(BROWSER_WORKER_DRIVER, execution.kernel_pid, &chunk.chunk)
                    .map_err(Self::kernel_error)?;
            }
            Some(ExecutionEvent::Exited(exited)) => {
                let execution = self.ensure_execution_state(&exited.vm_id, &exited.execution_id)?;
                {
                    let vm = self.vm_mut(&exited.vm_id)?;
                    vm.kernel
                        .exit_process(
                            BROWSER_WORKER_DRIVER,
                            execution.kernel_pid,
                            exited.exit_code,
                        )
                        .map_err(Self::kernel_error)?;
                }
                self.release_execution(&exited.execution_id, "browser.worker.reaped")?;
            }
            Some(ExecutionEvent::GuestRequest(call)) => {
                let fields = unsupported_guest_kernel_call_detail(
                    None,
                    &call.execution_id,
                    &call.operation,
                    call.payload.len(),
                )
                .into_iter()
                .collect();
                self.emit_structured(
                    &call.vm_id,
                    agentos_native_sidecar_core::UNSUPPORTED_GUEST_KERNEL_CALL_EVENT,
                    fields,
                )?;
            }
            Some(ExecutionEvent::SignalState(state)) => {
                self.ensure_execution_state(&state.vm_id, &state.execution_id)?;
                let registration = protocol_signal_registration(&state.registration);
                let vm = self.vm_mut(&state.vm_id)?;
                apply_process_signal_state_update(
                    &mut vm.signal_states,
                    &state.execution_id,
                    state.signal,
                    registration,
                );
            }
            None => {}
        }

        Ok(event)
    }

    fn register_context(
        &mut self,
        vm_id: String,
        handle: GuestContextHandle,
        entrypoint: BrowserWorkerEntrypoint,
    ) -> Result<(), BrowserSidecarError> {
        self.contexts.insert(
            handle.context_id.clone(),
            ContextState {
                vm_id: vm_id.clone(),
                runtime: handle.runtime,
                entrypoint,
            },
        );
        let vm_state = self
            .vms
            .get_mut(&vm_id)
            .expect("VM should exist while registering a guest context");
        vm_state.contexts.insert(handle.context_id.clone());

        self.emit_structured(
            &vm_id,
            "browser.context.created",
            BTreeMap::from([
                (String::from("context_id"), handle.context_id),
                (
                    String::from("runtime"),
                    runtime_label(handle.runtime).to_string(),
                ),
            ]),
        )
    }

    fn release_execution(
        &mut self,
        execution_id: &str,
        event_name: &'static str,
    ) -> Result<(), BrowserSidecarError> {
        let Some(execution) = self.executions.remove(execution_id) else {
            return Ok(());
        };

        if let Some(vm_state) = self.vms.get_mut(&execution.vm_id) {
            vm_state.active_executions.remove(execution_id);
            vm_state.signal_states.remove(execution_id);
        }

        let vm_id = execution.vm_id;
        self.reap_execution_kernel_process(&vm_id, execution.kernel_pid)?;
        let runtime = execution.worker.runtime;
        let worker_id = execution.worker.worker_id;
        self.bridge
            .terminate_worker(BrowserWorkerHandleRequest {
                vm_id: vm_id.clone(),
                execution_id: execution_id.to_string(),
                worker_id: worker_id.clone(),
            })
            .map_err(Self::bridge_error)?;

        self.emit_structured(
            &vm_id,
            event_name,
            BTreeMap::from([
                (String::from("execution_id"), execution_id.to_string()),
                (String::from("runtime"), runtime_label(runtime).to_string()),
                (String::from("worker_id"), worker_id),
            ]),
        )?;

        let next_state = if self.active_worker_count(&vm_id) == 0 {
            LifecycleState::Ready
        } else {
            LifecycleState::Busy
        };
        self.emit_lifecycle(
            &vm_id,
            next_state,
            Some(String::from(
                "browser sidecar worker bookkeeping was updated on the main thread",
            )),
        )
    }

    fn ensure_vm(&self, vm_id: &str) -> Result<(), BrowserSidecarError> {
        if self.vms.contains_key(vm_id) {
            Ok(())
        } else {
            Err(BrowserSidecarError::InvalidState(format!(
                "unknown browser sidecar VM: {vm_id}"
            )))
        }
    }

    fn ensure_execution(&self, vm_id: &str, execution_id: &str) -> Result<(), BrowserSidecarError> {
        let execution = self.executions.get(execution_id).ok_or_else(|| {
            BrowserSidecarError::InvalidState(format!(
                "unknown browser sidecar execution: {execution_id}"
            ))
        })?;

        if execution.vm_id == vm_id {
            Ok(())
        } else {
            Err(BrowserSidecarError::InvalidState(format!(
                "browser sidecar execution {execution_id} belongs to vm {}, not {vm_id}",
                execution.vm_id
            )))
        }
    }

    fn ensure_execution_state(
        &self,
        vm_id: &str,
        execution_id: &str,
    ) -> Result<ExecutionState, BrowserSidecarError> {
        let execution = self.executions.get(execution_id).cloned().ok_or_else(|| {
            BrowserSidecarError::InvalidState(format!(
                "unknown browser sidecar execution: {execution_id}"
            ))
        })?;

        if execution.vm_id == vm_id {
            Ok(execution)
        } else {
            Err(BrowserSidecarError::InvalidState(format!(
                "browser sidecar execution {execution_id} belongs to vm {}, not {vm_id}",
                execution.vm_id
            )))
        }
    }

    fn vm(&self, vm_id: &str) -> Result<&VmState, BrowserSidecarError> {
        self.vms.get(vm_id).ok_or_else(|| {
            BrowserSidecarError::InvalidState(format!("unknown browser sidecar VM: {vm_id}"))
        })
    }

    fn vm_mut(&mut self, vm_id: &str) -> Result<&mut VmState, BrowserSidecarError> {
        self.vms.get_mut(vm_id).ok_or_else(|| {
            BrowserSidecarError::InvalidState(format!("unknown browser sidecar VM: {vm_id}"))
        })
    }

    fn emit_lifecycle(
        &mut self,
        vm_id: &str,
        state: LifecycleState,
        detail: Option<String>,
    ) -> Result<(), BrowserSidecarError> {
        self.bridge
            .emit_lifecycle(LifecycleEventRecord {
                vm_id: vm_id.to_string(),
                state,
                detail,
            })
            .map_err(Self::bridge_error)
    }

    fn emit_structured(
        &mut self,
        vm_id: &str,
        name: &str,
        fields: BTreeMap<String, String>,
    ) -> Result<(), BrowserSidecarError> {
        self.bridge
            .emit_structured_event(StructuredEventRecord {
                vm_id: vm_id.to_string(),
                name: name.to_string(),
                fields,
            })
            .map_err(Self::bridge_error)
    }

    fn bridge_error(error: BridgeError<B>) -> BrowserSidecarError {
        BrowserSidecarError::Bridge(format!("{error:?}"))
    }

    fn sidecar_core_error(
        error: agentos_native_sidecar_core::SidecarCoreError,
    ) -> BrowserSidecarError {
        BrowserSidecarError::InvalidState(error.to_string())
    }

    fn kernel_error(error: KernelError) -> BrowserSidecarError {
        BrowserSidecarError::Kernel(error.to_string())
    }

    fn apply_root_filesystem_entry(
        &mut self,
        vm_id: &str,
        entry: &RootFilesystemEntry,
    ) -> Result<(), BrowserSidecarError> {
        // Shared with native: write the bootstrap entry through the VM's filesystem
        // (trusted, pre-guest; permissions default to allow at bootstrap time) with
        // deterministic mode/uid/gid defaults — see sidecar-core::apply_root_filesystem_entry.
        let filesystem = self.vm_mut(vm_id)?.kernel.filesystem_mut();
        agentos_native_sidecar_core::apply_root_filesystem_entry(filesystem, entry)
            .map_err(Self::sidecar_core_error)
    }
}

fn root_snapshot_from_entries(
    entries: &[RootFilesystemEntry],
) -> Result<RootFilesystemSnapshot, BrowserSidecarError> {
    // Shared with native (sidecar-core): protocol entries -> kernel snapshot.
    agentos_native_sidecar_core::root_snapshot_from_entries(entries)
        .map_err(|error| BrowserSidecarError::InvalidState(error.to_string()))
}

#[cfg(not(target_arch = "wasm32"))]
fn browser_vm_fetch_timeout() -> Duration {
    std::env::var(BROWSER_VM_FETCH_TIMEOUT_MS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(BROWSER_VM_FETCH_TIMEOUT)
}

#[cfg(target_arch = "wasm32")]
fn browser_vm_fetch_timeout() -> Duration {
    BROWSER_VM_FETCH_TIMEOUT
}

fn socket_host_matches(requested: Option<&str>, actual: &str) -> bool {
    match requested {
        None => true,
        Some(requested) if requested == actual => true,
        Some(requested)
            if is_unspecified_socket_host(requested) && is_unspecified_socket_host(actual) =>
        {
            true
        }
        Some(requested) if is_unspecified_socket_host(requested) => is_loopback_socket_host(actual),
        Some(requested) if requested.eq_ignore_ascii_case("localhost") => {
            is_loopback_socket_host(actual)
        }
        _ => false,
    }
}

fn is_unspecified_socket_host(host: &str) -> bool {
    host == "0.0.0.0" || host == "::"
}

fn is_loopback_socket_host(host: &str) -> bool {
    host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost")
}

impl<B> BrowserExtensionHost for BrowserSidecar<B>
where
    B: BrowserSidecarBridge,
    BridgeError<B>: fmt::Debug,
{
    fn write_file(
        &mut self,
        vm_id: &str,
        path: &str,
        contents: Vec<u8>,
    ) -> Result<(), BrowserSidecarError> {
        BrowserSidecar::write_file(self, vm_id, path, contents)
    }

    fn read_file(&mut self, vm_id: &str, path: &str) -> Result<Vec<u8>, BrowserSidecarError> {
        BrowserSidecar::read_file(self, vm_id, path)
    }

    fn mkdir(
        &mut self,
        vm_id: &str,
        path: &str,
        recursive: bool,
    ) -> Result<(), BrowserSidecarError> {
        BrowserSidecar::mkdir(self, vm_id, path, recursive)
    }

    fn read_dir(&mut self, vm_id: &str, path: &str) -> Result<Vec<String>, BrowserSidecarError> {
        BrowserSidecar::read_dir(self, vm_id, path)
    }

    fn create_javascript_context(
        &mut self,
        request: CreateJavascriptContextRequest,
    ) -> Result<GuestContextHandle, BrowserSidecarError> {
        BrowserSidecar::create_javascript_context(self, request)
    }

    fn create_wasm_context(
        &mut self,
        request: CreateWasmContextRequest,
    ) -> Result<GuestContextHandle, BrowserSidecarError> {
        BrowserSidecar::create_wasm_context(self, request)
    }

    fn start_execution(
        &mut self,
        request: StartExecutionRequest,
    ) -> Result<StartedExecution, BrowserSidecarError> {
        BrowserSidecar::start_execution(self, request)
    }

    fn write_stdin(
        &mut self,
        request: WriteExecutionStdinRequest,
    ) -> Result<(), BrowserSidecarError> {
        BrowserSidecar::write_stdin(self, request)
    }

    fn close_stdin(&mut self, request: ExecutionHandleRequest) -> Result<(), BrowserSidecarError> {
        BrowserSidecar::close_stdin(self, request)
    }

    fn kill_execution(&mut self, request: KillExecutionRequest) -> Result<(), BrowserSidecarError> {
        BrowserSidecar::kill_execution(self, request)
    }

    fn poll_execution_event(
        &mut self,
        request: PollExecutionEventRequest,
    ) -> Result<Option<ExecutionEvent>, BrowserSidecarError> {
        BrowserSidecar::poll_execution_event(self, request)
    }
}

fn runtime_label(runtime: GuestRuntime) -> &'static str {
    match runtime {
        GuestRuntime::JavaScript => "javascript",
        GuestRuntime::WebAssembly => "webassembly",
    }
}

fn protocol_signal_registration(
    registration: &SignalHandlerRegistration,
) -> ProtocolSignalHandlerRegistration {
    ProtocolSignalHandlerRegistration {
        action: match registration.action {
            SignalDispositionAction::Default => ProtocolSignalDispositionAction::Default,
            SignalDispositionAction::Ignore => ProtocolSignalDispositionAction::Ignore,
            SignalDispositionAction::User => ProtocolSignalDispositionAction::User,
        },
        mask: registration.mask.clone(),
        flags: registration.flags,
    }
}

#[cfg(test)]
impl<B> BrowserSidecar<B>
where
    B: BrowserSidecarBridge,
    BridgeError<B>: fmt::Debug,
{
    /// Test-only: number of entries still tracked in the global `contexts` map.
    pub(crate) fn test_total_context_count(&self) -> usize {
        self.contexts.len()
    }

    /// Test-only: number of entries still tracked in the global `executions` map.
    pub(crate) fn test_total_execution_count(&self) -> usize {
        self.executions.len()
    }

    /// Test-only: inject a context directly into both the global `contexts` map
    /// and the owning VM's context set, bypassing the bridge round-trip so a
    /// dispose-path test can exercise cleanup at the smallest seam.
    pub(crate) fn test_insert_context(&mut self, vm_id: &str, context_id: &str) {
        self.contexts.insert(
            context_id.to_string(),
            ContextState {
                vm_id: vm_id.to_string(),
                runtime: GuestRuntime::JavaScript,
                entrypoint: BrowserWorkerEntrypoint::JavaScript {
                    bootstrap_module: None,
                },
            },
        );
        if let Some(vm) = self.vms.get_mut(vm_id) {
            vm.contexts.insert(context_id.to_string());
        }
    }

    /// Test-only: inject an active execution directly into both the global
    /// `executions` map and the owning VM's active-execution set.
    pub(crate) fn test_insert_execution(&mut self, vm_id: &str, execution_id: &str) {
        self.executions.insert(
            execution_id.to_string(),
            ExecutionState {
                vm_id: vm_id.to_string(),
                worker: BrowserWorkerHandle {
                    worker_id: format!("worker-{execution_id}"),
                    runtime: GuestRuntime::JavaScript,
                },
                kernel_pid: 0,
                stdin_write_fd: 0,
                cwd: String::new(),
            },
        );
        if let Some(vm) = self.vms.get_mut(vm_id) {
            vm.active_executions.insert(execution_id.to_string());
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // browser_worker_identity below is shared with the worker shim
mod tests {
    use super::*;
    use agentos_bridge::{
        ChmodRequest, ClockRequest, CommandPermissionRequest, CreateDirRequest, DiagnosticRecord,
        DirectoryEntry, EnvironmentPermissionRequest, ExecutionHandleRequest, FileMetadata,
        FilesystemPermissionRequest, FilesystemSnapshot, FlushFilesystemStateRequest,
        LoadFilesystemStateRequest, LogRecord, NetworkPermissionRequest, PathRequest,
        PermissionDecision, RandomBytesRequest, ReadDirRequest, ReadFileRequest, RenameRequest,
        ScheduleTimerRequest, ScheduledTimer, SymlinkRequest, TruncateRequest, WriteFileRequest,
    };
    use agentos_bridge::{
        ClockBridge, EventBridge, ExecutionBridge, FilesystemBridge, PermissionBridge,
        PersistenceBridge, RandomBridge,
    };
    use agentos_kernel::kernel::KernelVmConfig;
    use agentos_kernel::permissions::Permissions;
    use agentos_native_sidecar_core::ca::{
        CA_CERTIFICATES_BUNDLE, CA_CERTIFICATES_GUEST_PATH, CA_CERTIFICATES_SYMLINK_PATH,
    };
    use std::time::SystemTime;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestBridgeError(String);

    /// Minimal bridge whose `terminate_worker` can be forced to fail, used to
    /// drive a mid-dispose error through `release_execution`.
    #[derive(Default)]
    struct TerminateFailingBridge {
        fail_terminate: bool,
    }

    impl BridgeTypes for TerminateFailingBridge {
        type Error = TestBridgeError;
    }

    impl FilesystemBridge for TerminateFailingBridge {
        fn read_file(&mut self, _request: ReadFileRequest) -> Result<Vec<u8>, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn write_file(&mut self, _request: WriteFileRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn stat(&mut self, _request: PathRequest) -> Result<FileMetadata, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn lstat(&mut self, _request: PathRequest) -> Result<FileMetadata, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn read_dir(
            &mut self,
            _request: ReadDirRequest,
        ) -> Result<Vec<DirectoryEntry>, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn create_dir(&mut self, _request: CreateDirRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn remove_file(&mut self, _request: PathRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn remove_dir(&mut self, _request: PathRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn rename(&mut self, _request: RenameRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn symlink(&mut self, _request: SymlinkRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn read_link(&mut self, _request: PathRequest) -> Result<String, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn chmod(&mut self, _request: ChmodRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn truncate(&mut self, _request: TruncateRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn exists(&mut self, _request: PathRequest) -> Result<bool, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
    }

    impl PermissionBridge for TerminateFailingBridge {
        fn check_filesystem_access(
            &mut self,
            _request: FilesystemPermissionRequest,
        ) -> Result<PermissionDecision, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn check_network_access(
            &mut self,
            _request: NetworkPermissionRequest,
        ) -> Result<PermissionDecision, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn check_command_execution(
            &mut self,
            _request: CommandPermissionRequest,
        ) -> Result<PermissionDecision, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn check_environment_access(
            &mut self,
            _request: EnvironmentPermissionRequest,
        ) -> Result<PermissionDecision, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
    }

    impl PersistenceBridge for TerminateFailingBridge {
        fn load_filesystem_state(
            &mut self,
            _request: LoadFilesystemStateRequest,
        ) -> Result<Option<FilesystemSnapshot>, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn flush_filesystem_state(
            &mut self,
            _request: FlushFilesystemStateRequest,
        ) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
    }

    impl ClockBridge for TerminateFailingBridge {
        fn wall_clock(&mut self, _request: ClockRequest) -> Result<SystemTime, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn monotonic_clock(&mut self, _request: ClockRequest) -> Result<Duration, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn schedule_timer(
            &mut self,
            _request: ScheduleTimerRequest,
        ) -> Result<ScheduledTimer, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
    }

    impl RandomBridge for TerminateFailingBridge {
        fn fill_random_bytes(
            &mut self,
            _request: RandomBytesRequest,
        ) -> Result<Vec<u8>, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
    }

    impl EventBridge for TerminateFailingBridge {
        fn emit_structured_event(
            &mut self,
            _event: StructuredEventRecord,
        ) -> Result<(), Self::Error> {
            Ok(())
        }
        fn emit_diagnostic(&mut self, _event: DiagnosticRecord) -> Result<(), Self::Error> {
            Ok(())
        }
        fn emit_log(&mut self, _event: LogRecord) -> Result<(), Self::Error> {
            Ok(())
        }
        fn emit_lifecycle(&mut self, _event: LifecycleEventRecord) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl ExecutionBridge for TerminateFailingBridge {
        fn create_javascript_context(
            &mut self,
            _request: CreateJavascriptContextRequest,
        ) -> Result<GuestContextHandle, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn create_wasm_context(
            &mut self,
            _request: CreateWasmContextRequest,
        ) -> Result<GuestContextHandle, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn start_execution(
            &mut self,
            _request: StartExecutionRequest,
        ) -> Result<StartedExecution, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn write_stdin(&mut self, _request: WriteExecutionStdinRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn close_stdin(&mut self, _request: ExecutionHandleRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn kill_execution(&mut self, _request: KillExecutionRequest) -> Result<(), Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
        fn poll_execution_event(
            &mut self,
            _request: PollExecutionEventRequest,
        ) -> Result<Option<ExecutionEvent>, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }
    }

    impl crate::BrowserWorkerBridge for TerminateFailingBridge {
        fn create_worker(
            &mut self,
            _request: BrowserWorkerSpawnRequest,
        ) -> Result<BrowserWorkerHandle, Self::Error> {
            unimplemented!("not exercised by dispose test")
        }

        fn terminate_worker(
            &mut self,
            _request: BrowserWorkerHandleRequest,
        ) -> Result<(), Self::Error> {
            if self.fail_terminate {
                Err(TestBridgeError(String::from("forced terminate failure")))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn browser_vm_bootstraps_ca_before_locking_read_only_root() {
        let mut sidecar = BrowserSidecar::new(
            TerminateFailingBridge::default(),
            BrowserSidecarConfig::default(),
        );
        let custom_cert_pem = "browser custom cert.pem\n";
        let mut kernel_config = KernelVmConfig::new("vm-ca");
        kernel_config.permissions = Permissions::allow_all();
        sidecar
            .create_vm_with_root_filesystem(
                kernel_config,
                RootFilesystemConfig {
                    mode: agentos_vm_config::RootFilesystemMode::ReadOnly,
                    disable_default_base_layer: true,
                    bootstrap_entries: vec![agentos_vm_config::RootFilesystemEntry {
                        path: CA_CERTIFICATES_SYMLINK_PATH.to_string(),
                        kind: agentos_vm_config::RootFilesystemEntryKind::File,
                        mode: None,
                        uid: None,
                        gid: None,
                        content: Some(custom_cert_pem.to_string()),
                        encoding: Some(agentos_vm_config::RootFilesystemEntryEncoding::Utf8),
                        target: None,
                        executable: false,
                    }],
                    ..RootFilesystemConfig::default()
                },
            )
            .expect("create browser VM with read-only root");

        assert_eq!(
            sidecar
                .read_file("vm-ca", CA_CERTIFICATES_GUEST_PATH)
                .expect("read browser VM CA bundle"),
            CA_CERTIFICATES_BUNDLE
        );
        assert_eq!(
            sidecar
                .read_file("vm-ca", CA_CERTIFICATES_SYMLINK_PATH)
                .expect("read custom regular cert.pem"),
            custom_cert_pem.as_bytes()
        );
        assert!(
            sidecar
                .write_file("vm-ca", CA_CERTIFICATES_GUEST_PATH, b"replacement")
                .is_err(),
            "browser service must finish read-only bootstrap before returning"
        );
    }

    // A mid-dispose worker-termination failure must still drain the VM, context,
    // and execution bookkeeping for that id — otherwise the VmState (holding a
    // BrowserKernel) and ContextState leak for the process lifetime.
    #[test]
    fn dispose_vm_drains_maps_even_when_worker_termination_fails() {
        let bridge = TerminateFailingBridge {
            fail_terminate: true,
        };
        let mut sidecar = BrowserSidecar::new(bridge, BrowserSidecarConfig::default());

        sidecar
            .create_vm(KernelVmConfig::new("vm-leak"))
            .expect("create vm");
        sidecar.test_insert_context("vm-leak", "ctx-leak");
        sidecar.test_insert_execution("vm-leak", "exec-leak");

        assert_eq!(sidecar.vm_count(), 1);
        assert_eq!(sidecar.test_total_context_count(), 1);
        assert_eq!(sidecar.test_total_execution_count(), 1);

        // The forced terminate_worker failure surfaces as an error, but the
        // dispose must still have reclaimed every entry for `vm-leak`.
        let result = sidecar.dispose_vm("vm-leak");
        assert!(result.is_err(), "forced terminate failure should surface");

        assert_eq!(sidecar.vm_count(), 0, "VmState leaked after failed dispose");
        assert_eq!(
            sidecar.test_total_context_count(),
            0,
            "ContextState leaked after failed dispose"
        );
        assert_eq!(
            sidecar.test_total_execution_count(),
            0,
            "ExecutionState leaked after failed dispose"
        );
    }
}

fn browser_worker_identity(
    kernel: &BrowserKernel,
    request: &StartExecutionRequest,
    kernel_pid: u32,
) -> (BrowserWorkerProcessConfig, BrowserWorkerOsConfig) {
    let mut env = kernel.environment().clone();
    env.extend(request.env.clone());
    let user = kernel.user_profile();
    let resource_limits = kernel.resource_limits();
    let identity =
        shared_guest_runtime_identity(&user, resource_limits, Some(u64::from(kernel_pid)), Some(0));

    (
        BrowserWorkerProcessConfig {
            cwd: request.cwd.clone(),
            env,
            argv: request.argv.clone(),
            platform: identity.process_platform.clone(),
            arch: identity.process_arch.clone(),
            version: String::from("v22.0.0"),
            pid: kernel_pid,
            ppid: 0,
            uid: identity.virtual_uid as u32,
            gid: identity.virtual_gid as u32,
        },
        BrowserWorkerOsConfig {
            platform: identity.process_platform,
            arch: identity.process_arch.clone(),
            r#type: identity.os_type,
            release: identity.os_release,
            version: identity.os_version,
            cpu_count: identity.os_cpu_count,
            totalmem: identity.os_totalmem,
            freemem: identity.os_freemem,
            hostname: identity.os_hostname,
            homedir: identity.os_homedir,
            tmpdir: identity.os_tmpdir,
            machine: identity.os_machine,
            user: identity.os_user,
            shell: identity.os_shell,
            uid: identity.virtual_uid as u32,
            gid: identity.virtual_gid as u32,
        },
    )
}
