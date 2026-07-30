//! VM lifecycle functions: create, configure, dispose, bootstrap, snapshot.
//!
//! Extracted from service.rs as part of the service.rs split (Step 0a).
//! Contains VM lifecycle methods on NativeSidecar<B> and associated helpers.

use crate::bootstrap::{
    apply_root_filesystem_entry, discover_kernel_commands, root_snapshot_entries,
    root_snapshot_entry, root_snapshot_from_entries, KernelCommandInventory,
};
use crate::bridge::{bridge_permissions, MountPluginContext};
use crate::execution::terminate_child_process_tree;
use crate::protocol::{
    AgentosProjectedAgent, ConfigureVmRequest, CreateLayerRequest, CreateOverlayRequest,
    DisposeReason, EventFrame, ExportSnapshotRequest, ImportSnapshotRequest, LinkPackageRequest,
    ListMountsRequest, MountDescriptor, MountInfo, MountPluginDescriptor, PackageCommands,
    ProjectedCommand, ProvidedCommandsRequest, RootFilesystemDescriptor, RootFilesystemEntry,
    SealLayerRequest, SnapshotRootFilesystemRequest, VmLifecycleState,
};
use crate::service::{
    audit_fields, dirname, emit_security_audit_event, emit_structured_event, kernel_error,
    normalize_path, plugin_error, root_filesystem_error, validate_permissions_policy, vfs_error,
};
use crate::state::{
    BridgeError, KernelSocketReadinessEvent, KernelSocketReadinessRegistry,
    KernelSocketReadinessTarget, QuarantinedVmGeneration, VmConfiguration, VmDnsConfig,
    VmListenPolicy, VmPendingByteBudget, VmQuarantineReason, VmReconciliationSnapshot, VmState,
    DISPOSE_VM_SIGKILL_GRACE, DISPOSE_VM_SIGTERM_GRACE, EXECUTION_DRIVER_NAME, JAVASCRIPT_COMMAND,
    PYTHON_COMMAND, WASM_COMMAND,
};
use crate::{DispatchResult, NativeSidecar, NativeSidecarBridge, SidecarError};

use agentos_bridge::{
    FilesystemSnapshot, FlushFilesystemStateRequest, LifecycleState, LoadFilesystemStateRequest,
};
use agentos_kernel::command_registry::CommandDriver;
use agentos_kernel::kernel::{KernelVm, KernelVmConfig};
use agentos_kernel::mount_plugin::OpenFileSystemPluginRequest;
use agentos_kernel::mount_table::{MountOptions, MountTable, MountedFileSystem};
use agentos_kernel::permissions::filter_env;
use agentos_kernel::root_fs::{
    encode_snapshot as encode_root_snapshot, FilesystemEntryKind as KernelFilesystemEntryKind,
    ROOT_FILESYSTEM_SNAPSHOT_FORMAT,
};
use agentos_kernel::socket_table::{SocketReadiness, SocketReadinessKind};
use agentos_native_sidecar_core::ca::{
    CA_CERTIFICATES_BUNDLE, CA_CERTIFICATES_GUEST_PATH, CA_CERTIFICATES_SYMLINK_PATH,
    CA_CERTIFICATES_SYMLINK_TARGET,
};
use agentos_native_sidecar_core::permissions::{allow_all_policy, deny_all_policy};
use agentos_native_sidecar_core::{
    layer_created_response, layer_sealed_response, mounts_listed_response,
    overlay_created_response, package_linked_response, protocol_root_filesystem_mode,
    provided_commands_response, root_filesystem_bootstrapped_response,
    root_filesystem_protocol_descriptor_from_config, root_filesystem_snapshot_response,
    snapshot_exported_response, snapshot_imported_response, vm_configured_response,
    vm_created_response, vm_disposed_response, VmLayerStore,
};
use agentos_runtime_tokio::accounting::{ResourceClass, ResourceLedger, ResourceLimit};
use agentos_runtime_tokio::capability::CapabilityRegistry;
use agentos_vm_config as vm_config;
use openssl::rand::rand_bytes;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ROOT_BOOTSTRAP_DIRS: &[(&str, u32, u32, u32)] = &[
    ("/dev", 0o755, 0, 0),
    ("/proc", 0o755, 0, 0),
    ("/tmp", 0o1777, 0, 0),
    ("/bin", 0o755, 0, 0),
    ("/lib", 0o755, 0, 0),
    ("/sbin", 0o755, 0, 0),
    ("/boot", 0o755, 0, 0),
    ("/etc", 0o755, 0, 0),
    // agentOS retains `/root/node_modules` as a compatibility projection.
    // Permit traversal without allowing the default guest to list `/root`.
    ("/root", 0o711, 0, 0),
    ("/run", 0o755, 0, 0),
    ("/srv", 0o755, 0, 0),
    ("/sys", 0o555, 0, 0),
    ("/opt", 0o755, 0, 0),
    ("/mnt", 0o755, 0, 0),
    ("/media", 0o755, 0, 0),
    ("/home", 0o755, 0, 0),
    ("/home/agentos", 0o2755, 1000, 1000),
    ("/usr", 0o755, 0, 0),
    ("/usr/bin", 0o755, 0, 0),
    ("/usr/games", 0o755, 0, 0),
    ("/usr/include", 0o755, 0, 0),
    ("/usr/lib", 0o755, 0, 0),
    ("/usr/libexec", 0o755, 0, 0),
    ("/usr/man", 0o755, 0, 0),
    ("/usr/local", 0o755, 0, 0),
    ("/usr/local/bin", 0o755, 0, 0),
    ("/usr/sbin", 0o755, 0, 0),
    ("/usr/share", 0o755, 0, 0),
    ("/usr/share/man", 0o755, 0, 0),
    ("/var", 0o755, 0, 0),
    ("/var/cache", 0o755, 0, 0),
    ("/var/empty", 0o555, 0, 0),
    ("/var/lib", 0o755, 0, 0),
    ("/var/lock", 0o777, 0, 0),
    ("/var/log", 0o755, 0, 0),
    ("/var/run", 0o777, 0, 0),
    ("/var/spool", 0o755, 0, 0),
    ("/var/tmp", 0o1777, 0, 0),
    ("/etc/agentos", 0o755, 0, 0),
    // Non-Alpine default agent working directory (also present in the base
    // filesystem snapshot); scaffold it here so it exists even when the
    // default base layer is disabled. It is the default cwd and mount root,
    // kept separate from $HOME (/home/agentos).
    ("/workspace", 0o755, 1000, 1000),
];

fn create_vm_unix_socket_host_dir() -> Result<PathBuf, SidecarError> {
    for _ in 0..32 {
        let mut nonce = [0_u8; 16];
        rand_bytes(&mut nonce).map_err(|error| {
            SidecarError::Io(format!("failed to generate Unix socket namespace: {error}"))
        })?;
        let suffix = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("agentos-uds-{suffix}"));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&path) {
            Ok(()) => {
                if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o700)) {
                    let cleanup_error = fs::remove_dir(&path).err();
                    return Err(SidecarError::Io(format!(
                        "failed to set private Unix socket namespace {} to mode 0700: {error}{}",
                        path.display(),
                        cleanup_error
                            .map(|cleanup| format!("; cleanup failed: {cleanup}"))
                            .unwrap_or_default()
                    )));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(SidecarError::Io(format!(
                    "failed to create private Unix socket namespace {}: {error}",
                    path.display()
                )))
            }
        }
    }
    Err(SidecarError::Io(String::from(
        "failed to allocate a unique private Unix socket namespace after 32 attempts",
    )))
}

fn send_kernel_socket_readiness_event(
    target: KernelSocketReadinessTarget,
    readiness: SocketReadiness,
) {
    if !target.live.load(Ordering::Acquire) {
        return;
    }
    let flags = match (target.event, readiness.kind) {
        (KernelSocketReadinessEvent::Accept, SocketReadinessKind::Accept) => {
            agentos_runtime_tokio::readiness::ReadyFlags::ACCEPT
        }
        (KernelSocketReadinessEvent::Data, SocketReadinessKind::Data) => {
            agentos_runtime_tokio::readiness::ReadyFlags::READABLE
        }
        (KernelSocketReadinessEvent::Datagram, SocketReadinessKind::Data) => {
            agentos_runtime_tokio::readiness::ReadyFlags::DATAGRAM
        }
        _ => return,
    };
    if target.live.load(Ordering::Acquire) {
        if let Some(notify) = &target.notify {
            notify.notify_one();
        }
    }
    if target.live.load(Ordering::Acquire) {
        let Some(session) = &target.session else {
            return;
        };
        if let Err(error) =
            session.publish_readiness(target.capability_id, target.capability_generation, flags)
        {
            eprintln!(
                "ERR_AGENTOS_KERNEL_READINESS_WAKE: failed to publish capability={} generation={} target={}: {error}",
                target.capability_id, target.capability_generation, target.target_id
            );
        }
    }
}

pub(crate) const DEFAULT_GUEST_PATH_ENV: &str =
    "/usr/local/sbin:/usr/local/bin:/opt/agentos/bin:/usr/sbin:/usr/bin:/sbin:/bin";
#[cfg(test)]
const KERNEL_COMMAND_STUB: &[u8] = b"#!/bin/sh\n# kernel command stub\n";

fn projected_command_guest_path(command: &str) -> String {
    format!("{}/{command}", crate::package_projection::OPT_AGENTOS_BIN)
}

fn projected_commands_from_provided_commands(
    provided_commands: &BTreeMap<String, Vec<String>>,
    kernel_commands: &KernelCommandInventory,
) -> Vec<ProjectedCommand> {
    let mut commands = BTreeMap::new();
    for command in provided_commands.values().flatten() {
        if kernel_commands.names.contains(command) {
            continue;
        }
        commands
            .entry(command.clone())
            .or_insert_with(|| ProjectedCommand {
                name: command.clone(),
                guest_path: projected_command_guest_path(command),
            });
    }
    commands.into_values().collect()
}

