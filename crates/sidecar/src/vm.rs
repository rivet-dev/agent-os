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
    audit_fields, emit_security_audit_event, kernel_error, plugin_error, root_filesystem_error,
};
use crate::state::{
    BridgeError, VmConfiguration, VmDnsConfig, VmLayer, VmLayerStore, VmOverlayLayer, VmState,
    DISPOSE_VM_SIGKILL_GRACE, DISPOSE_VM_SIGTERM_GRACE, EXECUTION_DRIVER_NAME,
    JAVASCRIPT_COMMAND, PYTHON_COMMAND, WASM_COMMAND,
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
                layers: VmLayerStore::default(),
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
            mounts: effective_mounts.clone(),
            software: payload.software.clone(),
            permissions: configured_permissions.clone(),
            module_access_cwd: payload.module_access_cwd.clone(),
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
        let layer_id = allocate_vm_layer_id(&mut vm.layers);
        vm.layers
            .layers
            .insert(layer_id.clone(), VmLayer::Writable(new_writable_layer()?));

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
        let layer = vm.layers.layers.remove(&payload.layer_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("unknown layer: {}", payload.layer_id))
        })?;
        let snapshot = match layer {
            VmLayer::Writable(mut filesystem) => filesystem.snapshot().map_err(root_filesystem_error)?,
            VmLayer::Snapshot(_) | VmLayer::Overlay(_) => {
                return Err(SidecarError::InvalidState(format!(
                    "layer {} is not writable",
                    payload.layer_id
                )));
            }
        };
        let layer_id = allocate_vm_layer_id(&mut vm.layers);
        vm.layers
            .layers
            .insert(layer_id.clone(), VmLayer::Snapshot(snapshot));

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
        let layer_id = allocate_vm_layer_id(&mut vm.layers);
        vm.layers.layers.insert(
            layer_id.clone(),
            VmLayer::Snapshot(root_snapshot_from_entries(&payload.entries)?),
        );

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
        let snapshot = materialize_vm_layer_snapshot(&mut vm.layers, &payload.layer_id)?;

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
        for layer_id in &payload.lower_layer_ids {
            if !vm.layers.layers.contains_key(layer_id) {
                return Err(SidecarError::InvalidState(format!(
                    "unknown lower layer: {layer_id}"
                )));
            }
        }
        if let Some(upper_layer_id) = payload.upper_layer_id.as_ref() {
            if !vm.layers.layers.contains_key(upper_layer_id) {
                return Err(SidecarError::InvalidState(format!(
                    "unknown upper layer: {upper_layer_id}"
                )));
            }
        }

        let layer_id = allocate_vm_layer_id(&mut vm.layers);
        vm.layers.layers.insert(
            layer_id.clone(),
            VmLayer::Overlay(VmOverlayLayer {
                mode: match payload.mode {
                    RootFilesystemMode::Ephemeral => KernelRootFilesystemMode::Ephemeral,
                    RootFilesystemMode::ReadOnly => KernelRootFilesystemMode::ReadOnly,
                },
                upper_layer_id: payload.upper_layer_id,
                lower_layer_ids: payload.lower_layer_ids,
            }),
        );

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
    if mounts.iter().any(|mount| mount.guest_path == "/root/node_modules") {
        return Ok(());
    }

    let Some(module_access_cwd) = module_access_cwd else {
        return Ok(());
    };
    let root = resolve_cwd(Some(module_access_cwd))?.join("node_modules");
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
    Ok(())
}

fn allocate_vm_layer_id(layers: &mut VmLayerStore) -> String {
    let layer_id = format!("layer-{}", layers.next_layer_id);
    layers.next_layer_id += 1;
    layer_id
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
