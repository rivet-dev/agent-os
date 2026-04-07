//! VM lifecycle functions: create, configure, dispose, bootstrap, snapshot.
//!
//! Extracted from service.rs as part of the service.rs split (Step 0a).
//! Contains VM lifecycle methods on NativeSidecar<B> and associated helpers.

use crate::bootstrap::{
    apply_root_filesystem_entry, build_root_filesystem, discover_command_guest_paths,
    root_snapshot_entry,
};
use crate::bridge::{bridge_permissions, MountPluginContext};
use crate::protocol::{
    ConfigureVmRequest, DisposeReason, EventFrame, ResponsePayload, RootFilesystemEntry,
    RootFilesystemSnapshotResponse, SnapshotRootFilesystemRequest, VmConfiguredResponse,
    VmCreatedResponse, VmDisposedResponse, VmLifecycleState,
};
use crate::service::{
    audit_fields, emit_security_audit_event, kernel_error, plugin_error, root_filesystem_error,
};
use crate::state::{
    BridgeError, VmConfiguration, VmDnsConfig, VmState, DISPOSE_VM_SIGKILL_GRACE,
    DISPOSE_VM_SIGTERM_GRACE, EXECUTION_DRIVER_NAME, JAVASCRIPT_COMMAND, PYTHON_COMMAND,
    WASM_COMMAND,
};
use crate::{DispatchResult, NativeSidecar, NativeSidecarBridge, SidecarError};