fn execution_driver_commands(
    kernel_commands: &KernelCommandInventory,
    provided_commands: &BTreeMap<String, Vec<String>>,
    additional_commands: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut commands = BTreeSet::from([
        String::from(JAVASCRIPT_COMMAND),
        String::from(PYTHON_COMMAND),
        String::from("python3"),
        String::from(WASM_COMMAND),
    ]);
    commands.extend(kernel_commands.names.iter().cloned());
    commands.extend(provided_commands.values().flatten().cloned());
    commands.extend(additional_commands);
    commands.into_iter().collect()
}
// ---------------------------------------------------------------------------
// NativeSidecar VM lifecycle methods
// ---------------------------------------------------------------------------

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) fn allocate_vm_identity(&mut self) -> Result<(String, u64), SidecarError> {
        self.reap_reconciled_quarantined_vms();
        self.ensure_vm_generation_capacity()?;
        let next = self.next_vm_id.checked_add(1).ok_or_else(|| {
            SidecarError::host(
                "ERR_AGENTOS_VM_ID_EXHAUSTED",
                String::from("VM id counter overflowed"),
            )
        })?;
        let generation = self
            .runtime_context
            .as_ref()
            .ok_or_else(|| {
                SidecarError::host(
                    "ERR_AGENTOS_RUNTIME_UNAVAILABLE",
                    String::from("VM generation allocation requires RuntimeContext"),
                )
            })?
            .allocate_vm_generation()
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
        self.next_vm_id = next;
        Ok((format!("vm-{next}"), generation))
    }

    pub(crate) async fn create_vm(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: crate::protocol::CreateVmRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let __t = Instant::now();
        let (connection_id, session_id) = self.session_scope_for(&request.ownership)?;
        self.require_owned_session(&connection_id, &session_id)?;
        let create_config: vm_config::CreateVmConfig = serde_json::from_str(&payload.config)
            .map_err(|error| {
                SidecarError::InvalidState(format!("invalid create VM config JSON: {error}"))
            })?;
        create_config
            .validate(self.config.max_frame_bytes)
            .map_err(|error| {
                SidecarError::InvalidState(format!("invalid create VM config: {error}"))
            })?;
        let root_filesystem =
            root_filesystem_protocol_descriptor_from_config(&create_config.root_filesystem);
        let permissions_policy = create_config
            .permissions
            .clone()
            .unwrap_or_else(deny_all_policy);
        validate_permissions_policy(&permissions_policy)?;

        let (vm_id, vm_generation) = self.allocate_vm_identity()?;
        let runtime_scratch_root = create_vm_runtime_scratch_root(&vm_id)?;
        let (guest_cwd, host_cwd) =
            resolve_vm_cwds(create_config.cwd.as_ref(), &runtime_scratch_root)?;
        fs::create_dir_all(&host_cwd)
            .map_err(|error| SidecarError::Io(format!("failed to create VM cwd: {error}")))?;
        let limits = crate::limits::vm_limits_from_config(
            create_config.limits.as_ref(),
            self.config.max_frame_bytes,
        )?;
        let resource_limits = limits.resources.clone();
        let process_runtime_context = self.runtime_context.as_ref().cloned().ok_or_else(|| {
            SidecarError::host(
                "ERR_AGENTOS_RUNTIME_UNAVAILABLE",
                String::from("VM admission requires RuntimeContext"),
            )
        })?;
        let process_resources = Arc::clone(process_runtime_context.resources());
        let vm_resources = Arc::new(vm_resource_ledger(
            &vm_id,
            vm_generation,
            &limits,
            process_resources,
        )?);
        let vm_runtime_context =
            process_runtime_context.scoped_for_vm(Arc::clone(&vm_resources), vm_generation);
        let database = match create_config.database.as_ref() {
            Some(descriptor) => {
                let database = crate::vm_sqlite::resolve_vm_sqlite(
                    descriptor,
                    vm_runtime_context.clone(),
                    limits.sqlite.max_result_bytes,
                )
                .await
                .map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "failed to resolve VM SQLite database: {error}"
                    ))
                })?;
                crate::plugins::chunked_actor_sqlite::bootstrap_schema(database.as_ref())
                    .await
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "failed to migrate VM SQLite database: {error}"
                        ))
                    })?;
                for extension in self.extensions.values() {
                    extension
                        .bootstrap_vm_database(database.clone())
                        .await
                        .map_err(|error| {
                            SidecarError::InvalidState(format!(
                                "failed to migrate extension VM database schema: {error}"
                            ))
                        })?;
                }
                Some(database)
            }
            None => None,
        };
        let capabilities = CapabilityRegistry::new(vm_generation, Arc::clone(&vm_resources));
        let dns = vm_dns_config_from_config(create_config.dns.as_ref())?;
        let listen_policy = vm_listen_policy_from_config(create_config.listen.as_ref())?;
        let create_loopback_exempt_ports: BTreeSet<u16> = create_config
            .loopback_exempt_ports
            .iter()
            .copied()
            .collect();
        self.bridge
            .set_vm_permissions(&vm_id, &permissions_policy)?;
        let permissions = bridge_permissions(self.bridge.clone(), &vm_id);
        let mut guest_env = filter_env(&vm_id, &create_config.env, &permissions);
        // Sidecar-owned bootstrap work still needs to install command stubs and
        // the root filesystem before the guest-visible policy takes effect.
        self.bridge
            .set_vm_permissions(&vm_id, &allow_all_policy())?;
        let native_root = native_root_plugin_from_config(create_config.native_root.as_ref())?;
        let loaded_snapshot = if native_root.is_some() {
            None
        } else {
            self.bridge.with_mut(|bridge| {
                bridge.load_filesystem_state(LoadFilesystemStateRequest {
                    vm_id: vm_id.clone(),
                })
            })?
        };
        let mut config = KernelVmConfig::new(vm_id.clone());
        config.vm_generation = vm_generation;
        config.cwd = guest_cwd.clone();
        config.env = guest_env.clone();
        if let Some(user) = create_config.user.as_ref() {
            config.user = agentos_kernel::user::UserConfig {
                uid: user.uid,
                gid: user.gid,
                euid: user.euid,
                egid: user.egid,
                username: user.username.clone(),
                homedir: user.homedir.clone(),
                shell: user.shell.clone(),
                gecos: user.gecos.clone(),
                group_name: user.group_name.clone(),
                supplementary_gids: user.supplementary_gids.clone().unwrap_or_default(),
                accounts: user
                    .accounts
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|account| agentos_kernel::user::UserAccount {
                        uid: account.uid,
                        gid: account.gid,
                        username: account.username.clone(),
                        homedir: account.homedir.clone(),
                        shell: account.shell.clone(),
                        gecos: account.gecos.clone().unwrap_or_default(),
                        supplementary_gids: account.supplementary_gids.clone(),
                    })
                    .collect(),
                groups: user
                    .groups
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|group| agentos_kernel::user::GroupRecord {
                        gid: group.gid,
                        name: group.name.clone(),
                        members: group.members.clone(),
                    })
                    .collect(),
            };
        }
        config.permissions = permissions;
        config.dns = agentos_kernel::dns::DnsConfig {
            name_servers: dns.name_servers.clone(),
            overrides: dns.overrides.clone(),
        };
        if self.runtime_context.is_none() {
            return Err(SidecarError::InvalidState(String::from(
                "VM creation requires the process RuntimeContext",
            )));
        }
        config.dns_resolver = Arc::clone(&self.dns_resolver);
        config.loopback_exempt_ports = create_loopback_exempt_ports.clone();
        let root_mount_table = if let Some(native_root) = native_root.as_ref() {
            build_native_root_mount_table(
                &self.mount_plugins,
                native_root,
                &root_filesystem,
                MountPluginContext {
                    bridge: self.bridge.clone(),
                    runtime_context: vm_runtime_context.clone(),
                    connection_id: connection_id.clone(),
                    session_id: session_id.clone(),
                    vm_id: vm_id.clone(),
                    sidecar_requests: self.sidecar_requests.clone(),
                    database: database.clone(),
                    max_pread_bytes: resource_limits.max_pread_bytes,
                },
            )?
        } else {
            agentos_native_sidecar_core::build_root_mount_table_with_loaded_snapshot(
                &create_config.root_filesystem,
                loaded_snapshot.as_ref(),
                &resource_limits,
            )
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?
        };
        config.resources = resource_limits;
        let mut kernel = KernelVm::new(root_mount_table, config);
        kernel
            .set_socket_resource_ledger(Arc::clone(&vm_resources))
            .map_err(kernel_error)?;
        let kernel_socket_readiness: KernelSocketReadinessRegistry = Arc::new(
            crate::state::KernelSocketReadinessRegistryState::new(limits.reactor.max_capabilities),
        );
        let readiness_targets = Arc::clone(&kernel_socket_readiness);
        kernel.set_socket_readiness_sink(Some(move |readiness: SocketReadiness| {
            for target in readiness_targets.targets(readiness.socket_id) {
                send_kernel_socket_readiness_event(target, readiness);
            }
        }));
        let kernel_commands = discover_kernel_commands(&mut kernel);
        refresh_guest_command_path_env(&mut guest_env, &kernel_commands.search_roots);
        let execution_commands = execution_driver_commands(
            &kernel_commands,
            &BTreeMap::new(),
            create_config.bootstrap_commands.iter().flatten().cloned(),
        );
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                execution_commands,
            ))
            .map_err(kernel_error)?;
        self.bridge
            .set_vm_permissions(&vm_id, &permissions_policy)?;

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
        let unix_socket_host_dir = create_vm_unix_socket_host_dir()?;
        let pending_stdin_bytes_budget = VmPendingByteBudget::new(
            limits.process.pending_stdin_bytes,
            agentos_bridge::queue_tracker::TrackedLimit::PendingKernelStdinBytes,
        );
        let pending_event_bytes_budget = VmPendingByteBudget::new(
            limits.process.pending_event_bytes,
            agentos_bridge::queue_tracker::TrackedLimit::PendingExecutionEventBytes,
        );
        let pending_child_sync_count_budget = VmPendingByteBudget::new(
            limits.process.max_pending_child_sync_count,
            agentos_bridge::queue_tracker::TrackedLimit::PendingChildProcessSyncCount,
        );
        let pending_child_sync_bytes_budget = VmPendingByteBudget::new(
            limits.process.max_pending_child_sync_bytes,
            agentos_bridge::queue_tracker::TrackedLimit::PendingChildProcessSyncBytes,
        );
        self.vms.insert(
            vm_id.clone(),
            VmState {
                connection_id: connection_id.clone(),
                session_id: session_id.clone(),
                generation: vm_generation,
                limits,
                pending_stdin_bytes_budget,
                pending_event_bytes_budget,
                pending_child_sync_count_budget,
                pending_child_sync_bytes_budget,
                resources: vm_resources,
                runtime_context: vm_runtime_context,
                database,
                capabilities,
                dns,
                listen_policy,
                create_loopback_exempt_ports,
                guest_env,
                standalone_wasm_backend: match create_config.wasm_backend.unwrap_or_default() {
                    vm_config::StandaloneWasmBackend::V8 => {
                        agentos_execution::StandaloneWasmBackend::V8
                    }
                    vm_config::StandaloneWasmBackend::Wasmtime => {
                        agentos_execution::StandaloneWasmBackend::Wasmtime
                    }
                    vm_config::StandaloneWasmBackend::WasmtimeThreads => {
                        agentos_execution::StandaloneWasmBackend::WasmtimeThreads
                    }
                },
                requested_runtime: payload.runtime,
                root_filesystem_mode: protocol_root_filesystem_mode(root_filesystem.mode),
                guest_cwd,
                runtime_scratch_root,
                host_cwd,
                kernel,
                kernel_socket_readiness,
                managed_host_net_descriptions: Arc::new(Mutex::new(BTreeMap::new())),
                host_net_transfer_descriptions: Arc::new(Mutex::new(BTreeMap::new())),
                loaded_snapshot,
                configuration: VmConfiguration {
                    permissions: permissions_policy,
                    js_runtime: create_config.js_runtime.clone(),
                    ..VmConfiguration::default()
                },
                layers: VmLayerStore::default(),
                provided_commands: BTreeMap::new(),
                command_permissions: BTreeMap::new(),
                bindings: BTreeMap::new(),
                active_processes: BTreeMap::new(),
                vm_fetch_streams: BTreeMap::new(),
                next_vm_fetch_stream_id: 0,
                executions: BTreeMap::new(),
                execution_processes: BTreeMap::new(),
                next_public_execution_id: 0,
                execution_retention_wake_deadline_ms: None,
                execution_retention_wake_task: None,
                package_mutation_execution_id: None,
                typescript_compiler_staged: false,
                exited_process_snapshots: VecDeque::new(),
                detached_child_processes: BTreeSet::new(),
                attached_child_event_cursor: 0,
                detached_child_event_cursor: 0,
                packages_staging_root: None,
                projected_agent_launch: BTreeMap::new(),
                unix_address_registry: Arc::new(Mutex::new(BTreeMap::new())),
                unix_socket_host_dir,
            },
        );
        self.observe_active_vm_generations();

        let events = vec![
            self.vm_lifecycle_event(
                &connection_id,
                &session_id,
                &vm_id,
                VmLifecycleState::Creating,
            ),
            self.vm_lifecycle_event(&connection_id, &session_id, &vm_id, VmLifecycleState::Ready),
        ];

        tracing::info!(target: "agentos_native_sidecar::perf", phase = "create_vm", elapsed_ms = __t.elapsed().as_millis() as u64, "vm phase");
        Ok(DispatchResult {
            response: vm_created_response(request, vm_id),
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
            response: vm_disposed_response(request, vm_id),
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
            response: root_filesystem_bootstrapped_response(request, entries.len() as u32),
            events: Vec::new(),
        })
    }

    pub(crate) async fn configure_vm(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: ConfigureVmRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let __t = Instant::now();
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let mount_plugins = &self.mount_plugins;
        let bridge = self.bridge.clone();
        let snapshot_runtime_context = self.runtime_context.as_ref().cloned().ok_or_else(|| {
            SidecarError::host(
                "ERR_AGENTOS_RUNTIME_UNAVAILABLE",
                String::from("snapshot pre-warm requires RuntimeContext"),
            )
        })?;
        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let max_pread_bytes = vm.kernel.resource_limits().max_pread_bytes;
        let original_permissions = vm.configuration.permissions.clone();
        let configured_permissions = payload
            .permissions
            .clone()
            .map(crate::wire::permissions_policy_config_from_wire)
            .unwrap_or_else(|| original_permissions.clone());
        validate_permissions_policy(&configured_permissions)?;
        bridge.set_vm_permissions(&vm_id, &allow_all_policy())?;
        let mut effective_mounts = payload.mounts.clone();
        append_module_access_mount(&mut effective_mounts, payload.module_access_cwd.as_ref())?;
        let package_descriptors = package_descriptors_from_wire(&payload.packages)?;
        let mut provided_commands: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for descriptor in &package_descriptors {
            provided_commands.insert(
                descriptor.name.clone(),
                descriptor
                    .commands
                    .iter()
                    .map(|target| target.command.clone())
                    .collect(),
            );
        }
        let snapshot_userland_code = resolve_agent_snapshot_bundle(&package_descriptors)?;
        let package_mounts =
            build_packages_projection(&vm_id, &package_descriptors, &payload.packages_mount_at)?;
        effective_mounts.extend(package_mounts);
        apply_package_provides_env(&mut vm.guest_env, &package_descriptors);
        append_package_provides_mounts(&mut effective_mounts, &package_descriptors)?;
        let reconfigure_result = reconcile_mounts(
            mount_plugins,
            vm,
            &effective_mounts,
            MountPluginContext {
                bridge: bridge.clone(),
                runtime_context: vm.runtime_context.clone(),
                connection_id: connection_id.clone(),
                session_id: session_id.clone(),
                vm_id: vm_id.clone(),
                sidecar_requests: self.sidecar_requests.clone(),
                database: vm.database.clone(),
                max_pread_bytes,
            },
        )
        .and_then(|()| {
            let kernel_commands = discover_kernel_commands(&mut vm.kernel);
            let execution_commands = execution_driver_commands(
                &kernel_commands,
                &provided_commands,
                payload
                    .bootstrap_commands
                    .iter()
                    .chain(payload.binding_shim_commands.iter())
                    .cloned(),
            );
            vm.kernel
                .replace_driver(CommandDriver::new(
                    EXECUTION_DRIVER_NAME,
                    execution_commands,
                ))
                .map_err(kernel_error)?;
            // Package manifests are sidecar-owned, so their command names are
            // only known during configure. Seal the root after those trusted
            // `/bin` stubs have been projected, never during create_vm.
            vm.kernel
                .finish_root_filesystem_bootstrap()
                .map_err(kernel_error)?;
            refresh_guest_command_path_env(&mut vm.guest_env, &kernel_commands.search_roots);
            vm.command_permissions = payload.command_permissions.clone().into_iter().collect();
            let mut loopback_exempt_ports = vm.create_loopback_exempt_ports.clone();
            loopback_exempt_ports.extend(payload.loopback_exempt_ports.iter().copied());
            vm.kernel.set_loopback_exempt_ports(loopback_exempt_ports);
            vm.configuration = VmConfiguration {
                mounts: effective_mounts.clone(),
                software: payload.software.clone(),
                permissions: configured_permissions.clone(),
                module_access_cwd: payload.module_access_cwd.clone(),
                instructions: payload.instructions.clone(),
                projected_modules: payload.projected_modules.clone(),
                command_permissions: payload.command_permissions.clone().into_iter().collect(),
                provided_commands: provided_commands.clone(),
                // jsRuntime is create-time only; preserve what create_vm stored.
                js_runtime: vm.configuration.js_runtime.clone(),
                snapshot_userland_code: snapshot_userland_code.clone(),
                loopback_exempt_ports: payload.loopback_exempt_ports.clone(),
            };
            vm.provided_commands = provided_commands;
            Ok(())
        });
        match reconfigure_result {
            Ok(()) => {
                bridge.set_vm_permissions(&vm_id, &configured_permissions)?;
            }
            Err(error) => {
                match bridge.restore_vm_permissions_fail_closed(
                    &vm_id,
                    &original_permissions,
                    "configure_vm rollback",
                    &error,
                ) {
                    Ok(()) => return Err(error),
                    Err(rollback_error) => {
                        self.vms
                            .get_mut(&vm_id)
                            .expect("owned VM should exist")
                            .configuration
                            .permissions = deny_all_policy();
                        return Err(rollback_error);
                    }
                }
            }
        }

        let applied_mounts = effective_mounts.len() as u32;
        let configured_software = payload.software.len() as u32;
        let kernel_commands = discover_kernel_commands(&mut vm.kernel);
        let projected_commands =
            projected_commands_from_provided_commands(&vm.provided_commands, &kernel_commands);
        let agents = projected_agents_from_descriptors(&package_descriptors);
        vm.projected_agent_launch = projected_agent_launch_from_descriptors(&package_descriptors);
        let _ = vm;
        // Pre-warm the agent-SDK snapshot when a configured package opts in with
        // `agent.snapshot`. The sidecar reads the bundle from the host package dir
        // it already projects, so the first session is warm without shipping the
        // source over the client wire.
        if let Some(userland) = snapshot_userland_code {
            let requested_bytes = userland.len();
            let runtime_for_job = snapshot_runtime_context.clone();
            match snapshot_runtime_context
                .blocking()
                .run(requested_bytes, move || {
                    agentos_execution::v8_host::pre_warm_agent_snapshot(&runtime_for_job, &userland)
                })
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("agent snapshot pre-warm failed: {error}"),
                Err(error) => {
                    eprintln!("agent snapshot pre-warm admission or execution failed: {error}")
                }
            }
        }

        tracing::info!(target: "agentos_native_sidecar::perf", phase = "configure_vm", elapsed_ms = __t.elapsed().as_millis() as u64, applied_mounts = applied_mounts as u64, "vm phase");
        Ok(DispatchResult {
            response: vm_configured_response(
                request,
                applied_mounts,
                configured_software,
                projected_commands,
                agents,
            ),
            events: Vec::new(),
        })
    }

    /// Runtime dynamic `linkSoftware`: add one package's tar/current/bin leaf
    /// mounts to the live VM so commands appear under `/opt/agentos/bin`
    /// immediately, with no reboot. Returns the linked command names.
    pub(crate) async fn link_package(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: LinkPackageRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let descriptor =
            crate::package_projection::read_package_manifest_from_path(&payload.package.path)?;
        let new_mounts = build_packages_projection(
            &vm_id,
            std::slice::from_ref(&descriptor),
            crate::package_projection::OPT_AGENTOS_ROOT,
        )?;
        if new_mounts.iter().all(|mount| {
            vm.configuration
                .mounts
                .iter()
                .any(|existing| existing.guest_path == mount.guest_path)
        }) {
            let projected_commands = descriptor
                .commands
                .iter()
                .map(|target| ProjectedCommand {
                    name: target.command.clone(),
                    guest_path: projected_command_guest_path(&target.command),
                })
                .collect();
            let agents = projected_agents_from_descriptors(std::slice::from_ref(&descriptor));
            return Ok(DispatchResult {
                response: package_linked_response(request, projected_commands, agents),
                events: Vec::new(),
            });
        }
        for mount in &new_mounts {
            if vm
                .configuration
                .mounts
                .iter()
                .any(|existing| existing.guest_path == mount.guest_path)
            {
                if let Some(command) = mount
                    .guest_path
                    .strip_prefix(crate::package_projection::OPT_AGENTOS_BIN)
                    .and_then(|path| path.strip_prefix('/'))
                    .filter(|path| !path.is_empty())
                {
                    return Err(SidecarError::InvalidState(format!(
                        "command {command:?} is already provided by another package"
                    )));
                }
                return Err(SidecarError::InvalidState(format!(
                    "agentos package mount already exists at {}",
                    mount.guest_path
                )));
            }
        }
        let mount_context = MountPluginContext {
            bridge: self.bridge.clone(),
            runtime_context: vm.runtime_context.clone(),
            connection_id: connection_id.clone(),
            session_id: session_id.clone(),
            vm_id: vm_id.clone(),
            sidecar_requests: self.sidecar_requests.clone(),
            database: vm.database.clone(),
            max_pread_bytes: vm.kernel.resource_limits().max_pread_bytes,
        };
        mount_leaf_descriptors(&self.mount_plugins, vm, &new_mounts, mount_context)?;
        vm.configuration.mounts.extend(new_mounts);

        let commands = descriptor
            .commands
            .iter()
            .map(|target| target.command.clone())
            .collect::<Vec<_>>();
        vm.provided_commands
            .insert(descriptor.name.clone(), commands.clone());
        vm.configuration
            .provided_commands
            .insert(descriptor.name.clone(), commands.clone());
        let retained_execution_commands = vm
            .kernel
            .commands()
            .into_iter()
            .filter_map(|(command, driver)| (driver == EXECUTION_DRIVER_NAME).then_some(command))
            .collect::<Vec<_>>();
        let kernel_commands = discover_kernel_commands(&mut vm.kernel);
        let execution_commands = execution_driver_commands(
            &kernel_commands,
            &vm.provided_commands,
            retained_execution_commands,
        );
        vm.kernel
            .replace_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                execution_commands,
            ))
            .map_err(kernel_error)?;
        refresh_guest_command_path_env(&mut vm.guest_env, &kernel_commands.search_roots);
        let projected_commands = commands
            .iter()
            .map(|command| ProjectedCommand {
                name: command.clone(),
                guest_path: projected_command_guest_path(command),
            })
            .collect();
        let agents = projected_agents_from_descriptors(std::slice::from_ref(&descriptor));
        if let Some(vm) = self.vms.get_mut(&vm_id) {
            vm.projected_agent_launch
                .extend(projected_agent_launch_from_descriptors(
                    std::slice::from_ref(&descriptor),
                ));
        }

        Ok(DispatchResult {
            response: package_linked_response(request, projected_commands, agents),
            events: Vec::new(),
        })
    }

    pub(crate) async fn provided_commands(
        &mut self,
        request: &crate::protocol::RequestFrame,
        _payload: ProvidedCommandsRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self
            .vms
            .get(&vm_id)
            .ok_or_else(|| SidecarError::host("ESTALE", "VM disappeared during command lookup"))?;
        let packages = vm
            .provided_commands
            .iter()
            .map(|(package_name, commands)| PackageCommands {
                package_name: package_name.clone(),
                commands: commands.clone(),
            })
            .collect();

        Ok(DispatchResult {
            response: provided_commands_response(request, packages),
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
        let layer_id = vm
            .layers
            .create_writable_layer()
            .map_err(sidecar_core_error)?;

        Ok(DispatchResult {
            response: layer_created_response(request, layer_id),
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
        let layer_id = vm
            .layers
            .seal_layer(&payload.layer_id)
            .map_err(sidecar_core_error)?;

        Ok(DispatchResult {
            response: layer_sealed_response(request, layer_id),
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
            .import_snapshot(root_snapshot_from_entries(&payload.entries)?)
            .map_err(sidecar_core_error)?;

        Ok(DispatchResult {
            response: snapshot_imported_response(request, layer_id),
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
        let snapshot = vm
            .layers
            .export_snapshot(&payload.layer_id)
            .map_err(sidecar_core_error)?;

        Ok(DispatchResult {
            response: snapshot_exported_response(
                request,
                payload.layer_id,
                root_snapshot_entries(&snapshot),
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
        let layer_id = vm
            .layers
            .create_overlay_layer(
                protocol_root_filesystem_mode(payload.mode),
                payload.upper_layer_id,
                payload.lower_layer_ids,
            )
            .map_err(sidecar_core_error)?;

        Ok(DispatchResult {
            response: overlay_created_response(request, layer_id),
            events: Vec::new(),
        })
    }

    pub(crate) async fn snapshot_root_filesystem(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: SnapshotRootFilesystemRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let snapshot = vm
            .kernel
            .snapshot_root_filesystem_bounded(payload.max_bytes)
            .map_err(kernel_error)?;

        Ok(DispatchResult {
            response: root_filesystem_snapshot_response(
                request,
                snapshot.entries.iter().map(root_snapshot_entry).collect(),
            ),
            events: Vec::new(),
        })
    }

    pub(crate) async fn list_mounts(
        &mut self,
        request: &crate::protocol::RequestFrame,
        _payload: ListMountsRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get(&vm_id).expect("owned VM should exist");
        let mounts = vm
            .kernel
            .mounted_filesystems()
            .into_iter()
            .map(|mount| MountInfo {
                path: mount.path,
                kind: mount.plugin_id,
                read_only: mount.read_only,
            })
            .collect();

        Ok(DispatchResult {
            response: mounts_listed_response(request, mounts),
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
        // Process termination needs the VM live in `self.vms` (it looks up and
        // signals the VM's active processes). Capture its result but keep tearing
        // down: a process that refuses to die must not strand the VM's tracking
        // entries for the process lifetime.
        let terminate_result = self.terminate_vm_processes(vm_id, &mut events).await;
        if let Some(vm) = self.vms.get_mut(vm_id) {
            if let Some(task) = vm.execution_retention_wake_task.take() {
                task.abort();
            }
            vm.execution_retention_wake_deadline_ms = None;
            for execution in vm.executions.values_mut() {
                if let Some(task) = execution.deadline_task.take() {
                    task.abort();
                }
            }
        }

        // Process teardown can require the VM blocking executor to flush
        // host-materialized state into a persistent filesystem plugin (for
        // example, a WAL-backed node:sqlite database copied into the actor
        // SQLite VFS). Closing runtime admission before termination makes that
        // mandatory final write fail with ERR_AGENTOS_BLOCKING_EXECUTOR_SHUTDOWN.
        // Dispose requests are serialized by the sidecar, so retire admission
        // after the process drain but before detaching the VM from its registry.
        let vm_before_disposal = self
            .vms
            .get(vm_id)
            .expect("owned VM should exist before disposal");
        let capability_admission_error = close_vm_admission(
            &vm_before_disposal.runtime_context,
            &vm_before_disposal.capabilities,
        )
        .err();
        if let Some(error) = capability_admission_error.as_ref() {
            eprintln!("ERR_AGENTOS_VM_CAPABILITY_ADMISSION_CLOSE: vm_id={vm_id} error={error}");
        }
        let fairness_retirement_result = retire_vm_fairness(
            &vm_before_disposal.runtime_context,
            vm_before_disposal.generation,
        );
        if let Err(error) = fairness_retirement_result.as_ref() {
            eprintln!("ERR_AGENTOS_VM_FAIRNESS_RETIRE: vm_id={vm_id} error={error}");
        }

        // Detach the VM from `self.vms` BEFORE the remaining fallible teardown so
        // no `?` below can leave the registry entry (or any per-VM map) behind.
        let mut vm = self
            .vms
            .remove(vm_id)
            .expect("owned VM should exist before disposal");

        // `continue_on_error = true` => `shutdown_configured_mounts` never returns
        // `Err` on the dispose path (it logs and presses on), so its result is
        // intentionally discarded rather than `?`-ed.
        let mount_context = MountPluginContext {
            bridge: self.bridge.clone(),
            runtime_context: vm.runtime_context.clone(),
            connection_id: connection_id.to_owned(),
            session_id: session_id.to_owned(),
            vm_id: vm_id.to_owned(),
            sidecar_requests: self.sidecar_requests.clone(),
            database: vm.database.clone(),
            max_pread_bytes: vm.kernel.resource_limits().max_pread_bytes,
        };
        if let Err(error) = shutdown_configured_mounts(&mut vm, &mount_context, "dispose_vm", true)
        {
            eprintln!(
                "ERR_AGENTOS_MOUNT_TEARDOWN: mount shutdown returned an unexpected error for VM {vm_id}: {error}"
            );
        }

        // Snapshot/flush/kernel-dispose/permission-reset can each fail; run them
        // in a helper whose result is captured so cleanup below is unconditional.
        let teardown_result = self.finish_vm_teardown(vm_id, &mut vm).await;

        // Reclaim EVERY per-VM tracking entry on EVERY exit path — even when a
        // teardown step above errored. Pre-fix these ran only after the fallible
        // steps' `?`, so any failure stranded the engine/extension maps (H1) and
        // the output-buffer map was never reclaimed at all (M6).
        self.reclaim_vm_tracking(session_id, vm_id);
        if let Err(error) = fs::remove_dir_all(&vm.runtime_scratch_root) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "ERR_AGENTOS_VM_SCRATCH_CLEANUP: failed to remove {}: {error}",
                    vm.runtime_scratch_root.display()
                );
            }
        }
        if let Some(staging_root) = vm.packages_staging_root.take() {
            if let Err(error) = fs::remove_dir_all(&staging_root) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "ERR_AGENTOS_PACKAGE_STAGING_CLEANUP: failed to remove {}: {error}",
                        staging_root.display()
                    );
                }
            }
        }
        if let Err(error) = fs::remove_dir_all(&vm.unix_socket_host_dir) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %vm.unix_socket_host_dir.display(),
                    %error,
                    "failed to remove private Unix socket namespace during VM teardown"
                );
            }
        }

        let shutdown_deadline = Duration::from_millis(vm.limits.reactor.shutdown_deadline_ms);
        let (reconciliation, deadline_expired) = wait_for_vm_reconciliation(
            vm.resources.as_ref(),
            &vm.runtime_context,
            &vm.capabilities,
            shutdown_deadline,
        )
        .await;
        let quarantine_reason = vm_quarantine_reason(
            capability_admission_error.is_some(),
            fairness_retirement_result.is_err(),
            reconciliation,
            deadline_expired,
        );

        if let Some(reason) = quarantine_reason {
            let diagnostic = match reason {
                VmQuarantineReason::TeardownDeadline => format!(
                    "ERR_AGENTOS_VM_TEARDOWN_DEADLINE: vm_id={vm_id} generation={} active_tasks={} outstanding_capabilities={} ledger_zero={} deadline_ms={}; raise limits.reactor.shutdownDeadlineMs",
                    vm.generation,
                    reconciliation.active_tasks,
                    reconciliation.outstanding_capabilities,
                    reconciliation.ledger_zero,
                    vm.limits.reactor.shutdown_deadline_ms
                ),
                VmQuarantineReason::ResourceIntegrity => format!(
                    "ERR_AGENTOS_VM_RESOURCE_INTEGRITY: vm_id={vm_id} generation={} accounting integrity failed; generation cannot be reaped",
                    vm.generation
                ),
                VmQuarantineReason::CapabilityRegistryIntegrity => format!(
                    "ERR_AGENTOS_VM_CAPABILITY_INTEGRITY: vm_id={vm_id} generation={} capability admission could not be closed; generation cannot be reaped; error={}",
                    vm.generation,
                    capability_admission_error.as_deref().unwrap_or("unknown")
                ),
                VmQuarantineReason::FairnessIntegrity => format!(
                    "ERR_AGENTOS_VM_FAIRNESS_INTEGRITY: vm_id={vm_id} generation={} fairness membership could not be retired; generation cannot be reaped; error={}",
                    vm.generation,
                    fairness_retirement_result
                        .as_ref()
                        .expect_err("fairness quarantine requires a retirement error")
                ),
            };
            eprintln!("{diagnostic}");
            if let Err(error) = terminate_result.as_ref() {
                eprintln!(
                    "ERR_AGENTOS_VM_TEARDOWN_CLEANUP: vm_id={vm_id} phase=processes error={error}"
                );
            }
            if let Err(error) = teardown_result.as_ref() {
                eprintln!(
                    "ERR_AGENTOS_VM_TEARDOWN_CLEANUP: vm_id={vm_id} phase=kernel_or_bridge error={error}"
                );
            }
            self.retain_quarantined_vm(QuarantinedVmGeneration {
                connection_id: connection_id.to_owned(),
                session_id: session_id.to_owned(),
                vm_id: vm_id.to_owned(),
                generation: vm.generation,
                resources: Arc::clone(&vm.resources),
                runtime_context: vm.runtime_context.clone(),
                capabilities: vm.capabilities.clone(),
                reason,
            })?;
            return Err(SidecarError::Execution(diagnostic));
        }

        self.observe_active_vm_generations();
        // Surface the first failure only AFTER cleanup has completed.
        fairness_retirement_result?;
        terminate_result?;
        teardown_result?;

        events.push(self.vm_lifecycle_event(
            connection_id,
            session_id,
            vm_id,
            VmLifecycleState::Disposed,
        ));
        Ok(events)
    }

    /// Run every fallible second-half cleanup step, retaining the first error
    /// while logging later failures. Teardown must reach kernel disposal and
    /// permission reset even when snapshot or bridge work fails.
    async fn finish_vm_teardown(
        &mut self,
        vm_id: &str,
        vm: &mut VmState,
    ) -> Result<(), SidecarError> {
        let mut first_error = None;
        let snapshot = if vm.kernel.root_filesystem_mut().is_some() {
            match vm
                .kernel
                .snapshot_root_filesystem()
                .map_err(kernel_error)
                .and_then(|snapshot| encode_root_snapshot(&snapshot).map_err(root_filesystem_error))
            {
                Ok(bytes) => Some(FilesystemSnapshot {
                    format: String::from(ROOT_FILESYSTEM_SNAPSHOT_FORMAT),
                    bytes,
                }),
                Err(error) => {
                    record_vm_teardown_error(vm_id, "snapshot", error, &mut first_error);
                    None
                }
            }
        } else {
            None
        };

        if let Err(error) = self
            .bridge
            .emit_lifecycle(vm_id, LifecycleState::Terminated)
        {
            record_vm_teardown_error(vm_id, "lifecycle", error, &mut first_error);
        }
        if let Err(error) = vm.kernel.dispose().map_err(kernel_error) {
            record_vm_teardown_error(vm_id, "kernel", error, &mut first_error);
        }
        if let Some(snapshot) = snapshot {
            if let Err(error) = self.bridge.with_mut(|bridge| {
                bridge.flush_filesystem_state(FlushFilesystemStateRequest {
                    vm_id: vm_id.to_owned(),
                    snapshot,
                })
            }) {
                record_vm_teardown_error(vm_id, "filesystem_flush", error, &mut first_error);
            }
        }
        if let Err(error) = self.bridge.clear_vm_permissions(vm_id) {
            record_vm_teardown_error(vm_id, "permission_reset", error, &mut first_error);
        }
        first_error.map_or(Ok(()), Err)
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
            // A shared V8 execution can take longer than the bounded event-pump
            // grace to report its exit after SIGKILL. VM teardown must still
            // finalize process-owned bridge state before snapshotting the root
            // filesystem; dropping ActiveProcess after the snapshot loses
            // committed host-materialized SQLite/WAL pages.
            let vm = self
                .vms
                .get_mut(vm_id)
                .expect("active VM should exist during process teardown");
            let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
            let unix_address_registry = Arc::clone(&vm.unix_address_registry);
            let remaining = std::mem::take(&mut vm.active_processes);
            eprintln!(
                "ERR_AGENTOS_VM_FORCED_PROCESS_FINALIZE: vm_id={vm_id} process_count={}",
                remaining.len()
            );
            for (process_id, mut process) in remaining {
                terminate_child_process_tree(
                    &mut vm.kernel,
                    &mut process,
                    &kernel_readiness,
                    &unix_address_registry,
                );
                process.kernel_handle.finish(137);
                if let Err(error) = vm.kernel.wait_and_reap(process.kernel_pid) {
                    eprintln!(
                        "ERR_AGENTOS_VM_FORCED_PROCESS_REAP: vm_id={vm_id} process_id={process_id} pid={} error={error}",
                        process.kernel_pid
                    );
                }
            }
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
            if let Some(event) = self.poll_event(&ownership, remaining).await? {
                events.push(event);
            }
        }

        Ok(())
    }
}

fn record_vm_teardown_error(
    vm_id: &str,
    phase: &str,
    error: SidecarError,
    first_error: &mut Option<SidecarError>,
) {
    eprintln!("ERR_AGENTOS_VM_TEARDOWN_CLEANUP: vm_id={vm_id} phase={phase} error={error}");
    if first_error.is_none() {
        *first_error = Some(error);
    }
}

fn vm_reconciliation_snapshot(
    resources: &ResourceLedger,
    runtime_context: &agentos_runtime_tokio::RuntimeContext,
    capabilities: &CapabilityRegistry,
) -> VmReconciliationSnapshot {
    VmReconciliationSnapshot {
        active_tasks: runtime_context.tasks().active_scoped(),
        outstanding_capabilities: capabilities.outstanding_len(),
        ledger_zero: resources.is_zero(),
        integrity_ok: resources.integrity_ok(),
    }
}

fn close_vm_admission(
    runtime_context: &agentos_runtime_tokio::RuntimeContext,
    capabilities: &CapabilityRegistry,
) -> Result<(), String> {
    let capability_result = capabilities
        .close_admission()
        .map_err(|error| error.to_string());
    runtime_context.close_admission();
    capability_result
}

fn retire_vm_fairness(
    runtime_context: &agentos_runtime_tokio::RuntimeContext,
    vm_generation: u64,
) -> Result<(), SidecarError> {
    runtime_context
        .fairness()
        .retire_vm(vm_generation)
        .map(|_| ())
        .map_err(|error| {
            SidecarError::host(
                "ERR_AGENTOS_FAIRNESS_RETIRE_VM",
                format!("generation={vm_generation}: {error}"),
            )
        })
}

fn vm_quarantine_reason(
    capability_registry_integrity_failed: bool,
    fairness_integrity_failed: bool,
    reconciliation: VmReconciliationSnapshot,
    deadline_expired: bool,
) -> Option<VmQuarantineReason> {
    if capability_registry_integrity_failed {
        Some(VmQuarantineReason::CapabilityRegistryIntegrity)
    } else if fairness_integrity_failed {
        Some(VmQuarantineReason::FairnessIntegrity)
    } else if !reconciliation.integrity_ok {
        Some(VmQuarantineReason::ResourceIntegrity)
    } else if deadline_expired
        || reconciliation.active_tasks != 0
        || reconciliation.outstanding_capabilities != 0
        || !reconciliation.ledger_zero
    {
        Some(VmQuarantineReason::TeardownDeadline)
    } else {
        None
    }
}

async fn wait_for_vm_reconciliation(
    resources: &ResourceLedger,
    runtime_context: &agentos_runtime_tokio::RuntimeContext,
    capabilities: &CapabilityRegistry,
    deadline: Duration,
) -> (VmReconciliationSnapshot, bool) {
    let initial = vm_reconciliation_snapshot(resources, runtime_context, capabilities);
    if initial.active_tasks == 0
        && initial.outstanding_capabilities == 0
        && initial.ledger_zero
        && initial.integrity_ok
    {
        return (initial, false);
    }

    let wait_for_ledger = async {
        loop {
            if resources.is_zero() || !resources.integrity_ok() {
                return;
            }
            resources.capacity_changed().await;
        }
    };
    let barrier = async {
        tokio::join!(
            runtime_context.tasks().wait_empty(),
            capabilities.wait_empty(),
            wait_for_ledger
        );
    };
    let deadline_expired = tokio::time::timeout(deadline, barrier).await.is_err();
    (
        vm_reconciliation_snapshot(resources, runtime_context, capabilities),
        deadline_expired,
    )
}

fn vm_resource_ledger(
    vm_id: &str,
    generation: u64,
    limits: &crate::limits::VmLimits,
    process: Arc<ResourceLedger>,
) -> Result<ResourceLedger, SidecarError> {
    let socket_limit = limits.resources.max_sockets.ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "limits.resources.maxSockets must be bounded for sidecar VMs",
        ))
    })?;
    let connection_limit = limits.resources.max_connections.ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "limits.resources.maxConnections must be bounded for sidecar VMs",
        ))
    })?;
    let buffered_byte_limit = limits.resources.max_socket_buffered_bytes.ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "limits.resources.maxSocketBufferedBytes must be bounded for sidecar VMs",
        ))
    })?;
    let datagram_limit = limits
        .resources
        .max_socket_datagram_queue_len
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "limits.resources.maxSocketDatagramQueueLen must be bounded for sidecar VMs",
            ))
        })?;
    let wasm_linear_memory_limit = limits.resources.max_wasm_memory_bytes.ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "limits.resources.maxWasmMemoryBytes must be bounded for sidecar VMs",
        ))
    })?;
    let _wasm_linear_memory_limit = usize::try_from(wasm_linear_memory_limit).map_err(|_| {
        SidecarError::InvalidState(String::from(
            "limits.resources.maxWasmMemoryBytes exceeds the host address space",
        ))
    })?;
    // `limits.resources.maxWasmMemoryBytes` is the accessible linear-memory
    // cap for each guest memory. Wasmtime's admission reservation also includes
    // bounded table and async-stack envelopes, so reusing the linear cap as the
    // child ledger's aggregate maximum makes one otherwise-valid Store
    // impossible to admit. Keep aggregate Store admission under the distinct,
    // process-wide runtime envelope; the child ledger still gives each VM a
    // bounded scope while the Store limiter independently enforces the exact
    // per-memory linear cap.
    let wasm_aggregate_memory_limit = process
        .usage(ResourceClass::WasmMemoryBytes)
        .limit
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "runtime.resources.maxWasmMemoryBytes must be bounded for sidecar VMs",
            ))
        })?;
    let child_limits = [
        (
            ResourceClass::Capabilities,
            ResourceLimit::new(
                limits.reactor.max_capabilities,
                "limits.reactor.maxCapabilities",
            ),
        ),
        (
            ResourceClass::ReadyHandles,
            ResourceLimit::new(
                limits.reactor.max_ready_handles,
                "limits.reactor.maxReadyHandles",
            ),
        ),
        (
            ResourceClass::Sockets,
            ResourceLimit::new(socket_limit, "limits.resources.maxSockets"),
        ),
        (
            ResourceClass::Connections,
            ResourceLimit::new(connection_limit, "limits.resources.maxConnections"),
        ),
        (
            ResourceClass::BufferedBytes,
            ResourceLimit::new(
                buffered_byte_limit,
                "limits.resources.maxSocketBufferedBytes",
            ),
        ),
        (
            ResourceClass::Datagrams,
            ResourceLimit::new(datagram_limit, "limits.resources.maxSocketDatagramQueueLen"),
        ),
        (
            ResourceClass::Timers,
            ResourceLimit::new(limits.js_runtime.max_timers, "limits.jsRuntime.maxTimers"),
        ),
        (
            ResourceClass::HandleCommands,
            ResourceLimit::new(
                limits.reactor.max_handle_commands,
                "limits.reactor.maxHandleCommands",
            ),
        ),
        (
            ResourceClass::HandleCommandBytes,
            ResourceLimit::new(
                limits.reactor.max_handle_command_bytes,
                "limits.reactor.maxHandleCommandBytes",
            ),
        ),
        (
            ResourceClass::BridgeCalls,
            ResourceLimit::new(
                limits.reactor.max_bridge_calls,
                "limits.reactor.maxBridgeCalls",
            ),
        ),
        (
            ResourceClass::BridgeRequestBytes,
            ResourceLimit::new(
                limits.reactor.max_bridge_request_bytes,
                "limits.reactor.maxBridgeRequestBytes",
            ),
        ),
        (
            ResourceClass::BridgeResponseBytes,
            ResourceLimit::new(
                limits.reactor.max_bridge_response_bytes,
                "limits.reactor.maxBridgeResponseBytes",
            ),
        ),
        (
            ResourceClass::AsyncCompletions,
            ResourceLimit::new(
                limits.reactor.max_async_completions,
                "limits.reactor.maxAsyncCompletions",
            ),
        ),
        (
            ResourceClass::AsyncCompletionBytes,
            ResourceLimit::new(
                limits.reactor.max_async_completion_bytes,
                "limits.reactor.maxAsyncCompletionBytes",
            ),
        ),
        (
            ResourceClass::UdpDatagrams,
            ResourceLimit::new(
                limits.udp.max_buffered_datagrams,
                "limits.udp.maxBufferedDatagrams",
            ),
        ),
        (
            ResourceClass::UdpBytes,
            ResourceLimit::new(limits.udp.max_buffered_bytes, "limits.udp.maxBufferedBytes"),
        ),
        (
            ResourceClass::TlsBytes,
            ResourceLimit::new(limits.tls.max_buffered_bytes, "limits.tls.maxBufferedBytes"),
        ),
        (
            ResourceClass::Tasks,
            ResourceLimit::new(limits.reactor.max_tasks, "limits.reactor.maxTasks"),
        ),
        (
            ResourceClass::ExecutorSlots,
            ResourceLimit::new(
                limits.reactor.max_blocking_jobs,
                "limits.reactor.maxBlockingJobs",
            ),
        ),
        (
            ResourceClass::ExecutorBytes,
            ResourceLimit::new(
                limits.reactor.max_blocking_bytes,
                "limits.reactor.maxBlockingBytes",
            ),
        ),
        (
            ResourceClass::WasmMemoryBytes,
            ResourceLimit::new(
                wasm_aggregate_memory_limit,
                "runtime.resources.maxWasmMemoryBytes",
            ),
        ),
        (
            ResourceClass::WasmThreads,
            ResourceLimit::new(
                limits.wasm.max_concurrent_threads,
                "limits.wasm.maxConcurrentThreads",
            ),
        ),
        (
            ResourceClass::Http2Connections,
            ResourceLimit::new(limits.http2.max_connections, "limits.http2.maxConnections"),
        ),
        (
            ResourceClass::Http2Streams,
            ResourceLimit::new(limits.http2.max_streams, "limits.http2.maxStreams"),
        ),
        (
            ResourceClass::Http2BufferedBytes,
            ResourceLimit::new(
                limits.http2.max_buffered_bytes,
                "limits.http2.maxBufferedBytes",
            ),
        ),
        (
            ResourceClass::Http2HeaderBytes,
            ResourceLimit::new(limits.http2.max_header_bytes, "limits.http2.maxHeaderBytes"),
        ),
        (
            ResourceClass::Http2DataBytes,
            ResourceLimit::new(limits.http2.max_data_bytes, "limits.http2.maxDataBytes"),
        ),
        (
            ResourceClass::Http2Commands,
            ResourceLimit::new(
                limits.http2.max_pending_commands,
                "limits.http2.maxPendingCommands",
            ),
        ),
        (
            ResourceClass::Http2CommandBytes,
            ResourceLimit::new(
                limits.http2.max_pending_command_bytes,
                "limits.http2.maxPendingCommandBytes",
            ),
        ),
        (
            ResourceClass::Http2Events,
            ResourceLimit::new(
                limits.http2.max_pending_events,
                "limits.http2.maxPendingEvents",
            ),
        ),
        (
            ResourceClass::Http2EventBytes,
            ResourceLimit::new(
                limits.http2.max_pending_event_bytes,
                "limits.http2.maxPendingEventBytes",
            ),
        ),
    ];
    for (resource, child_limit) in &child_limits {
        if let Some(parent_limit) = process.usage(*resource).limit {
            if child_limit.maximum > parent_limit {
                return Err(SidecarError::InvalidState(format!(
                    "{} ({}) must be <= process {} ({parent_limit})",
                    child_limit.config_path,
                    child_limit.maximum,
                    match resource {
                        ResourceClass::Capabilities => "runtime.resources.maxCapabilities",
                        ResourceClass::ReadyHandles => "runtime.resources.maxReadyHandles",
                        ResourceClass::Sockets => "runtime.resources.maxSockets",
                        ResourceClass::Connections => "runtime.resources.maxConnections",
                        ResourceClass::BufferedBytes => {
                            "runtime.resources.maxSocketBufferedBytes"
                        }
                        ResourceClass::Datagrams => "runtime.resources.maxDatagrams",
                        ResourceClass::HandleCommands => {
                            "runtime.resources.maxHandleCommands"
                        }
                        ResourceClass::HandleCommandBytes => {
                            "runtime.resources.maxHandleCommandBytes"
                        }
                        ResourceClass::BridgeCalls => "runtime.resources.maxBridgeCalls",
                        ResourceClass::BridgeRequestBytes => {
                            "runtime.resources.maxBridgeRequestBytes"
                        }
                        ResourceClass::BridgeResponseBytes => {
                            "runtime.resources.maxBridgeResponseBytes"
                        }
                        ResourceClass::AsyncCompletions => {
                            "runtime.resources.maxAsyncCompletions"
                        }
                        ResourceClass::AsyncCompletionBytes => {
                            "runtime.resources.maxAsyncCompletionBytes"
                        }
                        ResourceClass::UdpDatagrams => "runtime.resources.maxUdpDatagrams",
                        ResourceClass::UdpBytes => "runtime.resources.maxUdpBytes",
                        ResourceClass::TlsBytes => "runtime.resources.maxTlsBytes",
                        ResourceClass::Timers => "runtime.resources.maxTimers",
                        ResourceClass::Tasks => "runtime.resources.maxTasks",
                        ResourceClass::ExecutorSlots => "runtime.blocking.maxJobs",
                        ResourceClass::ExecutorBytes => "runtime.blocking.maxQueuedBytes",
                        ResourceClass::WasmMemoryBytes => {
                            "runtime.resources.maxWasmMemoryBytes"
                        }
                        ResourceClass::WasmThreads => "runtime.resources.maxWasmThreads",
                        ResourceClass::Http2Connections => "limits.http2.maxConnections",
                        ResourceClass::Http2Streams => "limits.http2.maxStreams",
                        ResourceClass::Http2BufferedBytes => "limits.http2.maxBufferedBytes",
                        ResourceClass::Http2HeaderBytes => "limits.http2.maxHeaderBytes",
                        ResourceClass::Http2DataBytes => "limits.http2.maxDataBytes",
                        ResourceClass::Http2Commands => "limits.http2.maxPendingCommands",
                        ResourceClass::Http2CommandBytes => {
                            "limits.http2.maxPendingCommandBytes"
                        }
                        ResourceClass::Http2Events => "limits.http2.maxPendingEvents",
                        ResourceClass::Http2EventBytes => "limits.http2.maxPendingEventBytes",
                    }
                )));
            }
        }
    }
    Ok(ResourceLedger::child(
        format!("vm={vm_id} generation={generation}"),
        child_limits,
        process,
    ))
}

