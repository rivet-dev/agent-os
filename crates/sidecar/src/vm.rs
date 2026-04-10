//! VM lifecycle functions: create, configure, dispose, bootstrap, snapshot.
//!
//! Extracted from service.rs as part of the service.rs split (Step 0a).
//! Contains VM lifecycle methods on NativeSidecar<B> and associated helpers.

use crate::bootstrap::{
    apply_root_filesystem_entry, build_root_filesystem, discover_command_guest_paths,
    root_snapshot_entries, root_snapshot_entry, root_snapshot_from_entries,
};
use crate::bridge::{bridge_permissions, MountPluginContext};
use crate::protocol::{
    ConfigureVmRequest, CreateLayerRequest, CreateOverlayRequest, DisposeReason, EventFrame,
    ExportSnapshotRequest, ImportSnapshotRequest, LayerCreatedResponse, LayerSealedResponse,
    MountDescriptor, MountPluginDescriptor, OverlayCreatedResponse, ResponsePayload,
    RootFilesystemEntry, RootFilesystemMode, RootFilesystemSnapshotResponse, SealLayerRequest,
    SnapshotExportedResponse, SnapshotImportedResponse, SnapshotRootFilesystemRequest,
    VmConfiguredResponse, VmCreatedResponse, VmDisposedResponse, VmLifecycleState,
};
use crate::service::{
    audit_fields, emit_security_audit_event, kernel_error, normalize_path, plugin_error,
    root_filesystem_error,
};
use crate::state::{
    BridgeError, VmConfiguration, VmDnsConfig, VmLayer, VmLayerStore, VmOverlayLayer, VmState,
    DISPOSE_VM_SIGKILL_GRACE, DISPOSE_VM_SIGTERM_GRACE, EXECUTION_DRIVER_NAME, JAVASCRIPT_COMMAND,
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
    encode_snapshot as encode_root_snapshot, RootFileSystem,
    RootFilesystemDescriptor as KernelRootFilesystemDescriptor,
    RootFilesystemMode as KernelRootFilesystemMode, RootFilesystemSnapshot,
    ROOT_FILESYSTEM_SNAPSHOT_FORMAT,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SHADOW_ROOT_BOOTSTRAP_DIRS: &[(&str, u32)] = &[
    ("/dev", 0o755),
    ("/proc", 0o755),
    ("/tmp", 0o1777),
    ("/bin", 0o755),
    ("/lib", 0o755),
    ("/sbin", 0o755),
    ("/boot", 0o755),
    ("/etc", 0o755),
    ("/root", 0o755),
    ("/run", 0o755),
    ("/srv", 0o755),
    ("/sys", 0o755),
    ("/opt", 0o755),
    ("/mnt", 0o755),
    ("/media", 0o755),
    ("/home", 0o755),
    ("/usr", 0o755),
    ("/usr/bin", 0o755),
    ("/usr/games", 0o755),
    ("/usr/include", 0o755),
    ("/usr/lib", 0o755),
    ("/usr/libexec", 0o755),
    ("/usr/man", 0o755),
    ("/usr/local", 0o755),
    ("/usr/local/bin", 0o755),
    ("/usr/sbin", 0o755),
    ("/usr/share", 0o755),
    ("/usr/share/man", 0o755),
    ("/var", 0o755),
    ("/var/cache", 0o755),
    ("/var/empty", 0o755),
    ("/var/lib", 0o755),
    ("/var/lock", 0o755),
    ("/var/log", 0o755),
    ("/var/run", 0o755),
    ("/var/spool", 0o755),
    ("/var/tmp", 0o1777),
    ("/etc/agentos", 0o755),
];

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
        let cwd = create_vm_shadow_root(&vm_id)?;
        let (guest_cwd, host_cwd) = resolve_vm_cwds(payload.metadata.get("cwd"), &cwd)?;
        fs::create_dir_all(&host_cwd)
            .map_err(|error| SidecarError::Io(format!("failed to create VM cwd: {error}")))?;
        let resource_limits = parse_resource_limits(&payload.metadata)?;
        let dns = parse_vm_dns_config(&payload.metadata)?;
        let permissions_policy = payload.permissions.clone().unwrap_or_default();
        self.bridge
            .set_vm_permissions(&vm_id, &permissions_policy)?;
        let permissions = bridge_permissions(self.bridge.clone(), &vm_id);
        let mut guest_env = filter_env(&vm_id, &extract_guest_env(&payload.metadata), &permissions);
        let loaded_snapshot = self.bridge.with_mut(|bridge| {
            bridge.load_filesystem_state(LoadFilesystemStateRequest {
                vm_id: vm_id.clone(),
            })
        })?;

        let mut config = KernelVmConfig::new(vm_id.clone());
        config.cwd = String::from("/");
        config.env = guest_env.clone();
        config.permissions = permissions;
        config.dns = agent_os_kernel::dns::DnsConfig {
            name_servers: dns.name_servers.clone(),
            overrides: dns.overrides.clone(),
        };
        config.resources = resource_limits;
        let root_filesystem =
            build_root_filesystem(&payload.root_filesystem, loaded_snapshot.as_ref())?;
        let mut kernel = KernelVm::new(
            agent_os_kernel::mount_table::MountTable::new(root_filesystem),
            config,
        );
        let command_guest_paths = discover_command_guest_paths(&mut kernel);
        refresh_guest_command_path_env(&mut guest_env, &command_guest_paths);
        let mut execution_commands =
            vec![String::from(JAVASCRIPT_COMMAND), String::from(WASM_COMMAND)];
        execution_commands.extend(command_guest_paths.keys().cloned());
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                execution_commands,
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
                guest_cwd,
                cwd,
                host_cwd,
                kernel,
                loaded_snapshot,
                configuration: VmConfiguration::default(),
                layers: VmLayerStore::default(),
                command_guest_paths,
                command_permissions: BTreeMap::new(),
                toolkits: BTreeMap::new(),
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
        let mut effective_mounts = payload.mounts.clone();
        append_module_access_mount(&mut effective_mounts, payload.module_access_cwd.as_ref())?;
        reconcile_mounts(
            mount_plugins,
            vm,
            &effective_mounts,
            MountPluginContext {
                bridge: self.bridge.clone(),
                connection_id: connection_id.clone(),
                session_id: session_id.clone(),
                vm_id: vm_id.clone(),
                sidecar_requests: self.sidecar_requests.clone(),
            },
        )?;
        vm.command_guest_paths = discover_command_guest_paths(&mut vm.kernel);
        refresh_guest_command_path_env(&mut vm.guest_env, &vm.command_guest_paths);
        let mut execution_commands =
            vec![String::from(JAVASCRIPT_COMMAND), String::from(WASM_COMMAND)];
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
            mounts: effective_mounts.clone(),
            software: payload.software.clone(),
            permissions: configured_permissions.clone(),
            module_access_cwd: payload.module_access_cwd.clone(),
            instructions: payload.instructions.clone(),
            projected_modules: payload.projected_modules.clone(),
            command_permissions: payload.command_permissions.clone(),
            allowed_node_builtins: payload.allowed_node_builtins.clone(),
            loopback_exempt_ports: payload.loopback_exempt_ports.clone(),
        };
        if let Some(permissions) = payload.permissions.as_ref() {
            self.bridge.set_vm_permissions(&vm_id, permissions)?;
        }

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::VmConfigured(VmConfiguredResponse {
                    applied_mounts: effective_mounts.len() as u32,
                    applied_software: payload.software.len() as u32,
                }),
            ),
            events: Vec::new(),
        })
    }

    pub(crate) async fn create_layer(
        &mut self,
        request: &crate::protocol::RequestFrame,
        _payload: CreateLayerRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let layer_id = vm.layers.create_writable_layer()?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::LayerCreated(LayerCreatedResponse { layer_id }),
            ),
            events: Vec::new(),
        })
    }

    pub(crate) async fn seal_layer(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: SealLayerRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let layer_id = vm.layers.seal_layer(&payload.layer_id)?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::LayerSealed(LayerSealedResponse { layer_id }),
            ),
            events: Vec::new(),
        })
    }

    pub(crate) async fn import_snapshot(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: ImportSnapshotRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let layer_id = vm
            .layers
            .import_snapshot(root_snapshot_from_entries(&payload.entries)?);

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::SnapshotImported(SnapshotImportedResponse { layer_id }),
            ),
            events: Vec::new(),
        })
    }

    pub(crate) async fn export_snapshot(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: ExportSnapshotRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let snapshot = vm.layers.export_snapshot(&payload.layer_id)?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::SnapshotExported(SnapshotExportedResponse {
                    layer_id: payload.layer_id,
                    entries: root_snapshot_entries(&snapshot),
                }),
            ),
            events: Vec::new(),
        })
    }

    pub(crate) async fn create_overlay(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: CreateOverlayRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let layer_id = vm.layers.create_overlay_layer(
            match payload.mode {
                RootFilesystemMode::Ephemeral => KernelRootFilesystemMode::Ephemeral,
                RootFilesystemMode::ReadOnly => KernelRootFilesystemMode::ReadOnly,
            },
            payload.upper_layer_id,
            payload.lower_layer_ids,
        )?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::OverlayCreated(OverlayCreatedResponse { layer_id }),
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
        let _ = fs::remove_dir_all(&vm.cwd);

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

fn append_module_access_mount(
    mounts: &mut Vec<MountDescriptor>,
    module_access_cwd: Option<&String>,
) -> Result<(), SidecarError> {
    if mounts
        .iter()
        .any(|mount| mount.guest_path == "/root/node_modules")
    {
        return Ok(());
    }

    let Some(module_access_cwd) = module_access_cwd else {
        return Ok(());
    };
    let root = resolve_host_path(Some(module_access_cwd))?.join("node_modules");
    if !root.is_dir() {
        return Ok(());
    }

    mounts.push(MountDescriptor {
        guest_path: String::from("/root/node_modules"),
        read_only: true,
        plugin: MountPluginDescriptor {
            id: String::from("module_access"),
            config: serde_json::json!({
                "hostPath": root,
            }),
        },
    });
    append_module_access_symlink_mounts(mounts, &root)?;
    Ok(())
}

fn append_module_access_symlink_mounts(
    mounts: &mut Vec<MountDescriptor>,
    node_modules_root: &Path,
) -> Result<(), SidecarError> {
    for entry in fs::read_dir(node_modules_root)
        .map_err(|error| SidecarError::Io(format!("failed to read module_access root: {error}")))?
    {
        let entry = entry.map_err(|error| {
            SidecarError::Io(format!("failed to inspect module_access root: {error}"))
        })?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            SidecarError::Io(format!("failed to stat module_access entry: {error}"))
        })?;
        if metadata.file_type().is_symlink() {
            append_module_access_symlink_mount(
                mounts,
                &format!("/root/node_modules/{name}"),
                &path,
            )?;
            continue;
        }
        if !metadata.is_dir() || !name.starts_with('@') {
            continue;
        }
        for scoped_entry in fs::read_dir(&path).map_err(|error| {
            SidecarError::Io(format!("failed to read module_access scope: {error}"))
        })? {
            let scoped_entry = scoped_entry.map_err(|error| {
                SidecarError::Io(format!("failed to inspect module_access scope: {error}"))
            })?;
            let scoped_name = scoped_entry.file_name().to_string_lossy().into_owned();
            if scoped_name.starts_with('.') {
                continue;
            }
            let scoped_path = scoped_entry.path();
            let scoped_metadata = fs::symlink_metadata(&scoped_path).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to stat module_access scoped entry: {error}"
                ))
            })?;
            if scoped_metadata.file_type().is_symlink() {
                append_module_access_symlink_mount(
                    mounts,
                    &format!("/root/node_modules/{name}/{scoped_name}"),
                    &scoped_path,
                )?;
            }
        }
    }

    Ok(())
}

