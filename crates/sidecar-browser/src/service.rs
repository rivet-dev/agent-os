use crate::{
    BrowserSidecarBridge, BrowserWorkerEntrypoint, BrowserWorkerHandle, BrowserWorkerHandleRequest,
    BrowserWorkerSpawnRequest,
};
use agent_os_bridge::{
    BridgeTypes, CreateJavascriptContextRequest, CreateWasmContextRequest, ExecutionEvent,
    ExecutionHandleRequest, GuestContextHandle, GuestRuntime, KillExecutionRequest,
    LifecycleEventRecord, LifecycleState, PollExecutionEventRequest, StartExecutionRequest,
    StartedExecution, StructuredEventRecord, WriteExecutionStdinRequest,
};
use agent_os_kernel::bridge::LifecycleState as KernelLifecycleState;
use agent_os_kernel::kernel::{KernelError, KernelVm, KernelVmConfig, VirtualProcessOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::vfs::MemoryFileSystem;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::time::Duration;

type BridgeError<B> = <B as BridgeTypes>::Error;
type BrowserKernel = KernelVm<MemoryFileSystem>;
const BROWSER_WORKER_DRIVER: &str = "browser.worker";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSidecarConfig {
    pub sidecar_id: String,
}

impl Default for BrowserSidecarConfig {
    fn default() -> Self {
        Self {
            sidecar_id: String::from("agent-os-sidecar-browser"),
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
    contexts: BTreeSet<String>,
    active_executions: BTreeSet<String>,
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
}

pub struct BrowserSidecar<B> {
    bridge: B,
    config: BrowserSidecarConfig,
    vms: BTreeMap<String, VmState>,
    contexts: BTreeMap<String, ContextState>,
    executions: BTreeMap<String, ExecutionState>,
}

impl<B> BrowserSidecar<B>
where
    B: BrowserSidecarBridge,
    BridgeError<B>: fmt::Debug,
{
    pub fn new(bridge: B, config: BrowserSidecarConfig) -> Self {
        Self {
            bridge,
            config,
            vms: BTreeMap::new(),
            contexts: BTreeMap::new(),
            executions: BTreeMap::new(),
        }
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

    pub fn create_vm(&mut self, mut config: KernelVmConfig) -> Result<(), BrowserSidecarError> {
        let vm_id = config.vm_id.clone();
        if self.vms.contains_key(&vm_id) {
            return Err(BrowserSidecarError::InvalidState(format!(
                "browser sidecar VM already exists: {vm_id}"
            )));
        }

        // Browser-side host capabilities are already mediated at the JS host-bridge
        // boundary, so the in-browser kernel stays permissive and remains the single
        // source of truth for VM-local filesystem, process, and socket state.
        config.permissions = Permissions::allow_all();

        self.emit_lifecycle(
            &vm_id,
            LifecycleState::Starting,
            Some(String::from(
                "browser sidecar booting kernel on main thread",
            )),
        )?;
        self.vms.insert(
            vm_id.clone(),
            VmState {
                kernel: KernelVm::new(MemoryFileSystem::new(), config),
                contexts: BTreeSet::new(),
                active_executions: BTreeSet::new(),
            },
        );
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

    pub fn kernel_state(&self, vm_id: &str) -> Result<LifecycleState, BrowserSidecarError> {
        let vm = self.vm(vm_id)?;
        Ok(match vm.kernel.state() {
            KernelLifecycleState::Starting => LifecycleState::Starting,
            KernelLifecycleState::Ready => LifecycleState::Ready,
            KernelLifecycleState::Busy => LifecycleState::Busy,
            KernelLifecycleState::Terminated => LifecycleState::Terminated,
        })
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
        let Some(vm_state) = self.vms.get(vm_id) else {
            return Err(BrowserSidecarError::InvalidState(format!(
                "unknown browser sidecar VM: {vm_id}"
            )));
        };

        let execution_ids = vm_state
            .active_executions
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for execution_id in execution_ids {
            self.release_execution(&execution_id, "browser.worker.disposed")?;
        }

        let context_ids = self
            .vms
            .get(vm_id)
            .expect("VM should still exist while disposing contexts")
            .contexts
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for context_id in context_ids {
            self.contexts.remove(&context_id);
        }

        self.vms.remove(vm_id);
        self.emit_lifecycle(
            vm_id,
            LifecycleState::Terminated,
            Some(String::from(
                "browser sidecar VM disposed on the main thread",
            )),
        )?;
        Ok(())
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
            let (stdin_read_fd, stdin_write_fd) = vm
                .kernel
                .open_pipe(BROWSER_WORKER_DRIVER, kernel_pid)
                .map_err(Self::kernel_error)?;
            vm.kernel
                .fd_dup2(BROWSER_WORKER_DRIVER, kernel_pid, stdin_read_fd, 0)
                .map_err(Self::kernel_error)?;
            let (_stdout_read_fd, stdout_write_fd) = vm
                .kernel
                .open_pipe(BROWSER_WORKER_DRIVER, kernel_pid)
                .map_err(Self::kernel_error)?;
            vm.kernel
                .fd_dup2(BROWSER_WORKER_DRIVER, kernel_pid, stdout_write_fd, 1)
                .map_err(Self::kernel_error)?;
            let (_stderr_read_fd, stderr_write_fd) = vm
                .kernel
                .open_pipe(BROWSER_WORKER_DRIVER, kernel_pid)
                .map_err(Self::kernel_error)?;
            vm.kernel
                .fd_dup2(BROWSER_WORKER_DRIVER, kernel_pid, stderr_write_fd, 2)
                .map_err(Self::kernel_error)?;
            (kernel_pid, stdin_write_fd)
        };

        let worker = self
            .bridge
            .create_worker(BrowserWorkerSpawnRequest {
                vm_id: request.vm_id.clone(),
                context_id: request.context_id.clone(),
                runtime: context.runtime,
                entrypoint: context.entrypoint.clone(),
            })
            .map_err(Self::bridge_error)?;

        let started = match self.bridge.start_execution(request.clone()) {
            Ok(started) => started,
            Err(error) => {
                self.bridge
                    .terminate_worker(BrowserWorkerHandleRequest {
                        vm_id: request.vm_id,
                        execution_id: String::from("pending"),
                        worker_id: worker.worker_id,
                    })
                    .map_err(Self::bridge_error)?;
                return Err(Self::bridge_error(error));
            }
        };

        self.executions.insert(
            started.execution_id.clone(),
            ExecutionState {
                vm_id: request.vm_id.clone(),
                worker: worker.clone(),
                kernel_pid,
                stdin_write_fd,
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
                (String::from("worker_id"), worker.worker_id),
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
        let execution = self.ensure_execution_state(&request.vm_id, &request.execution_id)?;
        {
            let vm = self.vm_mut(&request.vm_id)?;
            vm.kernel
                .kill_process(
                    BROWSER_WORKER_DRIVER,
                    execution.kernel_pid,
                    execution_signal_to_kernel(request.signal),
                )
                .map_err(Self::kernel_error)?;
        }
        self.bridge
            .kill_execution(request)
            .map_err(Self::bridge_error)
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
            Some(ExecutionEvent::GuestRequest(_)) | None => {}
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
        }

        let vm_id = execution.vm_id;
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

    fn kernel_error(error: KernelError) -> BrowserSidecarError {
        BrowserSidecarError::Kernel(error.to_string())
    }
}

fn runtime_label(runtime: GuestRuntime) -> &'static str {
    match runtime {
        GuestRuntime::JavaScript => "javascript",
        GuestRuntime::WebAssembly => "webassembly",
    }
}

fn execution_signal_to_kernel(signal: agent_os_bridge::ExecutionSignal) -> i32 {
    match signal {
        agent_os_bridge::ExecutionSignal::Terminate => 15,
        agent_os_bridge::ExecutionSignal::Interrupt => 2,
        agent_os_bridge::ExecutionSignal::Kill => 9,
    }
}