// ---------------------------------------------------------------------------
// Free functions — VM lifecycle helpers
// ---------------------------------------------------------------------------

fn native_root_plugin_from_config(
    config: Option<&vm_config::NativeRootFilesystemConfig>,
) -> Result<Option<NativeRootPluginConfig>, SidecarError> {
    let Some(config) = config else {
        return Ok(None);
    };
    let plugin_config = serde_json::to_string(&config.plugin.config).map_err(|error| {
        SidecarError::InvalidState(format!(
            "failed to serialize nativeRoot.plugin.config: {error}"
        ))
    })?;
    Ok(Some(NativeRootPluginConfig {
        plugin: MountPluginDescriptor {
            id: config.plugin.id.clone(),
            config: plugin_config,
        },
        read_only: config.read_only,
    }))
}

fn vm_dns_config_from_config(
    config: Option<&vm_config::VmDnsConfig>,
) -> Result<VmDnsConfig, SidecarError> {
    let Some(config) = config else {
        return Ok(VmDnsConfig::default());
    };
    let name_servers = config
        .name_servers
        .iter()
        .map(|entry| parse_vm_dns_nameserver(entry))
        .collect::<Result<Vec<_>, _>>()?;
    let mut overrides = BTreeMap::new();
    for (hostname, addresses) in &config.overrides {
        let normalized_hostname = normalize_dns_hostname(hostname)?;
        let parsed_addresses = addresses
            .iter()
            .map(|entry| {
                entry.parse::<IpAddr>().map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "invalid DNS override {hostname}={entry}: {error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        overrides.insert(normalized_hostname, parsed_addresses);
    }
    Ok(VmDnsConfig {
        name_servers,
        overrides,
    })
}

fn vm_listen_policy_from_config(
    config: Option<&vm_config::VmListenPolicyConfig>,
) -> Result<VmListenPolicy, SidecarError> {
    let mut policy = VmListenPolicy::default();
    let Some(config) = config else {
        return Ok(policy);
    };
    if let Some(port_min) = config.port_min {
        policy.port_min = port_min;
    }
    if let Some(port_max) = config.port_max {
        policy.port_max = port_max;
    }
    if policy.port_min > policy.port_max {
        return Err(SidecarError::InvalidState(format!(
            "invalid listen port range {} exceeds {}",
            policy.port_min, policy.port_max
        )));
    }
    if let Some(allow_privileged) = config.allow_privileged {
        policy.allow_privileged = allow_privileged;
    }
    Ok(policy)
}

#[derive(Debug, Clone)]
struct NativeRootPluginConfig {
    plugin: MountPluginDescriptor,
    read_only: bool,
}

fn build_native_root_mount_table<B>(
    mount_plugins: &agentos_kernel::mount_plugin::FileSystemPluginRegistry<MountPluginContext<B>>,
    native_root: &NativeRootPluginConfig,
    descriptor: &RootFilesystemDescriptor,
    context: MountPluginContext<B>,
) -> Result<MountTable, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !descriptor.lowers.is_empty() {
        return Err(SidecarError::InvalidState(String::from(
            "native root filesystems do not support rootFilesystem.lowers",
        )));
    }

    let config_value: serde_json::Value = serde_json::from_str(&native_root.plugin.config)
        .map_err(|error| {
            SidecarError::InvalidState(format!(
                "root native plugin config for {} is not valid JSON: {error}",
                native_root.plugin.id
            ))
        })?;
    let mut filesystem = mount_plugins
        .open(
            &native_root.plugin.id,
            OpenFileSystemPluginRequest {
                vm_id: &context.vm_id,
                guest_path: "/",
                read_only: native_root.read_only,
                config: &config_value,
                context: &context,
            },
        )
        .map_err(plugin_error)?;

    bootstrap_native_root_filesystem(filesystem.as_mut(), descriptor)?;

    Ok(MountTable::new_boxed_root(
        filesystem,
        MountOptions::new(native_root.plugin.id.clone()).read_only(native_root.read_only),
    ))
}

fn bootstrap_native_root_filesystem(
    filesystem: &mut dyn MountedFileSystem,
    descriptor: &RootFilesystemDescriptor,
) -> Result<(), SidecarError> {
    for (guest_path, mode, uid, gid) in ROOT_BOOTSTRAP_DIRS {
        filesystem.mkdir(guest_path, true).map_err(vfs_error)?;
        filesystem.chmod(guest_path, *mode).map_err(vfs_error)?;
        filesystem
            .chown(guest_path, *uid, *gid)
            .map_err(vfs_error)?;
    }

    seed_native_ca_certificates_bundle(filesystem)?;

    for entry in &descriptor.bootstrap_entries {
        apply_native_root_filesystem_entry(filesystem, entry)?;
    }

    Ok(())
}

fn apply_native_root_filesystem_entry(
    filesystem: &mut dyn MountedFileSystem,
    entry: &RootFilesystemEntry,
) -> Result<(), SidecarError> {
    let snapshot = root_snapshot_from_entries(std::slice::from_ref(entry))?;
    let kernel_entry = snapshot
        .entries
        .into_iter()
        .next()
        .expect("root snapshot from one entry should contain one entry");
    ensure_mounted_parent_directories(filesystem, &kernel_entry.path)?;
    prepare_mounted_destination(filesystem, &kernel_entry.path, &kernel_entry.kind)?;

    match kernel_entry.kind {
        KernelFilesystemEntryKind::Directory => filesystem
            .mkdir(&kernel_entry.path, true)
            .map_err(vfs_error)?,
        KernelFilesystemEntryKind::File => filesystem
            .write_file(&kernel_entry.path, kernel_entry.content.unwrap_or_default())
            .map_err(vfs_error)?,
        KernelFilesystemEntryKind::Symlink => filesystem
            .symlink(
                kernel_entry.target.as_deref().ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "root filesystem bootstrap for symlink {} requires a target",
                        entry.path
                    ))
                })?,
                &kernel_entry.path,
            )
            .map_err(vfs_error)?,
    }

    if !matches!(kernel_entry.kind, KernelFilesystemEntryKind::Symlink) {
        filesystem
            .chmod(&kernel_entry.path, kernel_entry.mode)
            .map_err(vfs_error)?;
        filesystem
            .chown(&kernel_entry.path, kernel_entry.uid, kernel_entry.gid)
            .map_err(vfs_error)?;
    }

    Ok(())
}