fn append_module_access_symlink_mount(
    mounts: &mut Vec<MountDescriptor>,
    guest_path: &str,
    symlink_path: &Path,
) -> Result<(), SidecarError> {
    if mounts.iter().any(|mount| mount.guest_path == guest_path) {
        return Ok(());
    }

    let target = fs::canonicalize(symlink_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to resolve module_access package symlink {}: {error}",
            symlink_path.display()
        ))
    })?;
    if !target.is_dir() {
        return Ok(());
    }

    mounts.push(MountDescriptor {
        guest_path: guest_path.to_owned(),
        read_only: true,
        plugin: MountPluginDescriptor {
            id: String::from("host_dir"),
            config: serde_json::json!({
                "hostPath": target,
                "readOnly": true,
            }),
        },
    });
    Ok(())
}

impl VmLayerStore {
    fn allocate_layer_id(&mut self) -> String {
        let layer_id = format!("layer-{}", self.next_layer_id);
        self.next_layer_id += 1;
        layer_id
    }

    fn create_writable_layer(&mut self) -> Result<String, SidecarError> {
        let layer_id = self.allocate_layer_id();
        self.layers
            .insert(layer_id.clone(), VmLayer::Writable(new_writable_layer()?));
        Ok(layer_id)
    }

    fn seal_layer(&mut self, layer_id: &str) -> Result<String, SidecarError> {
        let layer = self
            .layers
            .remove(layer_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown layer: {layer_id}")))?;
        let snapshot = match layer {
            VmLayer::Writable(mut filesystem) => filesystem.snapshot().map_err(root_filesystem_error)?,
            VmLayer::Snapshot(_) | VmLayer::Overlay(_) => {
                return Err(SidecarError::InvalidState(format!(
                    "layer {layer_id} is not writable"
                )));
            }
        };
        let sealed_layer_id = self.allocate_layer_id();
        self.layers
            .insert(sealed_layer_id.clone(), VmLayer::Snapshot(snapshot));
        Ok(sealed_layer_id)
    }

    fn import_snapshot(&mut self, snapshot: RootFilesystemSnapshot) -> String {
        let layer_id = self.allocate_layer_id();
        self.layers.insert(layer_id.clone(), VmLayer::Snapshot(snapshot));
        layer_id
    }

    fn export_snapshot(&mut self, layer_id: &str) -> Result<RootFilesystemSnapshot, SidecarError> {
        materialize_vm_layer_snapshot(self, layer_id)
    }

    fn create_overlay_layer(
        &mut self,
        mode: KernelRootFilesystemMode,
        upper_layer_id: Option<String>,
        lower_layer_ids: Vec<String>,
    ) -> Result<String, SidecarError> {
        for layer_id in &lower_layer_ids {
            if !self.layers.contains_key(layer_id) {
                return Err(SidecarError::InvalidState(format!(
                    "unknown lower layer: {layer_id}"
                )));
            }
        }
        if let Some(layer_id) = upper_layer_id.as_ref() {
            if !self.layers.contains_key(layer_id) {
                return Err(SidecarError::InvalidState(format!(
                    "unknown upper layer: {layer_id}"
                )));
            }
        }

        let layer_id = self.allocate_layer_id();
        self.layers.insert(
            layer_id.clone(),
            VmLayer::Overlay(VmOverlayLayer {
                mode,
                upper_layer_id,
                lower_layer_ids,
            }),
        );
        Ok(layer_id)
    }
}

fn new_writable_layer() -> Result<RootFileSystem, SidecarError> {
    RootFileSystem::from_descriptor(KernelRootFilesystemDescriptor {
        mode: KernelRootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: Vec::new(),
        bootstrap_entries: Vec::new(),
    })
    .map_err(root_filesystem_error)
}

fn materialize_vm_layer_snapshot(
    layers: &mut VmLayerStore,
    layer_id: &str,
) -> Result<RootFilesystemSnapshot, SidecarError> {
    materialize_vm_layer_snapshot_inner(layers, layer_id, &mut std::collections::BTreeSet::new())
}

fn materialize_vm_layer_snapshot_inner(
    layers: &mut VmLayerStore,
    layer_id: &str,
    active: &mut std::collections::BTreeSet<String>,
) -> Result<RootFilesystemSnapshot, SidecarError> {
    if !active.insert(layer_id.to_owned()) {
        return Err(SidecarError::InvalidState(format!(
            "layer graph cycle detected at {layer_id}"
        )));
    }

    let result = if let Some(VmLayer::Snapshot(snapshot)) = layers.layers.get(layer_id) {
        Ok(snapshot.clone())
    } else if let Some(VmLayer::Overlay(overlay)) = layers.layers.get(layer_id) {
        let overlay = overlay.clone();
        let lowers = overlay
            .lower_layer_ids
            .iter()
            .map(|lower_id| materialize_vm_layer_snapshot_inner(layers, lower_id, active))
            .collect::<Result<Vec<_>, _>>()?;
        let bootstrap_entries = match overlay.upper_layer_id.as_deref() {
            Some(upper_layer_id) => dedupe_overlay_bootstrap_entries(
                &lowers,
                materialize_vm_layer_snapshot_inner(layers, upper_layer_id, active)?.entries,
            ),
            None => Vec::new(),
        };
        let mut root = RootFileSystem::from_descriptor(KernelRootFilesystemDescriptor {
            mode: overlay.mode,
            disable_default_base_layer: true,
            lowers,
            bootstrap_entries,
        })
        .map_err(root_filesystem_error)?;
        root.snapshot().map_err(root_filesystem_error)
    } else if let Some(VmLayer::Writable(filesystem)) = layers.layers.get_mut(layer_id) {
        filesystem.snapshot().map_err(root_filesystem_error)
    } else {
        Err(SidecarError::InvalidState(format!(
            "unknown layer: {layer_id}"
        )))
    };

    active.remove(layer_id);
    result
}

fn dedupe_overlay_bootstrap_entries(
    lowers: &[RootFilesystemSnapshot],
    upper_entries: Vec<agent_os_kernel::root_fs::FilesystemEntry>,
) -> Vec<agent_os_kernel::root_fs::FilesystemEntry> {
    let mut lower_paths = lowers
        .iter()
        .flat_map(|snapshot| snapshot.entries.iter().map(|entry| entry.path.clone()))
        .collect::<std::collections::BTreeSet<_>>();

    upper_entries
        .into_iter()
        .filter(|entry| {
            if lower_paths.contains(&entry.path)
                && matches!(
                    entry.kind,
                    agent_os_kernel::root_fs::FilesystemEntryKind::Directory
                )
            {
                return false;
            }
            lower_paths.insert(entry.path.clone());
            true
        })
        .collect()
}

fn resolve_guest_cwd(value: Option<&String>) -> String {
    value
        .map(|path| normalize_guest_path(path))
        .unwrap_or_else(|| String::from("/"))
}

fn resolve_vm_cwds(
    metadata_cwd: Option<&String>,
    shadow_root: &Path,
) -> Result<(String, PathBuf), SidecarError> {
    if let Some(raw_cwd) = metadata_cwd {
        let candidate = PathBuf::from(raw_cwd);
        if candidate.is_absolute() || raw_cwd.starts_with('.') {
            let resolved_host_cwd = resolve_host_path(Some(raw_cwd))?;
            return Ok((String::from("/"), resolved_host_cwd));
        }
    }

    let guest_cwd = resolve_guest_cwd(metadata_cwd);
    let host_cwd = shadow_path_for_guest(shadow_root, &guest_cwd);
    Ok((guest_cwd, host_cwd))
}

fn resolve_host_path(value: Option<&String>) -> Result<PathBuf, SidecarError> {
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

fn create_vm_shadow_root(vm_id: &str) -> Result<PathBuf, SidecarError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SidecarError::Io(format!("failed to compute shadow-root nonce: {error}")))?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("agent-os-sidecar-shadow-{vm_id}-{nonce}"));
    fs::create_dir_all(&root)
        .map_err(|error| SidecarError::Io(format!("failed to create VM shadow root: {error}")))?;
    bootstrap_shadow_root(&root)?;
    Ok(root)
}