use agent_os_bridge::{
    FilesystemSnapshot, FlushFilesystemStateRequest, LifecycleState, LoadFilesystemStateRequest,
};
use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig};
use agent_os_kernel::mount_plugin::OpenFileSystemPluginRequest;
use agent_os_kernel::mount_table::MountOptions;
use agent_os_kernel::permissions::filter_env;
use agent_os_kernel::resource_accounting::ResourceLimits;
use agent_os_kernel::root_fs::{
    encode_snapshot as encode_root_snapshot, ROOT_FILESYSTEM_SNAPSHOT_FORMAT,
};
use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// NativeSidecar VM lifecycle methods
// ---------------------------------------------------------------------------

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) async fn create_vm(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: crate::protocol::CreateVmRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id) = self.session_scope_for(&request.ownership)?;
        self.require_owned_session(&connection_id, &session_id)?;

        self.next_vm_id += 1;
        let vm_id = format!("vm-{}", self.next_vm_id);
        let cwd = resolve_cwd(payload.metadata.get("cwd"))?;
        let resource_limits = parse_resource_limits(&payload.metadata)?;
        let dns = parse_vm_dns_config(&payload.metadata)?;
        let permissions_policy = payload.permissions.clone().unwrap_or_default();
        self.bridge
            .set_vm_permissions(&vm_id, &permissions_policy)?;
        let permissions = bridge_permissions(self.bridge.clone(), &vm_id);
        let guest_env = filter_env(&vm_id, &extract_guest_env(&payload.metadata), &permissions);
        let loaded_snapshot = self.bridge.with_mut(|bridge| {
            bridge.load_filesystem_state(LoadFilesystemStateRequest {
                vm_id: vm_id.clone(),
            })
        })?;

        let mut config = KernelVmConfig::new(vm_id.clone());
        config.cwd = String::from("/");
        config.env = guest_env.clone();
        config.permissions = permissions;
        config.resources = resource_limits;
        let root_filesystem =
            build_root_filesystem(&payload.root_filesystem, loaded_snapshot.as_ref())?;
        let mut kernel = KernelVm::new(
            agent_os_kernel::mount_table::MountTable::new(root_filesystem),
            config,
        );
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                [JAVASCRIPT_COMMAND, PYTHON_COMMAND, WASM_COMMAND],
            ))
            .map_err(kernel_error)?;
        kernel
            .root_filesystem_mut()
            .expect("native sidecar root filesystem should exist")
            .finish_bootstrap();

        self.bridge
            .emit_lifecycle(&vm_id, LifecycleState::Starting)?;
        self.bridge.emit_lifecycle(&vm_id, LifecycleState::Ready)?;
        self.bridge.emit_log(
            &vm_id,
            format!("created VM {vm_id} for session {session_id}"),
        )?;

        self.sessions
            .get_mut(&session_id)
            .expect("owned session should exist")
            .vm_ids
            .insert(vm_id.clone());
        self.vms.insert(
            vm_id.clone(),
            VmState {
                connection_id: connection_id.clone(),
                session_id: session_id.clone(),
                metadata: payload.metadata,
                dns,
                guest_env,
                requested_runtime: payload.runtime,
                cwd,
                kernel,
                loaded_snapshot,
                configuration: VmConfiguration::default(),
                command_guest_paths: BTreeMap::new(),
                command_permissions: BTreeMap::new(),
                active_processes: BTreeMap::new(),
                signal_states: BTreeMap::new(),
            },
        );

        let events = vec![
            self.vm_lifecycle_event(
                &connection_id,
                &session_id,
                &vm_id,
                VmLifecycleState::Creating,
            ),
            self.vm_lifecycle_event(&connection_id, &session_id, &vm_id, VmLifecycleState::Ready),
        ];

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::VmCreated(VmCreatedResponse { vm_id }),
            ),
            events,
        })
    }

    pub(crate) async fn dispose_vm(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: crate::protocol::DisposeVmRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        let events = self
            .dispose_vm_internal(&connection_id, &session_id, &vm_id, payload.reason)
            .await?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::VmDisposed(VmDisposedResponse { vm_id }),
            ),
            events,
        })
    }

    pub(crate) async fn bootstrap_root_filesystem(
        &mut self,
        request: &crate::protocol::RequestFrame,
        entries: Vec<RootFilesystemEntry>,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let root = vm.kernel.root_filesystem_mut().ok_or_else(|| {
            SidecarError::InvalidState(String::from("VM root filesystem is unavailable"))
        })?;
        for entry in &entries {
            apply_root_filesystem_entry(root, entry)?;
        }

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::RootFilesystemBootstrapped(
                    crate::protocol::RootFilesystemBootstrappedResponse {
                        entry_count: entries.len() as u32,
                    },
                ),
            ),
            events: Vec::new(),
        })
    }

    pub(crate) async fn configure_vm(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: ConfigureVmRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let mount_plugins = &self.mount_plugins;
        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        reconcile_mounts(
            mount_plugins,
            vm,
            &payload.mounts,
            MountPluginContext {
                bridge: self.bridge.clone(),
                vm_id: vm_id.clone(),
            },
        )?;
        vm.command_guest_paths = discover_command_guest_paths(&mut vm.kernel);
        let mut execution_commands = vec![
            String::from(JAVASCRIPT_COMMAND),
            String::from(PYTHON_COMMAND),
            String::from(WASM_COMMAND),
        ];
        execution_commands.extend(vm.command_guest_paths.keys().cloned());
        vm.kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                execution_commands,
            ))
            .map_err(kernel_error)?;
        vm.command_permissions = payload.command_permissions.clone();
        let configured_permissions = payload
            .permissions
            .clone()
            .unwrap_or_else(|| vm.configuration.permissions.clone());
        vm.configuration = VmConfiguration {
            mounts: payload.mounts.clone(),
            software: payload.software.clone(),
            permissions: configured_permissions.clone(),
            instructions: payload.instructions.clone(),
            projected_modules: payload.projected_modules.clone(),
            command_permissions: payload.command_permissions.clone(),
        };
        if let Some(permissions) = payload.permissions.as_ref() {
            self.bridge.set_vm_permissions(&vm_id, permissions)?;
        }

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::VmConfigured(VmConfiguredResponse {
                    applied_mounts: payload.mounts.len() as u32,
                    applied_software: payload.software.len() as u32,
                }),
            ),
            events: Vec::new(),
        })
    }

    pub(crate) async fn snapshot_root_filesystem(
        &mut self,
        request: &crate::protocol::RequestFrame,
        _payload: SnapshotRootFilesystemRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let snapshot = vm.kernel.snapshot_root_filesystem().map_err(kernel_error)?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::RootFilesystemSnapshot(RootFilesystemSnapshotResponse {
                    entries: snapshot.entries.iter().map(root_snapshot_entry).collect(),
                }),
            ),
            events: Vec::new(),
        })
    }

    pub(crate) async fn dispose_vm_internal(
        &mut self,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        _reason: DisposeReason,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        self.require_owned_vm(connection_id, session_id, vm_id)?;

        let mut events = vec![self.vm_lifecycle_event(
            connection_id,
            session_id,
            vm_id,
            VmLifecycleState::Disposing,
        )];
        self.terminate_vm_processes(vm_id, &mut events).await?;

        let mut vm = self
            .vms
            .remove(vm_id)
            .expect("owned VM should exist before disposal");
        let snapshot = FilesystemSnapshot {
            format: String::from(ROOT_FILESYSTEM_SNAPSHOT_FORMAT),
            bytes: encode_root_snapshot(
                &vm.kernel.snapshot_root_filesystem().map_err(kernel_error)?,
            )
            .map_err(root_filesystem_error)?,
        };

        self.bridge
            .emit_lifecycle(vm_id, LifecycleState::Terminated)?;
        vm.kernel.dispose().map_err(kernel_error)?;
        self.bridge.with_mut(|bridge| {
            bridge.flush_filesystem_state(FlushFilesystemStateRequest {
                vm_id: vm_id.to_owned(),
                snapshot,
            })
        })?;
        self.bridge.clear_vm_permissions(vm_id)?;
        self.javascript_engine.dispose_vm(vm_id);
        self.python_engine.dispose_vm(vm_id);
        self.wasm_engine.dispose_vm(vm_id);

        if let Some(session) = self.sessions.get_mut(session_id) {
            session.vm_ids.remove(vm_id);
        }

        events.push(self.vm_lifecycle_event(
            connection_id,
            session_id,
            vm_id,
            VmLifecycleState::Disposed,
        ));
        Ok(events)
    }

    pub(crate) async fn terminate_vm_processes(
        &mut self,
        vm_id: &str,
        events: &mut Vec<EventFrame>,
    ) -> Result<(), SidecarError> {
        let process_ids = self
            .vms
            .get(vm_id)
            .map(|vm| vm.active_processes.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        if process_ids.is_empty() {
            return Ok(());
        }

        for process_id in process_ids {
            if self
                .vms
                .get(vm_id)
                .is_some_and(|vm| vm.active_processes.contains_key(&process_id))
            {
                self.kill_process_internal(vm_id, &process_id, "SIGTERM")?;
            }
        }
        self.wait_for_vm_processes_to_exit(vm_id, DISPOSE_VM_SIGTERM_GRACE, events)
            .await?;

        if !self.vm_has_active_processes(vm_id) {
            return Ok(());
        }

        let remaining = self
            .vms
            .get(vm_id)
            .map(|vm| vm.active_processes.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for process_id in remaining {
            if self
                .vms
                .get(vm_id)
                .is_some_and(|vm| vm.active_processes.contains_key(&process_id))
            {
                self.kill_process_internal(vm_id, &process_id, "SIGKILL")?;
            }
        }
        self.wait_for_vm_processes_to_exit(vm_id, DISPOSE_VM_SIGKILL_GRACE, events)
            .await?;

        if self.vm_has_active_processes(vm_id) {
            return Err(SidecarError::Execution(format!(
                "failed to terminate active guest executions for VM {vm_id}"
            )));
        }

        Ok(())
    }

    pub(crate) async fn wait_for_vm_processes_to_exit(
        &mut self,
        vm_id: &str,
        timeout: Duration,
        events: &mut Vec<EventFrame>,
    ) -> Result<(), SidecarError> {
        let ownership = self.vm_ownership(vm_id)?;
        let deadline = Instant::now() + timeout;

        while self.vm_has_active_processes(vm_id) && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Some(event) = self
                .poll_event(&ownership, remaining.min(Duration::from_millis(10)))
                .await?
            {
                events.push(event);
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Free functions — VM lifecycle helpers
// ---------------------------------------------------------------------------

fn reconcile_mounts<B>(
    mount_plugins: &agent_os_kernel::mount_plugin::FileSystemPluginRegistry<MountPluginContext<B>>,
    vm: &mut VmState,
    mounts: &[crate::protocol::MountDescriptor],
    context: MountPluginContext<B>,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    for existing in &vm.configuration.mounts {
        match vm.kernel.unmount_filesystem(&existing.guest_path) {
            Ok(()) => emit_security_audit_event(
                &context.bridge,
                &context.vm_id,
                "security.mount.unmounted",
                audit_fields([
                    (String::from("guest_path"), existing.guest_path.clone()),
                    (String::from("plugin_id"), existing.plugin.id.clone()),
                    (String::from("read_only"), existing.read_only.to_string()),
                ]),
            ),
            Err(error) if error.code() == "EINVAL" => {}
            Err(error) => return Err(kernel_error(error)),
        }
    }

    for mount in mounts {
        let filesystem = mount_plugins
            .open(
                &mount.plugin.id,
                OpenFileSystemPluginRequest {
                    vm_id: &context.vm_id,
                    guest_path: &mount.guest_path,
                    read_only: mount.read_only,
                    config: &mount.plugin.config,
                    context: &context,
                },
            )
            .map_err(plugin_error)?;

        vm.kernel
            .mount_boxed_filesystem(
                &mount.guest_path,
                filesystem,
                MountOptions::new(mount.plugin.id.clone()).read_only(mount.read_only),
            )
            .map_err(kernel_error)?;
        emit_security_audit_event(
            &context.bridge,
            &context.vm_id,
            "security.mount.mounted",
            audit_fields([
                (String::from("guest_path"), mount.guest_path.clone()),
                (String::from("plugin_id"), mount.plugin.id.clone()),
                (String::from("read_only"), mount.read_only.to_string()),
            ]),
        );
    }

    Ok(())
}

fn resolve_cwd(value: Option<&String>) -> Result<PathBuf, SidecarError> {
    match value {
        Some(path) => {
            let cwd = PathBuf::from(path);
            let resolved = if cwd.is_absolute() {
                cwd
            } else {
                std::env::current_dir()
                    .map_err(|error| {
                        SidecarError::Io(format!("failed to resolve current directory: {error}"))
                    })?
                    .join(cwd)
            };
            Ok(resolved)
        }
        None => std::env::current_dir().map_err(|error| {
            SidecarError::Io(format!("failed to resolve current directory: {error}"))
        }),
    }
}

pub(crate) fn extract_guest_env(metadata: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    metadata
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("env.")
                .map(|env_key| (env_key.to_owned(), value.clone()))
        })
        .collect()
}

pub(crate) fn parse_resource_limits(
    metadata: &BTreeMap<String, String>,
) -> Result<ResourceLimits, SidecarError> {
    let mut limits = ResourceLimits::default();
    if metadata.contains_key("resource.max_processes") {
        limits.max_processes = parse_resource_limit(metadata, "resource.max_processes")?;
    }
    if metadata.contains_key("resource.max_open_fds") {
        limits.max_open_fds = parse_resource_limit(metadata, "resource.max_open_fds")?;
    }
    if metadata.contains_key("resource.max_pipes") {
        limits.max_pipes = parse_resource_limit(metadata, "resource.max_pipes")?;
    }
    if metadata.contains_key("resource.max_ptys") {
        limits.max_ptys = parse_resource_limit(metadata, "resource.max_ptys")?;
    }
    if metadata.contains_key("resource.max_sockets") {
        limits.max_sockets = parse_resource_limit(metadata, "resource.max_sockets")?;
    }
    if metadata.contains_key("resource.max_connections") {
        limits.max_connections = parse_resource_limit(metadata, "resource.max_connections")?;
    }
    if metadata.contains_key("resource.max_filesystem_bytes") {
        limits.max_filesystem_bytes =
            parse_resource_limit_u64(metadata, "resource.max_filesystem_bytes")?;
    }
    if metadata.contains_key("resource.max_inode_count") {
        limits.max_inode_count = parse_resource_limit(metadata, "resource.max_inode_count")?;
    }
    if metadata.contains_key("resource.max_blocking_read_ms") {
        limits.max_blocking_read_ms =
            parse_resource_limit_u64(metadata, "resource.max_blocking_read_ms")?;
    }
    if metadata.contains_key("resource.max_pread_bytes") {
        limits.max_pread_bytes = parse_resource_limit(metadata, "resource.max_pread_bytes")?;
    }
    if metadata.contains_key("resource.max_fd_write_bytes") {
        limits.max_fd_write_bytes = parse_resource_limit(metadata, "resource.max_fd_write_bytes")?;
    }
    if metadata.contains_key("resource.max_process_argv_bytes") {
        limits.max_process_argv_bytes =
            parse_resource_limit(metadata, "resource.max_process_argv_bytes")?;
    }
    if metadata.contains_key("resource.max_process_env_bytes") {
        limits.max_process_env_bytes =
            parse_resource_limit(metadata, "resource.max_process_env_bytes")?;
    }
    if metadata.contains_key("resource.max_readdir_entries") {
        limits.max_readdir_entries =
            parse_resource_limit(metadata, "resource.max_readdir_entries")?;
    }
    if metadata.contains_key("resource.max_wasm_fuel") {
        limits.max_wasm_fuel = parse_resource_limit_u64(metadata, "resource.max_wasm_fuel")?;
    }
    if metadata.contains_key("resource.max_wasm_memory_bytes") {
        limits.max_wasm_memory_bytes =
            parse_resource_limit_u64(metadata, "resource.max_wasm_memory_bytes")?;
    }
    if metadata.contains_key("resource.max_wasm_stack_bytes") {
        limits.max_wasm_stack_bytes =
            parse_resource_limit(metadata, "resource.max_wasm_stack_bytes")?;
    }
    Ok(limits)
}

fn parse_resource_limit(
    metadata: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<usize>, SidecarError> {
    let Some(value) = metadata.get(key) else {
        return Ok(None);
    };

    let parsed = value.parse::<usize>().map_err(|error| {
        SidecarError::InvalidState(format!("invalid resource limit {key}={value}: {error}"))
    })?;
    Ok(Some(parsed))
}

fn parse_resource_limit_u64(
    metadata: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<u64>, SidecarError> {
    let Some(value) = metadata.get(key) else {
        return Ok(None);
    };

    let parsed = value.parse::<u64>().map_err(|error| {
        SidecarError::InvalidState(format!("invalid resource limit {key}={value}: {error}"))
    })?;
    Ok(Some(parsed))
}

fn parse_vm_dns_config(metadata: &BTreeMap<String, String>) -> Result<VmDnsConfig, SidecarError> {
    use crate::state::{VM_DNS_OVERRIDE_METADATA_PREFIX, VM_DNS_SERVERS_METADATA_KEY};

    let mut config = VmDnsConfig::default();

    if let Some(value) = metadata.get(VM_DNS_SERVERS_METADATA_KEY) {
        config.name_servers = value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(parse_vm_dns_nameserver)
            .collect::<Result<Vec<_>, _>>()?;
    }

    for (key, value) in metadata {
        let Some(hostname) = key.strip_prefix(VM_DNS_OVERRIDE_METADATA_PREFIX) else {
            continue;
        };
        let normalized_hostname = normalize_dns_hostname(hostname)?;
        let addresses = value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                entry.parse::<IpAddr>().map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "invalid DNS override {key}={value}: {error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if addresses.is_empty() {
            return Err(SidecarError::InvalidState(format!(
                "DNS override {key} must contain at least one IP address"
            )));
        }
        config.overrides.insert(normalized_hostname, addresses);
    }

    Ok(config)
}

fn parse_vm_dns_nameserver(value: &str) -> Result<SocketAddr, SidecarError> {
    use crate::state::VM_DNS_SERVERS_METADATA_KEY;

    if let Ok(address) = value.parse::<SocketAddr>() {
        return Ok(address);
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, 53));
    }
    Err(SidecarError::InvalidState(format!(
        "invalid {} entry {value}; expected IP or IP:port",
        VM_DNS_SERVERS_METADATA_KEY
    )))
}

pub(crate) fn normalize_dns_hostname(hostname: &str) -> Result<String, SidecarError> {
    let normalized = hostname.trim().trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(SidecarError::InvalidState(String::from(
            "DNS hostname must not be empty",
        )));
    }
    Ok(normalized)
}