fn seed_native_ca_certificates_bundle(
    filesystem: &mut dyn MountedFileSystem,
) -> Result<(), SidecarError> {
    if CA_CERTIFICATES_BUNDLE.is_empty() {
        return Err(SidecarError::Io(
            "embedded Mozilla CA certificate bundle is empty".to_string(),
        ));
    }

    if !mounted_entry_exists(filesystem, CA_CERTIFICATES_GUEST_PATH)? {
        ensure_mounted_parent_directories(filesystem, CA_CERTIFICATES_GUEST_PATH)?;
        filesystem
            .write_file(CA_CERTIFICATES_GUEST_PATH, CA_CERTIFICATES_BUNDLE.to_vec())
            .map_err(vfs_error)?;
        filesystem
            .chmod(CA_CERTIFICATES_GUEST_PATH, 0o644)
            .map_err(vfs_error)?;
        filesystem
            .chown(CA_CERTIFICATES_GUEST_PATH, 0, 0)
            .map_err(vfs_error)?;
    }

    if !mounted_entry_exists(filesystem, CA_CERTIFICATES_SYMLINK_PATH)? {
        ensure_mounted_parent_directories(filesystem, CA_CERTIFICATES_SYMLINK_PATH)?;
        filesystem
            .symlink(CA_CERTIFICATES_SYMLINK_TARGET, CA_CERTIFICATES_SYMLINK_PATH)
            .map_err(vfs_error)?;
    }

    Ok(())
}