fn bootstrap_shadow_root(root: &Path) -> Result<(), SidecarError> {
    for (guest_path, mode) in SHADOW_ROOT_BOOTSTRAP_DIRS {
        let host_path = shadow_path_for_guest(root, guest_path);
        fs::create_dir_all(&host_path).map_err(|error| {
            SidecarError::Io(format!(
                "failed to create shadow directory {}: {error}",
                host_path.display()
            ))
        })?;
        fs::set_permissions(&host_path, fs::Permissions::from_mode(*mode)).map_err(|error| {
            SidecarError::Io(format!(
                "failed to set shadow directory mode {} on {}: {error}",
                format!("{mode:o}"),
                host_path.display()
            ))
        })?;
    }
    Ok(())
}

fn shadow_path_for_guest(shadow_root: &std::path::Path, guest_path: &str) -> PathBuf {
    let normalized = normalize_guest_path(guest_path);
    let relative = normalized.trim_start_matches('/');
    if relative.is_empty() {
        return shadow_root.to_path_buf();
    }
    shadow_root.join(relative)
}

fn normalize_guest_path(path: &str) -> String {
    let mut segments = Vec::new();
    let absolute = path.starts_with('/');
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }

    if !absolute {
        return format!("/{}", segments.join("/"));
    }
    if segments.is_empty() {
        String::from("/")
    } else {
        format!("/{}", segments.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::{bootstrap_shadow_root, shadow_path_for_guest};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn bootstrap_shadow_root_seeds_standard_directories() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("agent-os-sidecar-shadow-test-{unique}"));
        fs::create_dir_all(&root).expect("temp shadow root should be created");

        bootstrap_shadow_root(&root).expect("shadow bootstrap should succeed");

        let tmp = shadow_path_for_guest(&root, "/tmp");
        let etc_agentos = shadow_path_for_guest(&root, "/etc/agentos");
        let usr_local_bin = shadow_path_for_guest(&root, "/usr/local/bin");

        assert!(tmp.is_dir(), "/tmp should exist in the shadow root");
        assert!(
            etc_agentos.is_dir(),
            "/etc/agentos should exist in the shadow root"
        );
        assert!(
            usr_local_bin.is_dir(),
            "/usr/local/bin should exist in the shadow root"
        );
        assert_eq!(
            fs::metadata(&tmp)
                .expect("/tmp metadata should be readable")
                .permissions()
                .mode()
                & 0o7777,
            0o1777,
            "/tmp should preserve its sticky-bit mode in the shadow root"
        );

        fs::remove_dir_all(&root).expect("temp shadow root should be removed");
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

fn refresh_guest_command_path_env(
    guest_env: &mut BTreeMap<String, String>,
    command_guest_paths: &BTreeMap<String, String>,
) {
    let mut merged = Vec::new();
    let mut seen = BTreeSet::new();

    if !command_guest_paths.is_empty() && seen.insert(String::from("/bin")) {
        merged.push(String::from("/bin"));
    }

    for guest_path in command_guest_paths.values() {
        let Some(parent) = Path::new(guest_path)
            .parent()
            .and_then(|path| path.to_str())
        else {
            continue;
        };
        let normalized = normalize_path(parent);
        if seen.insert(normalized.clone()) {
            merged.push(normalized);
        }
    }

    if let Some(existing_path) = guest_env.get("PATH") {
        for segment in existing_path.split(':') {
            let trimmed = segment.trim();
            if trimmed.is_empty() {
                continue;
            }
            let normalized = if trimmed.starts_with('/') {
                normalize_path(trimmed)
            } else {
                trimmed.to_owned()
            };
            if seen.insert(normalized.clone()) {
                merged.push(normalized);
            }
        }
    }

    if !merged.is_empty() {
        guest_env.insert(String::from("PATH"), merged.join(":"));
    }
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