fn mounted_entry_exists(
    filesystem: &dyn MountedFileSystem,
    path: &str,
) -> Result<bool, SidecarError> {
    match filesystem.lstat(path) {
        Ok(_) => Ok(true),
        Err(error) if error.code() == "ENOENT" => Ok(false),
        Err(error) => Err(vfs_error(error)),
    }
}

fn prepare_mounted_destination(
    filesystem: &mut dyn MountedFileSystem,
    path: &str,
    desired_kind: &KernelFilesystemEntryKind,
) -> Result<(), SidecarError> {
    let existing = match filesystem.lstat(path) {
        Ok(existing) => existing,
        Err(error) if error.code() == "ENOENT" => return Ok(()),
        Err(error) => return Err(vfs_error(error)),
    };
    let already_compatible = match desired_kind {
        KernelFilesystemEntryKind::Directory => existing.is_directory && !existing.is_symbolic_link,
        KernelFilesystemEntryKind::File => !existing.is_directory && !existing.is_symbolic_link,
        KernelFilesystemEntryKind::Symlink => false,
    };
    if already_compatible {
        return Ok(());
    }

    if existing.is_directory && !existing.is_symbolic_link {
        filesystem.remove_dir(path).map_err(vfs_error)?;
    } else {
        filesystem.remove_file(path).map_err(vfs_error)?;
    }
    Ok(())
}

fn ensure_mounted_parent_directories(
    filesystem: &mut dyn MountedFileSystem,
    path: &str,
) -> Result<(), SidecarError> {
    let parent = dirname(path);
    if parent != "/" && !filesystem.exists(&parent) {
        ensure_mounted_parent_directories(filesystem, &parent)?;
        filesystem.mkdir(&parent, true).map_err(vfs_error)?;
    }
    Ok(())
}

fn reconcile_mounts<B>(
    mount_plugins: &agentos_kernel::mount_plugin::FileSystemPluginRegistry<MountPluginContext<B>>,
    vm: &mut VmState,
    mounts: &[crate::protocol::MountDescriptor],
    context: MountPluginContext<B>,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    shutdown_configured_mounts(vm, &context, "configure_vm", false)?;
    mount_leaf_descriptors(mount_plugins, vm, mounts, context)
}

fn mount_leaf_descriptors<B>(
    mount_plugins: &agentos_kernel::mount_plugin::FileSystemPluginRegistry<MountPluginContext<B>>,
    vm: &mut VmState,
    mounts: &[crate::protocol::MountDescriptor],
    context: MountPluginContext<B>,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let max_filesystem_bytes = vm.kernel.resource_limits().max_filesystem_bytes;
    let max_inode_count = vm.kernel.resource_limits().max_inode_count;
    // Mount parents before nested leaves. Configure payload order is not a
    // filesystem invariant, and mounting a parent after one of its children is
    // rejected by the kernel mount table.
    let mut ordered_mounts = mounts.iter().collect::<Vec<_>>();
    ordered_mounts.sort_by_key(|mount| mount_path_depth(&mount.guest_path));
    for mount in ordered_mounts {
        let config_value: serde_json::Value =
            serde_json::from_str(&mount.plugin.config).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "mount plugin config for {} is not valid JSON: {error}",
                    mount.plugin.id
                ))
            })?;
        let filesystem = mount_plugins
            .open(
                &mount.plugin.id,
                OpenFileSystemPluginRequest {
                    vm_id: &context.vm_id,
                    guest_path: &mount.guest_path,
                    read_only: mount.read_only,
                    config: &config_value,
                    context: &context,
                },
            )
            .map_err(plugin_error)?;

        vm.kernel
            .mount_boxed_filesystem(
                &mount.guest_path,
                filesystem,
                MountOptions::new(mount.plugin.id.clone())
                    .guest_source(mount.guest_source.clone())
                    .guest_fstype(mount.guest_fstype.clone())
                    .read_only(mount.read_only)
                    .max_bytes(max_filesystem_bytes)
                    .max_inodes(max_inode_count)
                    .absolute_symlinks_mount_relative(
                        matches!(mount.plugin.id.as_str(), "host_dir" | "module_access")
                            || (mount.plugin.id == "agentos_packages"
                                && matches!(
                                    config_value.get("kind").and_then(serde_json::Value::as_str),
                                    Some("tar" | "hostDir")
                                )),
                    ),
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

fn shutdown_configured_mounts<B>(
    vm: &mut VmState,
    context: &MountPluginContext<B>,
    phase: &str,
    continue_on_error: bool,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    // Nested leaves must be detached before their parents. In particular, npm
    // workspace packages are explicit child mounts below `/node_modules`.
    let mut existing_mounts = vm.configuration.mounts.clone();
    existing_mounts.sort_by_key(|mount| std::cmp::Reverse(mount_path_depth(&mount.guest_path)));
    for existing in existing_mounts {
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
            Err(error) => {
                if let Err(emit_error) = emit_structured_event(
                    &context.bridge,
                    &context.vm_id,
                    "filesystem.mount.shutdown_failed",
                    audit_fields([
                        (String::from("guest_path"), existing.guest_path.clone()),
                        (String::from("plugin_id"), existing.plugin.id.clone()),
                        (String::from("read_only"), existing.read_only.to_string()),
                        (String::from("phase"), String::from(phase)),
                        (String::from("error_code"), String::from(error.code())),
                        (String::from("error"), error.to_string()),
                    ]),
                ) {
                    eprintln!(
                        "ERR_AGENTOS_DIAGNOSTIC_EMIT: failed to emit mount shutdown failure for VM {} at {}: {emit_error:?}",
                        context.vm_id, existing.guest_path
                    );
                }

                if !continue_on_error {
                    return Err(kernel_error(error));
                }
            }
        }
    }

    Ok(())
}

fn mount_path_depth(path: &str) -> usize {
    path.split('/')
        .filter(|component| !component.is_empty())
        .count()
}

/// Build the `/opt/agentos` package projection for `configure_vm`.
///
/// The projection mounts the package tar directly and serves derived aliases as
/// synthetic symlink leaves. This eliminates extraction and the old host-disk
/// symlink farm: the tar VFS indexes member offsets once and reads mmap-backed
/// byte ranges. Each managed entry is a granular leaf mount, while parent dirs
/// such as `/opt/agentos/bin` and `/opt/agentos/pkgs/<pkg>` remain writable
/// overlay dirs so guest-installed commands can coexist beside managed entries.
fn build_packages_projection(
    _vm_id: &str,
    packages: &[crate::package_projection::PackageDescriptor],
    mount_at: &str,
) -> Result<Vec<MountDescriptor>, SidecarError> {
    Ok(
        crate::package_projection::build_package_leaf_mounts(packages, mount_at)?
            .into_iter()
            .map(package_leaf_mount_to_descriptor)
            .collect(),
    )
}

fn package_leaf_mount_to_descriptor(
    mount: crate::package_projection::PackageLeafMount,
) -> MountDescriptor {
    match mount {
        crate::package_projection::PackageLeafMount::Tar {
            guest_path,
            tar_path,
            root,
        } => MountDescriptor {
            guest_path,
            guest_source: String::from("agentos_packages"),
            guest_fstype: String::from("agentos_packages"),
            read_only: true,
            plugin: MountPluginDescriptor {
                id: String::from("agentos_packages"),
                config: serde_json::json!({
                    "kind": "tar",
                    "tarPath": tar_path,
                    "root": root,
                    "readOnly": true,
                })
                .to_string(),
            },
        },
        crate::package_projection::PackageLeafMount::HostDir {
            guest_path,
            host_path,
        } => MountDescriptor {
            guest_path,
            guest_source: String::from("agentos_packages"),
            guest_fstype: String::from("agentos_packages"),
            read_only: true,
            plugin: MountPluginDescriptor {
                id: String::from("agentos_packages"),
                config: serde_json::json!({
                    "kind": "hostDir",
                    "hostPath": host_path,
                    "readOnly": true,
                })
                .to_string(),
            },
        },
        crate::package_projection::PackageLeafMount::SingleSymlink { guest_path, target } => {
            MountDescriptor {
                guest_path,
                guest_source: String::from("agentos_packages"),
                guest_fstype: String::from("agentos_packages"),
                read_only: true,
                plugin: MountPluginDescriptor {
                    id: String::from("agentos_packages"),
                    config: serde_json::json!({
                        "kind": "singleSymlink",
                        "target": target,
                        "readOnly": true,
                    })
                    .to_string(),
                },
            }
        }
    }
}

fn package_descriptors_from_wire(
    packages: &[crate::protocol::PackageDescriptor],
) -> Result<Vec<crate::package_projection::PackageDescriptor>, SidecarError> {
    packages
        .iter()
        .map(|package| crate::package_projection::read_package_manifest_from_path(&package.path))
        .collect()
}

fn projected_agent_launch_from_descriptors(
    packages: &[crate::package_projection::PackageDescriptor],
) -> BTreeMap<String, crate::state::ProjectedAgentLaunch> {
    packages
        .iter()
        .filter_map(|package| {
            let acp_entrypoint = package.acp_entrypoint.clone()?;
            Some((
                package.name.clone(),
                crate::state::ProjectedAgentLaunch {
                    acp_entrypoint,
                    env: package.agent_env.clone().into_iter().collect(),
                    launch_args: package.agent_launch_args.clone(),
                },
            ))
        })
        .collect()
}

fn projected_agents_from_descriptors(
    packages: &[crate::package_projection::PackageDescriptor],
) -> Vec<AgentosProjectedAgent> {
    packages
        .iter()
        .flat_map(|package| {
            let Some(acp_entrypoint) = package.acp_entrypoint.as_ref() else {
                return Vec::new();
            };
            vec![AgentosProjectedAgent {
                id: package.name.clone(),
                acp_entrypoint: acp_entrypoint.clone(),
                adapter_entrypoint: format!(
                    "{}/{}",
                    crate::package_projection::OPT_AGENTOS_BIN,
                    acp_entrypoint
                ),
            }]
        })
        .collect()
}

fn resolve_agent_snapshot_bundle(
    packages: &[crate::package_projection::PackageDescriptor],
) -> Result<Option<String>, SidecarError> {
    for package in packages {
        if let Some(bundle) = crate::package_projection::read_agent_snapshot_bundle(package)? {
            return Ok(Some(bundle));
        }
    }
    Ok(None)
}

fn apply_package_provides_env(
    guest_env: &mut BTreeMap<String, String>,
    packages: &[crate::package_projection::PackageDescriptor],
) {
    for package in packages {
        let Some(provides) = package.provides.as_ref() else {
            continue;
        };
        for (key, value) in &provides.env {
            guest_env
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }
}

fn append_package_provides_mounts(
    mounts: &mut Vec<MountDescriptor>,
    packages: &[crate::package_projection::PackageDescriptor],
) -> Result<(), SidecarError> {
    for package in packages {
        let Some(provides) = package.provides.as_ref() else {
            continue;
        };
        for file in &provides.files {
            match crate::package_projection::package_provides_file_mount(
                package,
                &file.source,
                &file.target,
            )? {
                Some(mount) => mounts.push(package_leaf_mount_to_descriptor(mount)),
                None => {
                    tracing::warn!(
                        package = %package.name,
                        source = %file.source,
                        target = %file.target,
                        "package provides file source is not a directory; skipping"
                    );
                }
            }
        }
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
        guest_source: String::from("module_access"),
        guest_fstype: String::from("module_access"),
        read_only: true,
        plugin: MountPluginDescriptor {
            id: String::from("module_access"),
            config: serde_json::json!({
                "hostPath": root,
            })
            .to_string(),
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
        guest_source: String::from("host_dir"),
        guest_fstype: String::from("host_dir"),
        read_only: true,
        plugin: MountPluginDescriptor {
            id: String::from("host_dir"),
            config: serde_json::json!({
                "hostPath": target,
                "readOnly": true,
            })
            .to_string(),
        },
    });
    Ok(())
}

fn sidecar_core_error(error: agentos_native_sidecar_core::SidecarCoreError) -> SidecarError {
    SidecarError::InvalidState(error.to_string())
}

fn resolve_guest_cwd(value: Option<&String>) -> String {
    value
        .map(|path| normalize_guest_path(path))
        .unwrap_or_else(|| String::from("/workspace"))
}

fn resolve_vm_cwds(
    metadata_cwd: Option<&String>,
    runtime_scratch_root: &Path,
) -> Result<(String, PathBuf), SidecarError> {
    if let Some(raw_cwd) = metadata_cwd {
        let candidate = PathBuf::from(raw_cwd);
        if candidate.is_absolute() || raw_cwd.starts_with('.') {
            let resolved_host_cwd = resolve_host_path(Some(raw_cwd))?;
            return Ok((String::from("/"), resolved_host_cwd));
        }
    }

    let guest_cwd = resolve_guest_cwd(metadata_cwd);
    let host_cwd = runtime_scratch_path_for_guest(runtime_scratch_root, &guest_cwd);
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

fn create_vm_runtime_scratch_root(vm_id: &str) -> Result<PathBuf, SidecarError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            SidecarError::Io(format!("failed to compute scratch-root nonce: {error}"))
        })?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("agentos-native-sidecar-runtime-{vm_id}-{nonce}"));
    fs::create_dir_all(&root)
        .map_err(|error| SidecarError::Io(format!("failed to create VM runtime root: {error}")))?;
    initialize_vm_runtime_scratch_root(root)
}

fn initialize_vm_runtime_scratch_root(root: PathBuf) -> Result<PathBuf, SidecarError> {
    let cleanup_root = root.clone();
    // macOS: `std::env::temp_dir()` lives under `/var/folders/…`, but `/var` is a
    // symlink to `/private/var`, and macOS fd→path recovery (`fcntl(F_GETPATH)`)
    // reports the resolved `/private/var/…` form. Canonicalize the private
    // runtime root so executor confinement compares the same host path form.
    #[cfg(target_os = "macos")]
    let initialized = fs::canonicalize(&root).map_err(|error| {
        SidecarError::Io(format!("failed to canonicalize VM runtime root: {error}"))
    });
    #[cfg(not(target_os = "macos"))]
    let initialized: Result<PathBuf, SidecarError> = Ok(root);

    match initialized {
        Ok(root) => Ok(root),
        Err(error) => match fs::remove_dir_all(&cleanup_root) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(SidecarError::Io(format!(
                "{error}; additionally failed to clean runtime root {}: {cleanup_error}",
                cleanup_root.display()
            ))),
        },
    }
}

fn runtime_scratch_path_for_guest(runtime_root: &Path, guest_path: &str) -> PathBuf {
    let relative = normalize_guest_path(guest_path);
    let relative = relative.trim_start_matches('/');
    if relative.is_empty() {
        runtime_root.to_path_buf()
    } else {
        runtime_root.join(relative)
    }
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
    command_search_roots: &[String],
) {
    let mut merged = Vec::new();
    let mut seen = BTreeSet::new();

    for root in command_search_roots {
        let normalized = normalize_path(root);
        if normalized == "/" {
            continue;
        }
        if seen.insert(normalized.clone()) {
            merged.push(normalized);
        }
    }

    for segment in DEFAULT_GUEST_PATH_ENV.split(':') {
        let normalized = normalize_path(segment);
        if seen.insert(normalized.clone()) {
            merged.push(normalized);
        }
    }

    // PATH is derived state. Strip roots managed by the command projection
    // before preserving caller-supplied extras, so a removed numeric legacy
    // mount cannot survive forever merely because it appeared in the previous
    // synthesized PATH value.
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
            if is_managed_guest_command_path_segment(&normalized) {
                continue;
            }
            if seen.insert(normalized.clone()) {
                merged.push(normalized);
            }
        }
    }

    guest_env.insert(String::from("PATH"), merged.join(":"));
}

fn is_managed_guest_command_path_segment(segment: &str) -> bool {
    let normalized = if segment.starts_with('/') {
        normalize_path(segment)
    } else {
        segment.to_owned()
    };
    if DEFAULT_GUEST_PATH_ENV
        .split(':')
        .any(|default| normalize_path(default) == normalized)
    {
        return true;
    }
    normalized
        .strip_prefix("/__agentos/commands/")
        .is_some_and(|root| {
            !root.is_empty() && !root.contains('/') && root.chars().all(|ch| ch.is_ascii_digit())
        })
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

// Retained for the native-root command-stub test; `python` is now a real
// command so production no longer prunes `/bin/python`.
#[cfg(test)]
fn prune_kernel_command_stub(
    kernel: &mut KernelVm<agentos_kernel::mount_table::MountTable>,
    path: &str,
) -> Result<(), SidecarError> {
    if !kernel.exists(path).map_err(kernel_error)? {
        return Ok(());
    }

    let content = kernel.read_file(path).map_err(kernel_error)?;
    if content == KERNEL_COMMAND_STUB {
        kernel.remove_file(path).map_err(kernel_error)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        bootstrap_native_root_filesystem, close_vm_admission, create_vm_unix_socket_host_dir,
        execution_driver_commands, native_root_plugin_from_config,
        projected_commands_from_provided_commands, prune_kernel_command_stub,
        refresh_guest_command_path_env, retire_vm_fairness, vm_quarantine_reason,
        vm_resource_ledger, wait_for_vm_reconciliation, CA_CERTIFICATES_BUNDLE,
        CA_CERTIFICATES_GUEST_PATH, CA_CERTIFICATES_SYMLINK_PATH, DEFAULT_GUEST_PATH_ENV,
        KERNEL_COMMAND_STUB,
    };
    use crate::bootstrap::KernelCommandInventory;
    use crate::bridge::MountPluginContext;
    use crate::plugins::chunked_local::ChunkedLocalMountPlugin;
    use crate::protocol::{RootFilesystemDescriptor, RootFilesystemEntry, RootFilesystemEntryKind};
    use crate::service::NativeSidecar;
    use crate::state::{
        ConnectionState, QuarantinedVmGeneration, SessionState, VmQuarantineReason,
        VmReconciliationSnapshot,
    };
    use crate::stdio::LocalBridge;
    use agentos_kernel::kernel::{KernelVm, KernelVmConfig};
    use agentos_kernel::mount_plugin::{FileSystemPluginFactory, OpenFileSystemPluginRequest};
    use agentos_kernel::mount_table::{MountOptions, MountTable};
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::VirtualFileSystem;
    use agentos_runtime_tokio::accounting::{ResourceClass, ResourceLedger, ResourceLimit};
    use agentos_runtime_tokio::capability::{CapabilityKind, CapabilityRegistry};
    use agentos_runtime_tokio::fairness::FairBudget;
    use agentos_runtime_tokio::metrics::ResourceMetricClass;
    use agentos_runtime_tokio::{RuntimeContext, SidecarRuntime, TaskClass};
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn reconciliation_handles(
        generation: u64,
    ) -> (Arc<ResourceLedger>, RuntimeContext, CapabilityRegistry) {
        let process = SidecarRuntime::process(&agentos_runtime_tokio::RuntimeConfig::default())
            .expect("initialize process runtime")
            .context();
        let resources = Arc::new(ResourceLedger::child(
            format!("teardown-test-vm-generation={generation}"),
            [
                (
                    ResourceClass::Tasks,
                    ResourceLimit::new(4, "limits.reactor.maxTasks"),
                ),
                (
                    ResourceClass::Capabilities,
                    ResourceLimit::new(4, "limits.reactor.maxCapabilities"),
                ),
                (
                    ResourceClass::Sockets,
                    ResourceLimit::new(4, "limits.resources.maxSockets"),
                ),
            ],
            Arc::clone(process.resources()),
        ));
        let runtime_context = process.scoped_for_vm(Arc::clone(&resources), generation);
        let capabilities = CapabilityRegistry::new(generation, Arc::clone(&resources));
        (resources, runtime_context, capabilities)
    }

    #[test]
    fn guest_command_path_rebuild_drops_removed_managed_roots() {
        let mut guest_env = BTreeMap::from([(
            String::from("PATH"),
            format!(
                "/__agentos/commands/001:{DEFAULT_GUEST_PATH_ENV}:/custom/bin:relative:/__agentos/commands/custom"
            ),
        )]);

        refresh_guest_command_path_env(&mut guest_env, &[String::from("/__agentos/commands/002")]);

        assert_eq!(
            guest_env.get("PATH").map(String::as_str),
            Some(
                "/__agentos/commands/002:/usr/local/sbin:/usr/local/bin:/opt/agentos/bin:/usr/sbin:/usr/bin:/sbin:/bin:/custom/bin:relative:/__agentos/commands/custom"
            )
        );
    }

    #[test]
    fn transient_inventory_drives_registration_and_projection_reporting() {
        let kernel_commands = KernelCommandInventory {
            names: BTreeSet::from([String::from("legacy"), String::from("shadowed")]),
            search_roots: vec![String::from("/__agentos/commands/001")],
        };
        let provided_commands = BTreeMap::from([
            (
                String::from("pkg-a"),
                vec![String::from("visible"), String::from("shadowed")],
            ),
            (String::from("pkg-b"), vec![String::from("second")]),
        ]);

        let registered = execution_driver_commands(
            &kernel_commands,
            &provided_commands,
            [String::from("binding"), String::from("visible")],
        );
        assert_eq!(
            registered.iter().collect::<BTreeSet<_>>().len(),
            registered.len()
        );
        for expected in [
            "binding", "legacy", "node", "python", "python3", "second", "shadowed", "visible",
            "wasm",
        ] {
            assert!(registered.iter().any(|command| command == expected));
        }

        assert_eq!(
            projected_commands_from_provided_commands(&provided_commands, &kernel_commands),
            vec![
                crate::protocol::ProjectedCommand {
                    name: String::from("second"),
                    guest_path: String::from("/opt/agentos/bin/second"),
                },
                crate::protocol::ProjectedCommand {
                    name: String::from("visible"),
                    guest_path: String::from("/opt/agentos/bin/visible"),
                },
            ]
        );
    }

    #[test]
    fn vm_runtime_bounds_every_resource_class_by_default() {
        let process = SidecarRuntime::process(&agentos_runtime_tokio::RuntimeConfig::default())
            .expect("initialize process runtime")
            .context();
        let ledger = vm_resource_ledger(
            "vm-all-resource-limits",
            88_001,
            &crate::limits::VmLimits::default(),
            Arc::clone(process.resources()),
        )
        .expect("construct bounded VM ledger");

        for resource in ResourceClass::ALL {
            let usage = ledger.usage(resource);
            assert_eq!(usage.used, 0, "{} starts charged", resource.name());
            assert!(
                usage.limit.is_some_and(|limit| limit > 0),
                "{} has no positive VM limit",
                resource.name()
            );
        }
        assert_eq!(
            ledger.usage(ResourceClass::WasmMemoryBytes).limit,
            process
                .resources()
                .usage(ResourceClass::WasmMemoryBytes)
                .limit,
            "the VM aggregate Store envelope must inherit the bounded process ceiling"
        );
        assert_ne!(
            ledger.usage(ResourceClass::WasmMemoryBytes).limit,
            crate::limits::VmLimits::default()
                .resources
                .max_wasm_memory_bytes
                .and_then(|value| usize::try_from(value).ok()),
            "the per-memory linear cap must not be reused as Store-overhead admission"
        );
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("teardown test runtime")
            .block_on(future)
    }

    fn active_vm_metric(sidecar: &NativeSidecar<LocalBridge>) -> usize {
        sidecar
            .runtime_context
            .as_ref()
            .expect("process runtime context")
            .metrics()
            .snapshot()
            .resources[ResourceMetricClass::ActiveVms.index()]
        .current
    }

    #[test]
    fn teardown_start_closes_capability_and_executor_admission() {
        let (_resources, runtime_context, capabilities) = reconciliation_handles(70_001);
        let stale_runtime_context = runtime_context.clone();
        close_vm_admission(&runtime_context, &capabilities).expect("close VM admission");
        let error = capabilities
            .reserve(CapabilityKind::UdpSocket)
            .expect_err("closed VM generation must reject new capabilities");
        assert!(error
            .to_string()
            .contains("ERR_AGENTOS_CAPABILITY_REGISTRY_CLOSED"));
        let task_error = stale_runtime_context
            .spawn(TaskClass::Vm, async {})
            .expect_err("stale VM runtime clone must reject new executor work");
        assert!(task_error
            .to_string()
            .contains("ERR_AGENTOS_TASK_ADMISSION_CLOSED"));
    }

    #[test]
    fn teardown_fairness_retirement_survives_generation_churn_past_max_vms() {
        let process = SidecarRuntime::process(&agentos_runtime_tokio::RuntimeConfig::default())
            .expect("initialize process runtime")
            .context();

        block_on(async {
            let mut first_generation = None;
            for _ in 0..=4_096 {
                let generation = process
                    .allocate_vm_generation()
                    .expect("allocate churn VM generation");
                first_generation.get_or_insert(generation);
                let turn = process
                    .fairness()
                    .acquire(generation, 1, FairBudget::new(1, 1))
                    .await
                    .expect("acquire churn fairness turn");
                turn.complete(FairBudget::new(1, 1), false)
                    .expect("complete churn fairness turn");
                retire_vm_fairness(&process, generation)
                    .expect("teardown must retire churn VM fairness membership");
            }

            let first_generation = first_generation.expect("at least one churn generation");
            let error = process
                .fairness()
                .acquire(first_generation, 2, FairBudget::new(1, 1))
                .await
                .expect_err("retired VM generation must not re-enroll");
            assert!(
                error
                    .to_string()
                    .contains("ERR_AGENTOS_FAIRNESS_CAPABILITY_RETIRED"),
                "{error}"
            );

            let successor = process
                .allocate_vm_generation()
                .expect("allocate post-churn VM generation");
            let turn = process
                .fairness()
                .acquire(successor, 1, FairBudget::new(1, 1))
                .await
                .expect("retirement must reclaim maxVms membership");
            turn.complete(FairBudget::new(1, 1), false)
                .expect("complete post-churn fairness turn");
            retire_vm_fairness(&process, successor)
                .expect("retire post-churn VM fairness membership");
        });
    }

    #[test]
    fn vm_executor_limits_must_fit_process_executor_limits() {
        let limits = crate::limits::VmLimits::default();
        for (resource, maximum, child_path, process_path) in [
            (
                ResourceClass::ExecutorSlots,
                1,
                "limits.reactor.maxBlockingJobs",
                "runtime.blocking.maxJobs",
            ),
            (
                ResourceClass::ExecutorBytes,
                1,
                "limits.reactor.maxBlockingBytes",
                "runtime.blocking.maxQueuedBytes",
            ),
        ] {
            let process = Arc::new(ResourceLedger::root(
                format!("executor-ceiling-test-{resource:?}"),
                [
                    (resource, ResourceLimit::new(maximum, process_path)),
                    (
                        ResourceClass::WasmMemoryBytes,
                        ResourceLimit::new(
                            1024 * 1024 * 1024,
                            "runtime.resources.maxWasmMemoryBytes",
                        ),
                    ),
                ],
            ));
            let error = vm_resource_ledger("vm-test", 70_005, &limits, process)
                .expect_err("VM executor limit must not exceed its process ceiling");
            let diagnostic = error.to_string();
            assert!(diagnostic.contains(child_path), "{diagnostic}");
            assert!(diagnostic.contains(process_path), "{diagnostic}");
        }
    }

    #[test]
    fn empty_vm_generation_reconciles_without_waiting() {
        let (resources, runtime_context, capabilities) = reconciliation_handles(70_002);
        let (snapshot, deadline_expired) = block_on(wait_for_vm_reconciliation(
            resources.as_ref(),
            &runtime_context,
            &capabilities,
            Duration::ZERO,
        ));
        assert!(!deadline_expired);
        assert_eq!(snapshot.active_tasks, 0);
        assert_eq!(snapshot.outstanding_capabilities, 0);
        assert!(snapshot.ledger_zero);
        assert!(snapshot.integrity_ok);
        assert_eq!(vm_quarantine_reason(false, false, snapshot, false), None);
    }

    #[test]
    fn fairness_retirement_failure_is_a_non_reapable_integrity_quarantine() {
        let generation = 70_007;
        let (resources, runtime_context, capabilities) = reconciliation_handles(generation);
        let snapshot = VmReconciliationSnapshot {
            active_tasks: 0,
            outstanding_capabilities: 0,
            ledger_zero: true,
            integrity_ok: true,
        };
        assert_eq!(
            vm_quarantine_reason(false, true, snapshot, false),
            Some(VmQuarantineReason::FairnessIntegrity)
        );
        let quarantined = QuarantinedVmGeneration {
            connection_id: String::from("conn-test"),
            session_id: String::from("session-test"),
            vm_id: String::from("vm-test"),
            generation,
            resources,
            runtime_context,
            capabilities,
            reason: VmQuarantineReason::FairnessIntegrity,
        };
        assert!(quarantined.reconciliation_snapshot().ledger_zero);
        assert!(!quarantined.can_reap());
    }

    #[test]
    fn integrity_quarantine_is_never_reaped_after_counts_reconcile() {
        let generation = 70_006;
        let (resources, runtime_context, capabilities) = reconciliation_handles(generation);
        let quarantined = QuarantinedVmGeneration {
            connection_id: String::from("conn-test"),
            session_id: String::from("session-test"),
            vm_id: String::from("vm-test"),
            generation,
            resources,
            runtime_context,
            capabilities,
            reason: VmQuarantineReason::ResourceIntegrity,
        };
        assert!(quarantined.reconciliation_snapshot().ledger_zero);
        assert!(!quarantined.can_reap());
    }

    #[test]
    fn stuck_supervised_task_enters_quarantine_until_barrier_releases() {
        block_on(async {
            let generation = 70_003;
            let (resources, runtime_context, capabilities) = reconciliation_handles(generation);
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let task = runtime_context
                .spawn(TaskClass::Vm, async move {
                    let _ = started_tx.send(());
                    let _ = release_rx.await;
                })
                .expect("spawn supervised VM task");
            started_rx
                .await
                .expect("task reached deterministic barrier");

            let (snapshot, deadline_expired) = wait_for_vm_reconciliation(
                resources.as_ref(),
                &runtime_context,
                &capabilities,
                Duration::ZERO,
            )
            .await;
            assert!(deadline_expired);
            assert_eq!(snapshot.active_tasks, 1);
            assert_eq!(
                vm_quarantine_reason(false, false, snapshot, deadline_expired),
                Some(VmQuarantineReason::TeardownDeadline)
            );

            let quarantined = QuarantinedVmGeneration {
                connection_id: String::from("conn-test"),
                session_id: String::from("session-test"),
                vm_id: String::from("vm-test"),
                generation,
                resources: Arc::clone(&resources),
                runtime_context: runtime_context.clone(),
                capabilities: capabilities.clone(),
                reason: VmQuarantineReason::TeardownDeadline,
            };
            assert!(!quarantined.can_reap());

            release_tx.send(()).expect("release task barrier");
            task.await.expect("supervised task joins");
            let (snapshot, deadline_expired) = wait_for_vm_reconciliation(
                resources.as_ref(),
                &runtime_context,
                &capabilities,
                Duration::ZERO,
            )
            .await;
            assert!(!deadline_expired);
            assert!(snapshot.ledger_zero);
            assert!(quarantined.can_reap());
        });
    }

    #[test]
    fn quarantined_generation_is_not_reused_by_successor() {
        let mut sidecar = NativeSidecar::new(LocalBridge::default()).expect("test sidecar");
        sidecar.observe_active_vm_generations();
        let baseline = active_vm_metric(&sidecar);
        let (quarantined_vm_id, generation) =
            sidecar.allocate_vm_identity().expect("allocate generation");
        let (resources, runtime_context, capabilities) = reconciliation_handles(generation);
        let held = resources
            .reserve(ResourceClass::Tasks, 1)
            .expect("hold quarantine accounting open");
        sidecar
            .retain_quarantined_vm(QuarantinedVmGeneration {
                connection_id: String::from("conn-test"),
                session_id: String::from("session-test"),
                vm_id: quarantined_vm_id.clone(),
                generation,
                resources: Arc::clone(&resources),
                runtime_context,
                capabilities,
                reason: VmQuarantineReason::TeardownDeadline,
            })
            .expect("retain quarantined generation");
        assert_eq!(active_vm_metric(&sidecar), baseline + 1);
        sidecar.connections.insert(
            String::from("conn-test"),
            ConnectionState {
                auth_token: String::new(),
                sessions: BTreeSet::from([String::from("session-test")]),
            },
        );
        sidecar.sessions.insert(
            String::from("session-test"),
            SessionState {
                connection_id: String::from("conn-test"),
                placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                    crate::protocol::SidecarPlacementShared { pool: None },
                ),
                metadata: BTreeMap::new(),
                vm_ids: BTreeSet::new(),
            },
        );
        let rejected = sidecar
            .require_owned_vm("conn-test", "session-test", &quarantined_vm_id)
            .expect_err("quarantined generation must reject work");
        assert!(rejected.to_string().contains("ERR_AGENTOS_VM_QUARANTINED"));

        let (successor_id, successor_generation) =
            sidecar.allocate_vm_identity().expect("allocate successor");
        assert!(successor_generation > generation);
        assert_ne!(successor_id, quarantined_vm_id);
        assert!(sidecar.quarantined_vms.contains_key(&generation));

        drop(held);
        sidecar.reap_reconciled_quarantined_vms();
        assert!(!sidecar.quarantined_vms.contains_key(&generation));
        assert_eq!(active_vm_metric(&sidecar), baseline);
    }

    #[test]
    fn vm_unix_socket_host_directories_are_private_and_unique() {
        let first = create_vm_unix_socket_host_dir()
            .expect("first private Unix socket namespace should be created");
        let second = create_vm_unix_socket_host_dir()
            .expect("second private Unix socket namespace should be created");

        assert_ne!(first, second, "VMs must not share a Unix socket namespace");
        for path in [&first, &second] {
            let mode = fs::metadata(path)
                .expect("private Unix socket namespace metadata should be readable")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, 0o700, "private Unix socket namespace must be 0700");
            fs::remove_dir(path).expect("private Unix socket namespace should be removable");
            assert!(
                !path.exists(),
                "removed Unix socket namespace must stay absent"
            );
        }
    }
    #[test]
    fn native_root_config_opens_chunked_local_as_persistent_root() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let database_path =
            std::env::temp_dir().join(format!("agentos-native-root-{unique}.sqlite"));
        let block_root = std::env::temp_dir().join(format!("agentos-native-root-blocks-{unique}"));
        let native_root =
            native_root_plugin_from_config(Some(&agentos_vm_config::NativeRootFilesystemConfig {
                plugin: agentos_vm_config::MountPluginDescriptor {
                    id: "chunked_local".to_string(),
                    config: serde_json::json!({
                        "metadataPath": database_path.to_string_lossy(),
                        "blockRoot": block_root.to_string_lossy(),
                    }),
                },
                read_only: false,
            }))
            .expect("native root config should parse")
            .expect("native root should be present");
        let config: serde_json::Value =
            serde_json::from_str(&native_root.plugin.config).expect("valid plugin config");
        let sidecar = NativeSidecar::new(LocalBridge::default()).expect("test sidecar");
        let mount_context = MountPluginContext {
            bridge: sidecar.bridge.clone(),
            runtime_context: sidecar
                .runtime_context
                .clone()
                .expect("test sidecar runtime context"),
            connection_id: String::from("connection-test"),
            session_id: String::from("session-test"),
            vm_id: String::from("vm-test"),
            sidecar_requests: sidecar.sidecar_requests.clone(),
            database: None,
            max_pread_bytes: None,
        };
        let plugin = ChunkedLocalMountPlugin;
        let mut filesystem = plugin
            .open(OpenFileSystemPluginRequest {
                vm_id: "vm-test",
                guest_path: "/",
                read_only: false,
                config: &config,
                context: &mount_context,
            })
            .expect("sqlite root should open");
        bootstrap_native_root_filesystem(
            filesystem.as_mut(),
            &RootFilesystemDescriptor {
                bootstrap_entries: vec![
                    RootFilesystemEntry {
                        path: "/etc/agentos/boot.txt".to_string(),
                        kind: RootFilesystemEntryKind::File,
                        content: Some("booted".to_string()),
                        ..Default::default()
                    },
                    RootFilesystemEntry {
                        path: CA_CERTIFICATES_SYMLINK_PATH.to_string(),
                        kind: RootFilesystemEntryKind::File,
                        content: Some("custom native cert.pem\n".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        )
        .expect("native root should bootstrap");

        let mut mount_table = MountTable::new_boxed_root(
            filesystem,
            MountOptions::new(native_root.plugin.id.clone()),
        );
        let home = mount_table.stat("/home/agentos").expect("stat guest home");
        assert_eq!(home.mode & 0o7777, 0o2755);
        assert_eq!((home.uid, home.gid), (1000, 1000));
        let workspace = mount_table.stat("/workspace").expect("stat workspace");
        assert_eq!(workspace.mode & 0o7777, 0o755);
        assert_eq!((workspace.uid, workspace.gid), (1000, 1000));
        let root_home = mount_table.stat("/root").expect("stat root home");
        assert_eq!(root_home.mode & 0o7777, 0o711);
        assert_eq!((root_home.uid, root_home.gid), (0, 0));
        assert_eq!(
            mount_table
                .read_file("/etc/agentos/boot.txt")
                .expect("bootstrap file should be readable"),
            b"booted".to_vec()
        );
        assert_eq!(
            mount_table
                .read_file(CA_CERTIFICATES_GUEST_PATH)
                .expect("default CA bundle should be readable from native root"),
            CA_CERTIFICATES_BUNDLE
        );
        assert_eq!(
            mount_table
                .read_file(CA_CERTIFICATES_SYMLINK_PATH)
                .expect("custom regular cert.pem should replace the default symlink"),
            b"custom native cert.pem\n".to_vec()
        );
        assert!(
            !mount_table
                .lstat(CA_CERTIFICATES_SYMLINK_PATH)
                .expect("lstat custom native cert.pem")
                .is_symbolic_link
        );
        mount_table
            .write_file("/home/agentos/persist.txt", b"persisted".to_vec())
            .expect("write through sqlite root should succeed");
        let mut kernel_config = KernelVmConfig::new("vm-test");
        kernel_config.permissions = Permissions::allow_all();
        let mut kernel = KernelVm::new(mount_table, kernel_config);
        kernel
            .write_file("/bin/python", KERNEL_COMMAND_STUB.to_vec())
            .expect("command stub should be writable");
        prune_kernel_command_stub(&mut kernel, "/bin/python")
            .expect("command stub prune should support native roots");
        assert!(
            !kernel.exists("/bin/python").expect("exists should succeed"),
            "stub should be pruned through the mounted root"
        );
        drop(kernel);

        let reopened = plugin
            .open(OpenFileSystemPluginRequest {
                vm_id: "vm-test",
                guest_path: "/",
                read_only: false,
                config: &config,
                context: &mount_context,
            })
            .expect("chunked local root should reopen");
        let mut reopened = MountTable::new_boxed_root(reopened, MountOptions::new("chunked_local"));
        assert_eq!(
            reopened
                .read_file("/home/agentos/persist.txt")
                .expect("persisted file should survive reopen"),
            b"persisted".to_vec()
        );

        let _ = fs::remove_file(database_path);
        let _ = fs::remove_dir_all(block_root);
    }
}
