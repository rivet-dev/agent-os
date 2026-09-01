//! VM lifecycle functions: create, configure, dispose, bootstrap, snapshot.
//!
//! Extracted from service.rs as part of the service.rs split (Step 0a).
//! Contains VM lifecycle methods on NativeSidecar<B> and associated helpers.

use crate::bootstrap::{
    apply_root_filesystem_entry, discover_command_guest_paths, root_snapshot_entries,
    root_snapshot_entry, root_snapshot_from_entries,
};
use crate::bridge::{bridge_permissions, build_mount_plugin_registry, MountPluginContext};
use crate::execution::{sync_process_host_writes_to_kernel, terminate_child_process_tree};
use crate::extension::Extension;
use crate::process_event_broker::ProcessEventBroker;
use crate::protocol::{
    AgentosProjectedAgent, ConfigureVmRequest, CreateLayerRequest, CreateOverlayRequest,
    DisposeReason, EventFrame, ExportSnapshotRequest, ImportSnapshotRequest, LinkPackageRequest,
    ListMountsRequest, MountDescriptor, MountInfo, MountPluginDescriptor, PackageCommands,
    ProjectedCommand, ProvidedCommandsRequest, RootFilesystemDescriptor, RootFilesystemEntry,
    RootFilesystemEntryEncoding, RootFilesystemLowerDescriptor, SealLayerRequest,
    SnapshotRootFilesystemRequest, VmLifecycleState,
};
use crate::request_operations::OperationCancellationReason;
use crate::service::{
    audit_fields, dirname, emit_security_audit_event, emit_structured_event, kernel_error,
    normalize_path, plugin_error, root_filesystem_error, validate_permissions_policy, vfs_error,
};
use crate::state::{
    BridgeError, KernelSocketReadinessEvent, KernelSocketReadinessRegistry,
    KernelSocketReadinessTarget, QuarantinedVmGeneration, VmConfiguration, VmDnsConfig,
    VmExecutionEngines, VmHandle, VmListenPolicy, VmPendingByteBudget, VmQuarantineReason,
    VmReconciliationSnapshot, VmState, DISPOSE_VM_SIGKILL_GRACE, DISPOSE_VM_SIGTERM_GRACE,
    EXECUTION_DRIVER_NAME, JAVASCRIPT_COMMAND, PYTHON_COMMAND, WASM_COMMAND,
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
use agentos_kernel::resource_accounting::ResourceLimits;
use agentos_kernel::root_fs::{
    decode_snapshot_with_import_limits, encode_snapshot as encode_root_snapshot,
    is_supported_root_filesystem_snapshot_format, FilesystemEntryKind as KernelFilesystemEntryKind,
    RootFilesystemImportLimits, ROOT_FILESYSTEM_SNAPSHOT_FORMAT,
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
    vm_created_response, vm_disposed_response, vm_lifecycle_event as shared_vm_lifecycle_event,
    VmLayerStore,
};
use agentos_runtime::accounting::{ResourceClass, ResourceLedger, ResourceLimit};
use agentos_runtime::capability::CapabilityRegistry;
use agentos_vm_config as vm_config;
use base64::Engine;
use openssl::rand::rand_bytes;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    ("/root", 0o700),
    ("/run", 0o755),
    ("/srv", 0o755),
    ("/sys", 0o555),
    ("/opt", 0o755),
    ("/mnt", 0o755),
    ("/media", 0o755),
    ("/home", 0o755),
    ("/home/agentos", 0o2755),
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
    ("/var/empty", 0o555),
    ("/var/lib", 0o755),
    ("/var/lock", 0o777),
    ("/var/log", 0o755),
    ("/var/run", 0o777),
    ("/var/spool", 0o755),
    ("/var/tmp", 0o1777),
    ("/etc/agentos", 0o755),
    // Non-Alpine default agent working directory (also present in the base
    // filesystem snapshot); scaffold it here so it exists even when the
    // default base layer is disabled. It is the default cwd and mount root,
    // kept separate from $HOME (/home/agentos).
    ("/workspace", 0o755),
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
    let flags = match (target.event, readiness.kind) {
        (KernelSocketReadinessEvent::Accept, SocketReadinessKind::Accept) => {
            agentos_runtime::readiness::ReadyFlags::ACCEPT
        }
        (KernelSocketReadinessEvent::Data, SocketReadinessKind::Data) => {
            agentos_runtime::readiness::ReadyFlags::READABLE
        }
        (KernelSocketReadinessEvent::Data, SocketReadinessKind::Hangup) => {
            agentos_runtime::readiness::ReadyFlags::END
        }
        (KernelSocketReadinessEvent::Datagram, SocketReadinessKind::Data) => {
            agentos_runtime::readiness::ReadyFlags::DATAGRAM
        }
        _ => return,
    };
    if let Some(notify) = target.notify {
        notify.notify_one();
    }
    if let Some(session) = target.session {
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

fn projected_commands_from_guest_paths(
    command_guest_paths: &BTreeMap<String, String>,
) -> Vec<ProjectedCommand> {
    command_guest_paths
        .iter()
        .filter(|(_, guest_path)| {
            guest_path.starts_with(crate::package_projection::OPT_AGENTOS_BIN)
        })
        .map(|(name, guest_path)| ProjectedCommand {
            name: name.clone(),
            guest_path: guest_path.clone(),
        })
        .collect()
}

/// Owned request context for work serialized by one VM's lifecycle ordering key.
///
/// Preparing this value performs the central ownership lookup once and clones the
/// per-VM handle. Executing the operation can then happen after the
/// `NativeSidecar` coordinator borrow has ended.
#[derive(Clone)]
pub(crate) struct OwnedVmLifecycleRequest {
    request: crate::protocol::RequestFrame,
    connection_id: String,
    session_id: String,
    vm_id: String,
    vm: VmHandle,
}

/// Cloneable dependencies needed by the owned `ConfigureVm` implementation.
pub(crate) struct ConfigureVmOwnedInput<B> {
    lifecycle: OwnedVmLifecycleRequest,
    bridge: crate::state::SharedBridge<B>,
    snapshot_runtime_context: agentos_runtime::RuntimeContext,
    sidecar_requests: crate::state::SharedSidecarRequestClient,
}

impl<B> Clone for ConfigureVmOwnedInput<B> {
    fn clone(&self) -> Self {
        Self {
            lifecycle: self.lifecycle.clone(),
            bridge: self.bridge.clone(),
            snapshot_runtime_context: self.snapshot_runtime_context.clone(),
            sidecar_requests: self.sidecar_requests.clone(),
        }
    }
}

/// Cloneable dependencies needed by the owned `LinkPackage` implementation.
pub(crate) struct LinkPackageOwnedInput<B> {
    lifecycle: OwnedVmLifecycleRequest,
    bridge: crate::state::SharedBridge<B>,
    sidecar_requests: crate::state::SharedSidecarRequestClient,
}

/// Create work detached from the process coordinator.
///
/// VM identity and resource admission are reserved during preparation. Database
/// resolution, schema migration, filesystem bootstrap, and kernel construction
/// happen when this owned value is executed.
pub(crate) struct PreparedCreateVm<B> {
    request: crate::protocol::RequestFrame,
    payload: crate::protocol::CreateVmRequest,
    connection_id: String,
    session_id: String,
    vm_id: String,
    vm_generation: u64,
    create_config: vm_config::CreateVmConfig,
    root_filesystem: RootFilesystemDescriptor,
    permissions_policy: agentos_vm_config::PermissionsPolicy,
    limits: crate::limits::VmLimits,
    vm_resources: Arc<ResourceLedger>,
    vm_runtime_context: agentos_runtime::RuntimeContext,
    dns: VmDnsConfig,
    listen_policy: VmListenPolicy,
    create_loopback_exempt_ports: BTreeSet<u16>,
    bridge: crate::state::SharedBridge<B>,
    dns_resolver: agentos_kernel::dns::SharedDnsResolver,
    sidecar_requests: crate::state::SharedSidecarRequestClient,
    process_event_notify: Arc<tokio::sync::Notify>,
    extensions: Vec<Arc<dyn Extension>>,
}

/// Fully constructed VM awaiting a short session/registry publication command.
pub(crate) struct CompletedCreateVm<B> {
    request: crate::protocol::RequestFrame,
    connection_id: String,
    session_id: String,
    vm_id: String,
    vm: VmState,
    events: Vec<EventFrame>,
    bridge: crate::state::SharedBridge<B>,
}

/// Validated dispose intent. The stdio owner begins VM disposal and waits for
/// operation drain before converting this plan into [`PreparedDisposeVm`].
pub(crate) struct DisposeVmPlan<B> {
    request: Option<crate::protocol::RequestFrame>,
    connection_id: String,
    session_id: String,
    vm_id: String,
    reason: DisposeReason,
    bridge: crate::state::SharedBridge<B>,
    sidecar_requests: crate::state::SharedSidecarRequestClient,
    process_event_broker: ProcessEventBroker,
}

/// Detached VM teardown payload.
///
/// Construction is a short coordinator command performed only after the
/// ownership coordinator has entered `Closing` and drained VM operations. The
/// value then owns all state needed for bounded teardown and reconciliation, so
/// no lifecycle permit or `&mut NativeSidecar` is retained across its awaits.
pub(crate) struct PreparedDisposeVm<B> {
    plan: DisposeVmPlan<B>,
    vm: VmState,
}

/// Teardown result awaiting short central tracking/quarantine finalization.
pub(crate) struct CompletedDisposeVm {
    request: Option<crate::protocol::RequestFrame>,
    connection_id: String,
    session_id: String,
    vm_id: String,
    events: Vec<EventFrame>,
    quarantine: Option<QuarantinedVmGeneration>,
    result: Result<(), SidecarError>,
}

impl<B> Clone for LinkPackageOwnedInput<B> {
    fn clone(&self) -> Self {
        Self {
            lifecycle: self.lifecycle.clone(),
            bridge: self.bridge.clone(),
            sidecar_requests: self.sidecar_requests.clone(),
        }
    }
}
// ---------------------------------------------------------------------------
// NativeSidecar VM lifecycle methods
// ---------------------------------------------------------------------------

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) fn prepare_vm_lifecycle_request(
        &self,
        request: &crate::protocol::RequestFrame,
    ) -> Result<OwnedVmLifecycleRequest, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        let vm = self.vms.handle(&vm_id).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "VM {vm_id} no longer exists while preparing its lifecycle operation"
            ))
        })?;
        Ok(OwnedVmLifecycleRequest {
            request: request.clone(),
            connection_id,
            session_id,
            vm_id,
            vm,
        })
    }

    pub(crate) fn prepare_configure_vm_request(
        &self,
        request: &crate::protocol::RequestFrame,
    ) -> Result<ConfigureVmOwnedInput<B>, SidecarError> {
        let lifecycle = self.prepare_vm_lifecycle_request(request)?;
        let snapshot_runtime_context = self.runtime_context.as_ref().cloned().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_RUNTIME_UNAVAILABLE: snapshot pre-warm requires RuntimeContext",
            ))
        })?;
        Ok(ConfigureVmOwnedInput {
            lifecycle,
            bridge: self.bridge.clone(),
            snapshot_runtime_context,
            sidecar_requests: self.sidecar_requests.clone(),
        })
    }

    pub(crate) fn prepare_link_package_request(
        &self,
        request: &crate::protocol::RequestFrame,
    ) -> Result<LinkPackageOwnedInput<B>, SidecarError> {
        Ok(LinkPackageOwnedInput {
            lifecycle: self.prepare_vm_lifecycle_request(request)?,
            bridge: self.bridge.clone(),
            sidecar_requests: self.sidecar_requests.clone(),
        })
    }

    pub(crate) fn prepare_create_vm(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: crate::protocol::CreateVmRequest,
    ) -> Result<PreparedCreateVm<B>, SidecarError> {
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
        let limits = crate::limits::vm_limits_from_config(
            create_config.limits.as_ref(),
            self.config.max_frame_bytes,
        )?;
        let dns = vm_dns_config_from_config(create_config.dns.as_ref())?;
        let listen_policy = vm_listen_policy_from_config(create_config.listen.as_ref())?;
        let create_loopback_exempt_ports = create_config
            .loopback_exempt_ports
            .iter()
            .copied()
            .collect();
        let (vm_id, vm_generation) = self.allocate_vm_identity()?;
        let process_runtime_context = self.runtime_context.as_ref().cloned().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_RUNTIME_UNAVAILABLE: VM admission requires RuntimeContext",
            ))
        })?;
        let vm_resources = Arc::new(vm_resource_ledger(
            &vm_id,
            vm_generation,
            &limits,
            Arc::clone(process_runtime_context.resources()),
        )?);
        let vm_runtime_context =
            process_runtime_context.scoped_for_vm(Arc::clone(&vm_resources), vm_generation);

        Ok(PreparedCreateVm {
            request: request.clone(),
            payload,
            connection_id,
            session_id,
            vm_id,
            vm_generation,
            create_config,
            root_filesystem,
            permissions_policy,
            limits,
            vm_resources,
            vm_runtime_context,
            dns,
            listen_policy,
            create_loopback_exempt_ports,
            bridge: self.bridge.clone(),
            dns_resolver: Arc::clone(&self.dns_resolver),
            sidecar_requests: self.sidecar_requests.clone(),
            process_event_notify: Arc::clone(&self.process_event_notify),
            extensions: self.extensions.values().cloned().collect(),
        })
    }

    pub(crate) fn complete_create_vm(
        &mut self,
        completed: CompletedCreateVm<B>,
    ) -> Result<DispatchResult, SidecarError> {
        let CompletedCreateVm {
            request,
            connection_id,
            session_id,
            vm_id,
            vm,
            events,
            bridge,
        } = completed;
        if let Err(error) = self.require_owned_session(&connection_id, &session_id) {
            cleanup_unpublished_vm(&bridge, &vm_id, &vm);
            return Err(error);
        }
        if self.vms.contains_key(&vm_id) {
            cleanup_unpublished_vm(&bridge, &vm_id, &vm);
            return Err(SidecarError::InvalidState(format!(
                "VM {vm_id} already exists during create finalization"
            )));
        }
        let cleanup_cwd = vm.cwd.clone();
        let cleanup_socket_dir = vm.unix_socket_host_dir.clone();
        if let Err(error) = self.vms.insert(vm_id.clone(), vm) {
            cleanup_path(&cleanup_cwd, "unpublished VM shadow root");
            cleanup_path(&cleanup_socket_dir, "unpublished VM Unix socket namespace");
            if let Err(cleanup_error) = bridge.clear_vm_permissions(&vm_id) {
                eprintln!(
                    "ERR_AGENTOS_VM_CREATE_CLEANUP: vm_id={vm_id} phase=permission_reset error={cleanup_error}"
                );
            }
            return Err(error);
        }
        self.sessions
            .get_mut(&session_id)
            .expect("owned session should exist during create finalization")
            .vm_ids
            .insert(vm_id.clone());
        self.observe_active_vm_generations();
        Ok(DispatchResult {
            response: vm_created_response(&request, vm_id),
            events,
        })
    }

    pub(crate) fn prepare_dispose_vm(
        &self,
        request: &crate::protocol::RequestFrame,
        payload: crate::protocol::DisposeVmRequest,
    ) -> Result<DisposeVmPlan<B>, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.prepare_owned_vm_disposal(
            connection_id,
            session_id,
            vm_id,
            payload.reason,
            Some(request.clone()),
        )
    }

    /// Prepare teardown for a VM owned by a non-protocol lifecycle source.
    ///
    /// Extension session cleanup uses this entry point for its bound VMs. It
    /// deliberately has no request envelope: callers complete it with
    /// [`Self::complete_owned_vm_disposal`] and receive only lifecycle events.
    pub(crate) fn prepare_internal_vm_disposal(
        &self,
        connection_id: String,
        session_id: String,
        vm_id: String,
        reason: DisposeReason,
    ) -> Result<DisposeVmPlan<B>, SidecarError> {
        self.prepare_owned_vm_disposal(connection_id, session_id, vm_id, reason, None)
    }

    fn prepare_owned_vm_disposal(
        &self,
        connection_id: String,
        session_id: String,
        vm_id: String,
        reason: DisposeReason,
        request: Option<crate::protocol::RequestFrame>,
    ) -> Result<DisposeVmPlan<B>, SidecarError> {
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        Ok(DisposeVmPlan {
            request,
            connection_id,
            session_id,
            vm_id,
            reason,
            bridge: self.bridge.clone(),
            sidecar_requests: self.sidecar_requests.clone(),
            process_event_broker: self.process_event_broker.clone(),
        })
    }

    /// Detach a VM after ownership coordination has entered `Closing` and all
    /// previously admitted VM operations have drained.
    pub(crate) fn detach_vm_for_disposal(
        &mut self,
        plan: DisposeVmPlan<B>,
    ) -> Result<PreparedDisposeVm<B>, SidecarError> {
        self.require_owned_vm(&plan.connection_id, &plan.session_id, &plan.vm_id)?;
        let cancellation_reason = dispose_cancellation_reason(&plan.reason);
        if let Err(error) = plan.process_event_broker.dispose_vm(
            &plan.connection_id,
            &plan.session_id,
            &plan.vm_id,
            cancellation_reason,
        ) {
            eprintln!(
                "ERR_AGENTOS_PROCESS_EVENT_VM_DISPOSAL: connection_id={} session_id={} vm_id={} error={error}",
                plan.connection_id, plan.session_id, plan.vm_id
            );
        }
        let vm = self
            .vms
            .try_remove(&plan.vm_id, "begin owned dispose")?
            .expect("owned VM should exist during dispose detachment");
        Ok(PreparedDisposeVm { plan, vm })
    }

    pub(crate) fn complete_dispose_vm(
        &mut self,
        completed: CompletedDisposeVm,
    ) -> Result<DispatchResult, SidecarError> {
        let request = completed.request.clone().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "protocol VM disposal completed without a request envelope",
            ))
        })?;
        let vm_id = completed.vm_id.clone();
        let events = self.complete_owned_vm_disposal(completed)?;
        Ok(DispatchResult {
            response: vm_disposed_response(&request, vm_id),
            events,
        })
    }

    /// Finalize a detached VM teardown without constructing a protocol
    /// response. This is the short central-state mutation paired with
    /// [`Self::prepare_internal_vm_disposal`].
    pub(crate) fn complete_owned_vm_disposal(
        &mut self,
        completed: CompletedDisposeVm,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        let CompletedDisposeVm {
            request: _,
            connection_id,
            session_id,
            vm_id,
            mut events,
            quarantine,
            result,
        } = completed;
        self.reclaim_vm_tracking(&session_id, &vm_id);
        if let Some(quarantine) = quarantine {
            self.retain_quarantined_vm(quarantine)?;
        } else {
            self.observe_active_vm_generations();
        }
        result?;
        events.push(shared_vm_lifecycle_event(
            &connection_id,
            &session_id,
            &vm_id,
            VmLifecycleState::Disposed,
        ));
        Ok(events)
    }

    pub(crate) fn allocate_vm_identity(&mut self) -> Result<(String, u64), SidecarError> {
        self.reap_reconciled_quarantined_vms();
        self.ensure_vm_generation_capacity()?;
        let next = self.next_vm_id.checked_add(1).ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_VM_ID_EXHAUSTED: VM id counter overflowed",
            ))
        })?;
        let generation = self
            .runtime_context
            .as_ref()
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "ERR_AGENTOS_RUNTIME_UNAVAILABLE: VM generation allocation requires RuntimeContext",
                ))
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
        let prepared = self.prepare_create_vm(request, payload)?;
        let completed = prepared.execute().await?;
        self.complete_create_vm(completed)
    }

    #[allow(dead_code)]
    async fn create_vm_legacy_impl(
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
        let cwd = create_vm_shadow_root(&vm_id)?;
        let (guest_cwd, host_cwd) = resolve_vm_cwds(create_config.cwd.as_ref(), &cwd)?;
        fs::create_dir_all(&host_cwd)
            .map_err(|error| SidecarError::Io(format!("failed to create VM cwd: {error}")))?;
        let limits = crate::limits::vm_limits_from_config(
            create_config.limits.as_ref(),
            self.config.max_frame_bytes,
        )?;
        let resource_limits = limits.resources.clone();
        let process_runtime_context = self.runtime_context.as_ref().cloned().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_RUNTIME_UNAVAILABLE: VM admission requires RuntimeContext",
            ))
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
        // Sidecar-owned bootstrap work still needs to reconcile command stubs and the root
        // filesystem before the guest-visible policy takes effect.
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
        if native_root.is_none() {
            materialize_shadow_root_snapshot_entries(
                &cwd,
                &root_filesystem,
                loaded_snapshot.as_ref(),
                &resource_limits,
            )?;
        }

        let mut config = KernelVmConfig::new(vm_id.clone());
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
        let command_guest_paths = discover_command_guest_paths(&mut kernel);
        refresh_guest_command_path_env(&mut guest_env, &command_guest_paths);
        let mut execution_commands = vec![
            String::from(JAVASCRIPT_COMMAND),
            String::from(PYTHON_COMMAND),
            // `python3` resolves to the same Pyodide runtime; register it so the
            // guest shell can find `/bin/python3` on PATH (the command resolver
            // already rewrites the alias to `python`).
            String::from("python3"),
            String::from(WASM_COMMAND),
        ];
        if let Some(bootstrap_commands) = &create_config.bootstrap_commands {
            execution_commands.extend(bootstrap_commands.iter().cloned());
        }
        execution_commands.extend(command_guest_paths.keys().cloned());
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                execution_commands,
            ))
            .map_err(kernel_error)?;
        if let Some(root) = kernel.root_filesystem_mut() {
            root.finish_bootstrap();
        }
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
        // Seed the baseline during VM creation. Otherwise a host-side deletion
        // that happens before the first shadow sync has no prior inventory and
        // the deleted kernel entry is resurrected/order-dependent.
        let shadow_sync_inventory = crate::execution::initial_shadow_sync_inventory(&cwd)?;
        let unix_socket_host_dir = create_vm_unix_socket_host_dir()?;
        let pending_stdin_bytes_budget = VmPendingByteBudget::new(
            limits.process.pending_stdin_bytes,
            agentos_bridge::queue_tracker::TrackedLimit::PendingKernelStdinBytes,
        );
        let pending_event_bytes_budget = VmPendingByteBudget::new(
            limits.process.pending_event_bytes,
            agentos_bridge::queue_tracker::TrackedLimit::PendingExecutionEventBytes,
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
                resources: vm_resources,
                execution_engines: VmExecutionEngines::new(
                    vm_id.clone(),
                    vm_runtime_context.clone(),
                    Arc::clone(&self.process_event_notify),
                ),
                runtime_context: vm_runtime_context,
                database,
                capabilities,
                dns,
                listen_policy,
                create_loopback_exempt_ports,
                guest_env,
                requested_runtime: payload.runtime,
                root_filesystem_mode: protocol_root_filesystem_mode(root_filesystem.mode),
                guest_cwd,
                cwd,
                host_cwd,
                kernel,
                kernel_socket_readiness,
                host_net_transfer_descriptions: Arc::new(Mutex::new(BTreeMap::new())),
                loaded_snapshot,
                configuration: VmConfiguration {
                    permissions: permissions_policy,
                    js_runtime: create_config.js_runtime.clone(),
                    ..VmConfiguration::default()
                },
                layers: VmLayerStore::default(),
                command_guest_paths,
                provided_commands: BTreeMap::new(),
                command_permissions: BTreeMap::new(),
                bindings: BTreeMap::new(),
                active_processes: BTreeMap::new(),
                vm_fetch_streams: BTreeMap::new(),
                next_vm_fetch_stream_id: 0,
                executions: BTreeMap::new(),
                execution_processes: BTreeMap::new(),
                execution_retention_wake_deadline_ms: None,
                execution_retention_wake_task: None,
                package_mutation_execution_id: None,
                typescript_compiler_staged: false,
                exited_process_snapshots: VecDeque::new(),
                detached_child_processes: BTreeSet::new(),
                attached_child_event_cursor: 0,
                detached_child_event_cursor: 0,
                signal_states: BTreeMap::new(),
                packages_staging_root: None,
                projected_agent_launch: BTreeMap::new(),
                shadow_sync_inventory,
                unix_address_registry: Arc::new(Mutex::new(BTreeMap::new())),
                unix_socket_host_dir,
            },
        )?;
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
        let plan = self.prepare_dispose_vm(request, payload)?;
        let prepared = self.detach_vm_for_disposal(plan)?;
        let completed = prepared.execute().await;
        self.complete_dispose_vm(completed)
    }

    pub(crate) fn bootstrap_root_filesystem(
        &mut self,
        request: &crate::protocol::RequestFrame,
        entries: Vec<RootFilesystemEntry>,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_vm_lifecycle_request(request);
        async move { bootstrap_root_filesystem_owned(input?, entries).await }
    }

    pub(crate) fn configure_vm(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: ConfigureVmRequest,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_configure_vm_request(request);
        async move { configure_vm_owned(input?, payload) }
    }

    /// Runtime dynamic `linkSoftware`: add one package's tar/current/bin leaf
    /// mounts to the live VM so commands appear under `/opt/agentos/bin`
    /// immediately, with no reboot. Returns the linked command names.
    pub(crate) fn link_package(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: LinkPackageRequest,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_link_package_request(request);
        async move { link_package_owned(input?, payload).await }
    }

    pub(crate) fn provided_commands(
        &mut self,
        request: &crate::protocol::RequestFrame,
        _payload: ProvidedCommandsRequest,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_vm_lifecycle_request(request);
        async move {
            let input = input?;
            let packages = input.vm.try_read("list provided commands", |vm| {
                vm.provided_commands
                    .iter()
                    .map(|(package_name, commands)| PackageCommands {
                        package_name: package_name.clone(),
                        commands: commands.clone(),
                    })
                    .collect()
            })?;
            Ok(DispatchResult {
                response: provided_commands_response(&input.request, packages),
                events: Vec::new(),
            })
        }
    }

    pub(crate) fn create_layer(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: CreateLayerRequest,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_vm_lifecycle_request(request);
        async move { create_layer_owned(input?, payload).await }
    }

    pub(crate) fn seal_layer(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: SealLayerRequest,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_vm_lifecycle_request(request);
        async move { seal_layer_owned(input?, payload).await }
    }

    pub(crate) fn import_snapshot(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: ImportSnapshotRequest,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_vm_lifecycle_request(request);
        async move { import_snapshot_owned(input?, payload).await }
    }

    pub(crate) fn export_snapshot(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: ExportSnapshotRequest,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_vm_lifecycle_request(request);
        async move { export_snapshot_owned(input?, payload).await }
    }

    pub(crate) fn create_overlay(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: CreateOverlayRequest,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_vm_lifecycle_request(request);
        async move { create_overlay_owned(input?, payload).await }
    }

    pub(crate) fn snapshot_root_filesystem(
        &mut self,
        request: &crate::protocol::RequestFrame,
        payload: SnapshotRootFilesystemRequest,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_vm_lifecycle_request(request);
        async move { snapshot_root_filesystem_owned(input?, payload).await }
    }

    pub(crate) fn list_mounts(
        &mut self,
        request: &crate::protocol::RequestFrame,
        _payload: ListMountsRequest,
    ) -> impl std::future::Future<Output = Result<DispatchResult, SidecarError>> + 'static {
        let input = self.prepare_vm_lifecycle_request(request);
        async move {
            let input = input?;
            let mounts = input.vm.try_read("list mounts", |vm| {
                vm.kernel
                    .mounted_filesystems()
                    .into_iter()
                    .map(|mount| MountInfo {
                        path: mount.path,
                        kind: mount.plugin_id,
                        read_only: mount.read_only,
                    })
                    .collect()
            })?;
            Ok(DispatchResult {
                response: mounts_listed_response(&input.request, mounts),
                events: Vec::new(),
            })
        }
    }

    pub(crate) async fn dispose_vm_internal(
        &mut self,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        reason: DisposeReason,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        self.require_owned_vm(connection_id, session_id, vm_id)?;

        let cancellation_reason = match &reason {
            DisposeReason::Requested => OperationCancellationReason::Explicit,
            DisposeReason::ConnectionClosed => OperationCancellationReason::ConnectionClosed,
            DisposeReason::HostShutdown => OperationCancellationReason::Shutdown,
        };
        if let Err(error) = self.process_event_broker.dispose_vm(
            connection_id,
            session_id,
            vm_id,
            cancellation_reason,
        ) {
            eprintln!(
                "ERR_AGENTOS_PROCESS_EVENT_VM_DISPOSAL: connection_id={connection_id} session_id={session_id} vm_id={vm_id} error={error}"
            );
        }

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
        if let Some(mut vm) = self.vms.get_mut(vm_id) {
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
        let (vm_runtime_context, vm_capabilities, vm_generation) = self
            .vms
            .get(vm_id)
            .map(|vm| {
                (
                    vm.runtime_context.clone(),
                    vm.capabilities.clone(),
                    vm.generation,
                )
            })
            .expect("owned VM should exist before disposal");
        let capability_admission_error =
            close_vm_admission(&vm_runtime_context, &vm_capabilities).err();
        if let Some(error) = capability_admission_error.as_ref() {
            eprintln!("ERR_AGENTOS_VM_CAPABILITY_ADMISSION_CLOSE: vm_id={vm_id} error={error}");
        }
        let fairness_retirement_result = retire_vm_fairness(&vm_runtime_context, vm_generation);
        if let Err(error) = fairness_retirement_result.as_ref() {
            eprintln!("ERR_AGENTOS_VM_FAIRNESS_RETIRE: vm_id={vm_id} error={error}");
        }

        // Detach the VM from `self.vms` BEFORE the remaining fallible teardown so
        // no `?` below can leave the registry entry (or any per-VM map) behind.
        let mut vm = self
            .vms
            .try_remove(vm_id, "dispose")?
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
        let _ = shutdown_configured_mounts(&mut vm, &mount_context, "dispose_vm", true);

        // Snapshot/flush/kernel-dispose/permission-reset can each fail; run them
        // in a helper whose result is captured so cleanup below is unconditional.
        let teardown_result = self.finish_vm_teardown(vm_id, &mut vm).await;

        // Reclaim EVERY per-VM tracking entry on EVERY exit path — even when a
        // teardown step above errored. Pre-fix these ran only after the fallible
        // steps' `?`, so any failure stranded the engine/extension maps (H1) and
        // the output-buffer map was never reclaimed at all (M6).
        self.reclaim_vm_tracking(session_id, vm_id);
        let _ = fs::remove_dir_all(&vm.cwd);
        if let Some(staging_root) = vm.packages_staging_root.take() {
            let _ = fs::remove_dir_all(&staging_root);
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
            let mut vm = self
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
                let should_sync_host_writes = process.host_write_dirty_recursive()
                    || !process.clean_host_writes_are_observable_recursive();
                if should_sync_host_writes {
                    sync_process_host_writes_to_kernel(&mut vm, &process)?;
                }
                terminate_child_process_tree(
                    &mut vm.kernel,
                    &mut process,
                    &kernel_readiness,
                    &unix_address_registry,
                );
                process.kernel_handle.finish(137);
                let _ = vm.kernel.wait_and_reap(process.kernel_pid);
                vm.signal_states.remove(&process_id);
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

impl<B> PreparedCreateVm<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) async fn execute(self) -> Result<CompletedCreateVm<B>, SidecarError> {
        let __t = Instant::now();
        let PreparedCreateVm {
            request,
            payload,
            connection_id,
            session_id,
            vm_id,
            vm_generation,
            create_config,
            root_filesystem,
            permissions_policy,
            limits,
            vm_resources,
            vm_runtime_context,
            dns,
            listen_policy,
            create_loopback_exempt_ports,
            bridge,
            dns_resolver,
            sidecar_requests,
            process_event_notify,
            extensions,
        } = self;
        let cwd = create_vm_shadow_root(&vm_id)?;
        let cleanup_bridge = bridge.clone();
        let cleanup_vm_id = vm_id.clone();
        let cleanup_cwd = cwd.clone();
        let result = async move {
            let (guest_cwd, host_cwd) = resolve_vm_cwds(create_config.cwd.as_ref(), &cwd)?;
            fs::create_dir_all(&host_cwd)
                .map_err(|error| SidecarError::Io(format!("failed to create VM cwd: {error}")))?;
            let resource_limits = limits.resources.clone();
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
                    for extension in extensions {
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
            bridge.set_vm_permissions(&vm_id, &permissions_policy)?;
            let permissions = bridge_permissions(bridge.clone(), &vm_id);
            let mut guest_env = filter_env(&vm_id, &create_config.env, &permissions);
            // Bootstrap runs under the trusted sidecar policy, then restores the
            // caller's guest-visible permissions before publication.
            bridge.set_vm_permissions(&vm_id, &allow_all_policy())?;
            let native_root = native_root_plugin_from_config(create_config.native_root.as_ref())?;
            let loaded_snapshot = if native_root.is_some() {
                None
            } else {
                bridge.with_mut(|bridge| {
                    bridge.load_filesystem_state(LoadFilesystemStateRequest {
                        vm_id: vm_id.clone(),
                    })
                })?
            };
            if native_root.is_none() {
                materialize_shadow_root_snapshot_entries(
                    &cwd,
                    &root_filesystem,
                    loaded_snapshot.as_ref(),
                    &resource_limits,
                )?;
            }

            let mut config = KernelVmConfig::new(vm_id.clone());
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
            config.dns_resolver = dns_resolver;
            config.loopback_exempt_ports = create_loopback_exempt_ports.clone();
            let mount_plugins = build_mount_plugin_registry::<B>()?;
            let root_mount_table = if let Some(native_root) = native_root.as_ref() {
                build_native_root_mount_table(
                    &mount_plugins,
                    native_root,
                    &root_filesystem,
                    MountPluginContext {
                        bridge: bridge.clone(),
                        runtime_context: vm_runtime_context.clone(),
                        connection_id: connection_id.clone(),
                        session_id: session_id.clone(),
                        vm_id: vm_id.clone(),
                        sidecar_requests,
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
                crate::state::KernelSocketReadinessRegistryState::new(
                    limits.reactor.max_capabilities,
                ),
            );
            let readiness_targets = Arc::clone(&kernel_socket_readiness);
            kernel.set_socket_readiness_sink(Some(move |readiness: SocketReadiness| {
                for target in readiness_targets.targets(readiness.socket_id) {
                    send_kernel_socket_readiness_event(target, readiness);
                }
            }));
            let command_guest_paths = discover_command_guest_paths(&mut kernel);
            refresh_guest_command_path_env(&mut guest_env, &command_guest_paths);
            let mut execution_commands = vec![
                String::from(JAVASCRIPT_COMMAND),
                String::from(PYTHON_COMMAND),
                String::from("python3"),
                String::from(WASM_COMMAND),
            ];
            if let Some(bootstrap_commands) = &create_config.bootstrap_commands {
                execution_commands.extend(bootstrap_commands.iter().cloned());
            }
            execution_commands.extend(command_guest_paths.keys().cloned());
            kernel
                .register_driver(CommandDriver::new(
                    EXECUTION_DRIVER_NAME,
                    execution_commands,
                ))
                .map_err(kernel_error)?;
            if let Some(root) = kernel.root_filesystem_mut() {
                root.finish_bootstrap();
            }
            bridge.set_vm_permissions(&vm_id, &permissions_policy)?;
            bridge.emit_lifecycle(&vm_id, LifecycleState::Starting)?;
            bridge.emit_lifecycle(&vm_id, LifecycleState::Ready)?;
            bridge.emit_log(
                &vm_id,
                format!("created VM {vm_id} for session {session_id}"),
            )?;

            let shadow_sync_inventory = crate::execution::initial_shadow_sync_inventory(&cwd)?;
            let unix_socket_host_dir = create_vm_unix_socket_host_dir()?;
            let pending_stdin_bytes_budget = VmPendingByteBudget::new(
                limits.process.pending_stdin_bytes,
                agentos_bridge::queue_tracker::TrackedLimit::PendingKernelStdinBytes,
            );
            let pending_event_bytes_budget = VmPendingByteBudget::new(
                limits.process.pending_event_bytes,
                agentos_bridge::queue_tracker::TrackedLimit::PendingExecutionEventBytes,
            );
            let events = vec![
                shared_vm_lifecycle_event(
                    &connection_id,
                    &session_id,
                    &vm_id,
                    VmLifecycleState::Creating,
                ),
                shared_vm_lifecycle_event(
                    &connection_id,
                    &session_id,
                    &vm_id,
                    VmLifecycleState::Ready,
                ),
            ];
            let vm = VmState {
                connection_id: connection_id.clone(),
                session_id: session_id.clone(),
                generation: vm_generation,
                limits,
                pending_stdin_bytes_budget,
                pending_event_bytes_budget,
                resources: vm_resources,
                execution_engines: VmExecutionEngines::new(
                    vm_id.clone(),
                    vm_runtime_context.clone(),
                    process_event_notify,
                ),
                runtime_context: vm_runtime_context,
                database,
                capabilities,
                dns,
                listen_policy,
                create_loopback_exempt_ports,
                guest_env,
                requested_runtime: payload.runtime,
                root_filesystem_mode: protocol_root_filesystem_mode(root_filesystem.mode),
                guest_cwd,
                cwd,
                host_cwd,
                kernel,
                kernel_socket_readiness,
                host_net_transfer_descriptions: Arc::new(Mutex::new(BTreeMap::new())),
                loaded_snapshot,
                configuration: VmConfiguration {
                    permissions: permissions_policy,
                    js_runtime: create_config.js_runtime.clone(),
                    ..VmConfiguration::default()
                },
                layers: VmLayerStore::default(),
                command_guest_paths,
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
                signal_states: BTreeMap::new(),
                packages_staging_root: None,
                projected_agent_launch: BTreeMap::new(),
                shadow_sync_inventory,
                unix_address_registry: Arc::new(Mutex::new(BTreeMap::new())),
                unix_socket_host_dir,
            };
            tracing::info!(target: "agentos_native_sidecar::perf", phase = "create_vm", elapsed_ms = __t.elapsed().as_millis() as u64, "vm phase");
            Ok(CompletedCreateVm {
                request,
                connection_id,
                session_id,
                vm_id,
                vm,
                events,
                bridge,
            })
        }
        .await;
        if result.is_err() {
            cleanup_path(&cleanup_cwd, "failed VM create shadow root");
            if let Err(error) = cleanup_bridge.clear_vm_permissions(&cleanup_vm_id) {
                eprintln!(
                    "ERR_AGENTOS_VM_CREATE_CLEANUP: vm_id={cleanup_vm_id} phase=permission_reset error={error}"
                );
            }
        }
        result
    }
}

impl<B> PreparedDisposeVm<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) async fn execute(mut self) -> CompletedDisposeVm {
        let DisposeVmPlan {
            request,
            connection_id,
            session_id,
            vm_id,
            reason: _,
            bridge,
            sidecar_requests,
            process_event_broker: _,
        } = self.plan;
        let mut events = vec![shared_vm_lifecycle_event(
            &connection_id,
            &session_id,
            &vm_id,
            VmLifecycleState::Disposing,
        )];

        let terminate_result =
            terminate_detached_vm_processes::<B>(&bridge, &vm_id, &mut self.vm).await;
        if let Some(task) = self.vm.execution_retention_wake_task.take() {
            task.abort();
        }
        self.vm.execution_retention_wake_deadline_ms = None;
        for execution in self.vm.executions.values_mut() {
            if let Some(task) = execution.deadline_task.take() {
                task.abort();
            }
        }

        let capability_admission_error =
            close_vm_admission(&self.vm.runtime_context, &self.vm.capabilities).err();
        if let Some(error) = capability_admission_error.as_ref() {
            eprintln!("ERR_AGENTOS_VM_CAPABILITY_ADMISSION_CLOSE: vm_id={vm_id} error={error}");
        }
        let fairness_retirement_result =
            retire_vm_fairness(&self.vm.runtime_context, self.vm.generation);
        if let Err(error) = fairness_retirement_result.as_ref() {
            eprintln!("ERR_AGENTOS_VM_FAIRNESS_RETIRE: vm_id={vm_id} error={error}");
        }

        let mount_context = MountPluginContext {
            bridge: bridge.clone(),
            runtime_context: self.vm.runtime_context.clone(),
            connection_id: connection_id.clone(),
            session_id: session_id.clone(),
            vm_id: vm_id.clone(),
            sidecar_requests,
            database: self.vm.database.clone(),
            max_pread_bytes: self.vm.kernel.resource_limits().max_pread_bytes,
        };
        if let Err(error) =
            shutdown_configured_mounts(&mut self.vm, &mount_context, "dispose_vm", true)
        {
            eprintln!(
                "ERR_AGENTOS_VM_TEARDOWN_CLEANUP: vm_id={vm_id} phase=mount_shutdown error={error}"
            );
        }
        let teardown_result = finish_vm_teardown_owned(&bridge, &vm_id, &mut self.vm);

        cleanup_path(&self.vm.cwd, "disposed VM shadow root");
        if let Some(staging_root) = self.vm.packages_staging_root.take() {
            cleanup_path(&staging_root, "disposed VM package staging root");
        }
        cleanup_path(
            &self.vm.unix_socket_host_dir,
            "disposed VM Unix socket namespace",
        );

        let shutdown_deadline = Duration::from_millis(self.vm.limits.reactor.shutdown_deadline_ms);
        let (reconciliation, deadline_expired) = wait_for_vm_reconciliation(
            self.vm.resources.as_ref(),
            &self.vm.runtime_context,
            &self.vm.capabilities,
            shutdown_deadline,
        )
        .await;
        let quarantine_reason = vm_quarantine_reason(
            capability_admission_error.is_some(),
            fairness_retirement_result.is_err(),
            reconciliation,
            deadline_expired,
        );

        let (quarantine, result) = if let Some(reason) = quarantine_reason {
            let diagnostic = quarantine_diagnostic(
                &vm_id,
                &self.vm,
                reconciliation,
                reason.clone(),
                capability_admission_error.as_deref(),
                fairness_retirement_result.as_ref().err(),
            );
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
            (
                Some(QuarantinedVmGeneration {
                    connection_id: connection_id.clone(),
                    session_id: session_id.clone(),
                    vm_id: vm_id.clone(),
                    generation: self.vm.generation,
                    resources: Arc::clone(&self.vm.resources),
                    runtime_context: self.vm.runtime_context.clone(),
                    capabilities: self.vm.capabilities.clone(),
                    reason,
                }),
                Err(SidecarError::Execution(diagnostic)),
            )
        } else {
            let result = fairness_retirement_result
                .and(terminate_result)
                .and(teardown_result);
            (None, result)
        };

        CompletedDisposeVm {
            request,
            connection_id,
            session_id,
            vm_id,
            events: std::mem::take(&mut events),
            quarantine,
            result,
        }
    }
}

async fn terminate_detached_vm_processes<B>(
    bridge: &crate::state::SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let mut first_error = None;
    let process_ids = vm.active_processes.keys().cloned().collect::<Vec<_>>();
    for process_id in &process_ids {
        if let Err(error) =
            NativeSidecar::<B>::kill_process_in_vm(bridge, vm, vm_id, process_id, "SIGTERM")
        {
            record_vm_teardown_error(vm_id, "sigterm", error, &mut first_error);
        }
    }
    if !process_ids.is_empty() {
        tokio::time::sleep(DISPOSE_VM_SIGTERM_GRACE).await;
    }
    for process_id in &process_ids {
        if let Err(error) =
            NativeSidecar::<B>::kill_process_in_vm(bridge, vm, vm_id, process_id, "SIGKILL")
        {
            record_vm_teardown_error(vm_id, "sigkill", error, &mut first_error);
        }
    }
    if !process_ids.is_empty() {
        tokio::time::sleep(DISPOSE_VM_SIGKILL_GRACE).await;
    }

    let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
    let unix_address_registry = Arc::clone(&vm.unix_address_registry);
    let remaining = std::mem::take(&mut vm.active_processes);
    if !remaining.is_empty() {
        eprintln!(
            "ERR_AGENTOS_VM_FORCED_PROCESS_FINALIZE: vm_id={vm_id} process_count={}",
            remaining.len()
        );
    }
    for (process_id, mut process) in remaining {
        let should_sync_host_writes = process.host_write_dirty_recursive()
            || !process.clean_host_writes_are_observable_recursive();
        if should_sync_host_writes {
            if let Err(error) = sync_process_host_writes_to_kernel(vm, &process) {
                record_vm_teardown_error(vm_id, "process_host_write_sync", error, &mut first_error);
            }
        }
        terminate_child_process_tree(
            &mut vm.kernel,
            &mut process,
            &kernel_readiness,
            &unix_address_registry,
        );
        process.kernel_handle.finish(137);
        if let Err(error) = vm.kernel.wait_and_reap(process.kernel_pid) {
            record_vm_teardown_error(vm_id, "process_reap", kernel_error(error), &mut first_error);
        }
        vm.signal_states.remove(&process_id);
    }
    first_error.map_or(Ok(()), Err)
}

fn finish_vm_teardown_owned<B>(
    bridge: &crate::state::SharedBridge<B>,
    vm_id: &str,
    vm: &mut VmState,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
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
    if let Err(error) = bridge.emit_lifecycle(vm_id, LifecycleState::Terminated) {
        record_vm_teardown_error(vm_id, "lifecycle", error, &mut first_error);
    }
    if let Err(error) = vm.kernel.dispose().map_err(kernel_error) {
        record_vm_teardown_error(vm_id, "kernel", error, &mut first_error);
    }
    if let Some(snapshot) = snapshot {
        if let Err(error) = bridge.with_mut(|bridge| {
            bridge.flush_filesystem_state(FlushFilesystemStateRequest {
                vm_id: vm_id.to_owned(),
                snapshot,
            })
        }) {
            record_vm_teardown_error(vm_id, "filesystem_flush", error, &mut first_error);
        }
    }
    if let Err(error) = bridge.clear_vm_permissions(vm_id) {
        record_vm_teardown_error(vm_id, "permission_reset", error, &mut first_error);
    }
    first_error.map_or(Ok(()), Err)
}

fn quarantine_diagnostic(
    vm_id: &str,
    vm: &VmState,
    reconciliation: VmReconciliationSnapshot,
    reason: VmQuarantineReason,
    capability_admission_error: Option<&str>,
    fairness_retirement_error: Option<&SidecarError>,
) -> String {
    match reason {
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
            capability_admission_error.unwrap_or("unknown")
        ),
        VmQuarantineReason::FairnessIntegrity => format!(
            "ERR_AGENTOS_VM_FAIRNESS_INTEGRITY: vm_id={vm_id} generation={} fairness membership could not be retired; generation cannot be reaped; error={}",
            vm.generation,
            fairness_retirement_error
                .map(ToString::to_string)
                .unwrap_or_else(|| String::from("unknown"))
        ),
    }
}

fn dispose_cancellation_reason(reason: &DisposeReason) -> OperationCancellationReason {
    match reason {
        DisposeReason::Requested => OperationCancellationReason::Explicit,
        DisposeReason::ConnectionClosed => OperationCancellationReason::ConnectionClosed,
        DisposeReason::HostShutdown => OperationCancellationReason::Shutdown,
    }
}

fn cleanup_unpublished_vm<B>(bridge: &crate::state::SharedBridge<B>, vm_id: &str, vm: &VmState)
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    cleanup_path(&vm.cwd, "unpublished VM shadow root");
    cleanup_path(
        &vm.unix_socket_host_dir,
        "unpublished VM Unix socket namespace",
    );
    if let Err(error) = bridge.clear_vm_permissions(vm_id) {
        eprintln!(
            "ERR_AGENTOS_VM_CREATE_CLEANUP: vm_id={vm_id} phase=permission_reset error={error}"
        );
    }
}

fn cleanup_path(path: &Path, label: &str) {
    if let Err(error) = fs::remove_dir_all(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "ERR_AGENTOS_VM_PATH_CLEANUP: label={label:?} path={} error={error}",
                path.display()
            );
        }
    }
}

pub(crate) async fn bootstrap_root_filesystem_owned(
    input: OwnedVmLifecycleRequest,
    entries: Vec<RootFilesystemEntry>,
) -> Result<DispatchResult, SidecarError> {
    input.vm.try_command("bootstrap root filesystem", |vm| {
        let root = vm.kernel.root_filesystem_mut().ok_or_else(|| {
            SidecarError::InvalidState(String::from("VM root filesystem is unavailable"))
        })?;
        for entry in &entries {
            apply_root_filesystem_entry(root, entry)?;
        }
        Ok(())
    })?;

    Ok(DispatchResult {
        response: root_filesystem_bootstrapped_response(&input.request, entries.len() as u32),
        events: Vec::new(),
    })
}

pub(crate) fn configure_vm_owned<B>(
    input: ConfigureVmOwnedInput<B>,
    payload: ConfigureVmRequest,
) -> Result<DispatchResult, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let __t = Instant::now();
    let ConfigureVmOwnedInput {
        lifecycle,
        bridge,
        snapshot_runtime_context,
        sidecar_requests,
    } = input;
    let OwnedVmLifecycleRequest {
        request,
        connection_id,
        session_id,
        vm_id,
        vm,
    } = lifecycle;

    let original_permissions = vm.try_read("read configure VM permissions", |vm| {
        vm.configuration.permissions.clone()
    })?;
    let configured_permissions = payload
        .permissions
        .clone()
        .map(crate::wire::permissions_policy_config_from_wire)
        .unwrap_or_else(|| original_permissions.clone());
    validate_permissions_policy(&configured_permissions)?;

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
    append_package_provides_mounts(&mut effective_mounts, &package_descriptors)?;
    let mount_plugins = build_mount_plugin_registry::<B>()?;

    bridge.set_vm_permissions(&vm_id, &allow_all_policy())?;
    let reconfigure_result = vm.try_command("configure VM", |vm| {
        apply_package_provides_env(&mut vm.guest_env, &package_descriptors);
        let mount_context = MountPluginContext {
            bridge: bridge.clone(),
            runtime_context: vm.runtime_context.clone(),
            connection_id: connection_id.clone(),
            session_id: session_id.clone(),
            vm_id: vm_id.clone(),
            sidecar_requests,
            database: vm.database.clone(),
            max_pread_bytes: vm.kernel.resource_limits().max_pread_bytes,
        };
        reconcile_mounts(&mount_plugins, vm, &effective_mounts, mount_context)?;

        vm.command_guest_paths = discover_command_guest_paths(&mut vm.kernel);
        // Package command leaves live under `/opt/agentos/bin`, outside the
        // legacy discovery tree. Register them explicitly for both absolute
        // and PATH-based execution.
        for commands in provided_commands.values() {
            for command in commands {
                vm.command_guest_paths
                    .entry(command.clone())
                    .or_insert_with(|| projected_command_guest_path(command));
            }
        }
        let command_guest_paths = vm.command_guest_paths.clone();
        refresh_guest_command_path_env(&mut vm.guest_env, &command_guest_paths);
        let mut execution_commands =
            vec![String::from(JAVASCRIPT_COMMAND), String::from(WASM_COMMAND)];
        execution_commands.extend(payload.bootstrap_commands.iter().cloned());
        execution_commands.extend(payload.binding_shim_commands.iter().cloned());
        execution_commands.extend(vm.command_guest_paths.keys().cloned());
        vm.kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                execution_commands,
            ))
            .map_err(kernel_error)?;
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
        vm.provided_commands = provided_commands.clone();
        let projected_commands = projected_commands_from_guest_paths(&vm.command_guest_paths);
        vm.projected_agent_launch = projected_agent_launch_from_descriptors(&package_descriptors);
        Ok(projected_commands)
    });

    let projected_commands = match reconfigure_result {
        Ok(projected_commands) => {
            bridge.set_vm_permissions(&vm_id, &configured_permissions)?;
            projected_commands
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
                    vm.try_command("configure VM fail-closed rollback", |vm| {
                        vm.configuration.permissions = deny_all_policy();
                        Ok(())
                    })?;
                    return Err(rollback_error);
                }
            }
        }
    };

    let applied_mounts = effective_mounts.len() as u32;
    let configured_software = payload.software.len() as u32;
    let agents = projected_agents_from_descriptors(&package_descriptors);

    // No VM state borrow survives this point. Pre-warm continues as a
    // supervised runtime task so lifecycle ordering is released immediately;
    // its own bounded blocking admission and result remain fully observable.
    if let Some(userland) = snapshot_userland_code {
        let requested_bytes = userland.len();
        let runtime_for_task = snapshot_runtime_context.clone();
        if let Err(error) = snapshot_runtime_context.spawn(
            agentos_runtime::TaskClass::Runtime,
            async move {
                let runtime_for_job = runtime_for_task.clone();
                match runtime_for_task
                    .blocking()
                    .run(requested_bytes, move || {
                        agentos_execution::v8_host::pre_warm_agent_snapshot(
                            &runtime_for_job,
                            &userland,
                        )
                    })
                    .await
                {
                    Ok(Ok(())) => tracing::debug!(
                        target: "agentos_native_sidecar::vm",
                        "agent snapshot pre-warm completed"
                    ),
                    Ok(Err(error)) => eprintln!(
                        "ERR_AGENTOS_SNAPSHOT_PREWARM: snapshot build failed: {error}"
                    ),
                    Err(error) => eprintln!(
                        "ERR_AGENTOS_SNAPSHOT_PREWARM_ADMISSION: blocking admission or execution failed: {error}"
                    ),
                }
            },
        ) {
            eprintln!(
                "ERR_AGENTOS_SNAPSHOT_PREWARM_TASK_ADMISSION: supervised task admission failed: {error}"
            );
        }
    }

    tracing::info!(target: "agentos_native_sidecar::perf", phase = "configure_vm", elapsed_ms = __t.elapsed().as_millis() as u64, applied_mounts = applied_mounts as u64, "vm phase");
    Ok(DispatchResult {
        response: vm_configured_response(
            &request,
            applied_mounts,
            configured_software,
            projected_commands,
            agents,
        ),
        events: Vec::new(),
    })
}

pub(crate) async fn link_package_owned<B>(
    input: LinkPackageOwnedInput<B>,
    payload: LinkPackageRequest,
) -> Result<DispatchResult, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let LinkPackageOwnedInput {
        lifecycle,
        bridge,
        sidecar_requests,
    } = input;
    let OwnedVmLifecycleRequest {
        request,
        connection_id,
        session_id,
        vm_id,
        vm,
    } = lifecycle;
    let descriptor =
        crate::package_projection::read_package_manifest_from_path(&payload.package.path)?;
    let new_mounts = build_packages_projection(
        &vm_id,
        std::slice::from_ref(&descriptor),
        crate::package_projection::OPT_AGENTOS_ROOT,
    )?;
    let commands = descriptor
        .commands
        .iter()
        .map(|target| target.command.clone())
        .collect::<Vec<_>>();
    let mount_plugins = build_mount_plugin_registry::<B>()?;

    vm.try_command("link VM package", |vm| {
        if new_mounts.iter().all(|mount| {
            vm.configuration
                .mounts
                .iter()
                .any(|existing| existing.guest_path == mount.guest_path)
        }) {
            return Ok(());
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
            bridge,
            runtime_context: vm.runtime_context.clone(),
            connection_id,
            session_id,
            vm_id: vm_id.clone(),
            sidecar_requests,
            database: vm.database.clone(),
            max_pread_bytes: vm.kernel.resource_limits().max_pread_bytes,
        };
        mount_leaf_descriptors(&mount_plugins, vm, &new_mounts, mount_context)?;
        vm.configuration.mounts.extend(new_mounts);
        vm.provided_commands
            .insert(descriptor.name.clone(), commands.clone());
        vm.configuration
            .provided_commands
            .insert(descriptor.name.clone(), commands.clone());
        for command in &commands {
            vm.command_guest_paths
                .entry(command.clone())
                .or_insert_with(|| projected_command_guest_path(command));
        }
        let command_guest_paths = vm.command_guest_paths.clone();
        refresh_guest_command_path_env(&mut vm.guest_env, &command_guest_paths);
        let mut execution_commands =
            vec![String::from(JAVASCRIPT_COMMAND), String::from(WASM_COMMAND)];
        execution_commands.extend(vm.command_guest_paths.keys().cloned());
        vm.kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                execution_commands,
            ))
            .map_err(kernel_error)?;
        vm.projected_agent_launch
            .extend(projected_agent_launch_from_descriptors(
                std::slice::from_ref(&descriptor),
            ));
        Ok(())
    })?;

    let projected_commands = commands
        .iter()
        .map(|command| ProjectedCommand {
            name: command.clone(),
            guest_path: projected_command_guest_path(command),
        })
        .collect();
    let agents = projected_agents_from_descriptors(std::slice::from_ref(&descriptor));
    Ok(DispatchResult {
        response: package_linked_response(&request, projected_commands, agents),
        events: Vec::new(),
    })
}

pub(crate) async fn create_layer_owned(
    input: OwnedVmLifecycleRequest,
    _payload: CreateLayerRequest,
) -> Result<DispatchResult, SidecarError> {
    let layer_id = input.vm.try_command("create VM layer", |vm| {
        vm.layers
            .create_writable_layer()
            .map_err(sidecar_core_error)
    })?;
    Ok(DispatchResult {
        response: layer_created_response(&input.request, layer_id),
        events: Vec::new(),
    })
}

pub(crate) async fn seal_layer_owned(
    input: OwnedVmLifecycleRequest,
    payload: SealLayerRequest,
) -> Result<DispatchResult, SidecarError> {
    let layer_id = input.vm.try_command("seal VM layer", |vm| {
        vm.layers
            .seal_layer(&payload.layer_id)
            .map_err(sidecar_core_error)
    })?;
    Ok(DispatchResult {
        response: layer_sealed_response(&input.request, layer_id),
        events: Vec::new(),
    })
}

pub(crate) async fn import_snapshot_owned(
    input: OwnedVmLifecycleRequest,
    payload: ImportSnapshotRequest,
) -> Result<DispatchResult, SidecarError> {
    let snapshot = root_snapshot_from_entries(&payload.entries)?;
    let layer_id = input.vm.try_command("import VM snapshot", |vm| {
        vm.layers
            .import_snapshot(snapshot)
            .map_err(sidecar_core_error)
    })?;
    Ok(DispatchResult {
        response: snapshot_imported_response(&input.request, layer_id),
        events: Vec::new(),
    })
}

pub(crate) async fn export_snapshot_owned(
    input: OwnedVmLifecycleRequest,
    payload: ExportSnapshotRequest,
) -> Result<DispatchResult, SidecarError> {
    let snapshot = input.vm.try_command("export VM snapshot", |vm| {
        vm.layers
            .export_snapshot(&payload.layer_id)
            .map_err(sidecar_core_error)
    })?;
    Ok(DispatchResult {
        response: snapshot_exported_response(
            &input.request,
            payload.layer_id,
            root_snapshot_entries(&snapshot),
        ),
        events: Vec::new(),
    })
}

pub(crate) async fn create_overlay_owned(
    input: OwnedVmLifecycleRequest,
    payload: CreateOverlayRequest,
) -> Result<DispatchResult, SidecarError> {
    let layer_id = input.vm.try_command("create VM overlay", |vm| {
        vm.layers
            .create_overlay_layer(
                protocol_root_filesystem_mode(payload.mode),
                payload.upper_layer_id,
                payload.lower_layer_ids,
            )
            .map_err(sidecar_core_error)
    })?;
    Ok(DispatchResult {
        response: overlay_created_response(&input.request, layer_id),
        events: Vec::new(),
    })
}

pub(crate) async fn snapshot_root_filesystem_owned(
    input: OwnedVmLifecycleRequest,
    payload: SnapshotRootFilesystemRequest,
) -> Result<DispatchResult, SidecarError> {
    let snapshot = input.vm.try_command("snapshot VM root filesystem", |vm| {
        vm.kernel
            .snapshot_root_filesystem_bounded(payload.max_bytes)
            .map_err(kernel_error)
    })?;
    Ok(DispatchResult {
        response: root_filesystem_snapshot_response(
            &input.request,
            snapshot.entries.iter().map(root_snapshot_entry).collect(),
        ),
        events: Vec::new(),
    })
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
    runtime_context: &agentos_runtime::RuntimeContext,
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
    runtime_context: &agentos_runtime::RuntimeContext,
    capabilities: &CapabilityRegistry,
) -> Result<(), String> {
    let capability_result = capabilities
        .close_admission()
        .map_err(|error| error.to_string());
    runtime_context.close_admission();
    capability_result
}

fn retire_vm_fairness(
    runtime_context: &agentos_runtime::RuntimeContext,
    vm_generation: u64,
) -> Result<(), SidecarError> {
    runtime_context
        .fairness()
        .retire_vm(vm_generation)
        .map(|_| ())
        .map_err(|error| {
            SidecarError::Execution(format!(
                "ERR_AGENTOS_FAIRNESS_RETIRE_VM: generation={vm_generation}: {error}"
            ))
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
    runtime_context: &agentos_runtime::RuntimeContext,
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
    for (guest_path, mode) in SHADOW_ROOT_BOOTSTRAP_DIRS {
        filesystem.mkdir(guest_path, true).map_err(vfs_error)?;
        let (uid, gid) = match *guest_path {
            "/home/agentos" | "/workspace" => (1000, 1000),
            _ => (0, 0),
        };
        filesystem.chown(guest_path, uid, gid).map_err(vfs_error)?;
        filesystem.chmod(guest_path, *mode).map_err(vfs_error)?;
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
                    .max_inodes(max_inode_count),
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
                let _ = emit_structured_event(
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
                );

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
    let root = std::env::temp_dir().join(format!("agentos-native-sidecar-shadow-{vm_id}-{nonce}"));
    fs::create_dir_all(&root)
        .map_err(|error| SidecarError::Io(format!("failed to create VM shadow root: {error}")))?;
    initialize_vm_shadow_root(root)
}

fn initialize_vm_shadow_root(root: PathBuf) -> Result<PathBuf, SidecarError> {
    let cleanup_root = root.clone();
    // macOS: `std::env::temp_dir()` lives under `/var/folders/…`, but `/var` is a
    // symlink to `/private/var`, and macOS fd→path recovery (`fcntl(F_GETPATH)`)
    // reports the resolved `/private/var/…` form. Canonicalize the shadow root up
    // front so the stored host-root matches those resolved paths; otherwise the
    // mapped-runtime confinement prefix checks (`strip_prefix(host_root)`) reject
    // every child and guest `readdir` of a populated dir returns empty. host_dir
    // mounts already canonicalize their root for the same reason.
    let initialized = (|| {
        #[cfg(target_os = "macos")]
        let root = fs::canonicalize(&root).map_err(|error| {
            SidecarError::Io(format!("failed to canonicalize VM shadow root: {error}"))
        })?;
        bootstrap_shadow_root(&root)?;
        Ok(root)
    })();

    match initialized {
        Ok(root) => Ok(root),
        Err(error) => match fs::remove_dir_all(&cleanup_root) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(SidecarError::Io(format!(
                "{error}; additionally failed to clean shadow root {}: {cleanup_error}",
                cleanup_root.display()
            ))),
        },
    }
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
                "failed to set shadow directory mode {mode:o} on {}: {error}",
                host_path.display()
            ))
        })?;
    }
    seed_ca_certificates_bundle(root)?;
    Ok(())
}

/// Seed the Mozilla CA bundle into the shadow root at
/// `/etc/ssl/certs/ca-certificates.crt` (plus the conventional
/// `/etc/ssl/cert.pem` symlink) so guest TLS clients resolve trust the standard
/// Linux way.
fn seed_ca_certificates_bundle(root: &Path) -> Result<(), SidecarError> {
    if CA_CERTIFICATES_BUNDLE.is_empty() {
        return Err(SidecarError::Io(
            "embedded Mozilla CA certificate bundle is empty".to_string(),
        ));
    }

    let bundle_path = shadow_path_for_guest(root, CA_CERTIFICATES_GUEST_PATH);
    if let Some(parent) = bundle_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!(
                "failed to create shadow CA certs directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    match fs::symlink_metadata(&bundle_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::write(&bundle_path, CA_CERTIFICATES_BUNDLE).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to seed CA bundle {}: {error}",
                    bundle_path.display()
                ))
            })?;
            fs::set_permissions(&bundle_path, fs::Permissions::from_mode(0o644)).map_err(
                |error| {
                    SidecarError::Io(format!(
                        "failed to set CA bundle mode on {}: {error}",
                        bundle_path.display()
                    ))
                },
            )?;
        }
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to inspect shadow CA bundle {}: {error}",
                bundle_path.display()
            )));
        }
    }

    let symlink_path = shadow_path_for_guest(root, CA_CERTIFICATES_SYMLINK_PATH);
    match fs::symlink_metadata(&symlink_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::os::unix::fs::symlink(CA_CERTIFICATES_SYMLINK_TARGET, &symlink_path).map_err(
                |error| {
                    SidecarError::Io(format!(
                        "failed to seed CA bundle symlink {}: {error}",
                        symlink_path.display()
                    ))
                },
            )?;
        }
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to inspect shadow CA bundle symlink {}: {error}",
                symlink_path.display()
            )));
        }
    }
    Ok(())
}

fn materialize_shadow_root_snapshot_entries(
    shadow_root: &Path,
    descriptor: &RootFilesystemDescriptor,
    loaded_snapshot: Option<&FilesystemSnapshot>,
    resource_limits: &ResourceLimits,
) -> Result<(), SidecarError> {
    let import_limits = RootFilesystemImportLimits::from_resource_limits(resource_limits);
    if let Some(snapshot) = loaded_snapshot
        .filter(|snapshot| is_supported_root_filesystem_snapshot_format(&snapshot.format))
        .map(|snapshot| {
            decode_snapshot_with_import_limits(&snapshot.bytes, &import_limits)
                .map_err(root_filesystem_error)
        })
        .transpose()?
    {
        materialize_shadow_entries(shadow_root, &root_snapshot_entries(&snapshot))?;
        materialize_shadow_entries(shadow_root, &descriptor.bootstrap_entries)?;
        return Ok(());
    }

    validate_shadow_descriptor_import_limits(descriptor, &import_limits)?;
    for lower in &descriptor.lowers {
        if let RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(inner) = lower {
            materialize_shadow_entries(shadow_root, &inner.entries)?;
        }
    }
    materialize_shadow_entries(shadow_root, &descriptor.bootstrap_entries)?;
    Ok(())
}

fn validate_shadow_descriptor_import_limits(
    descriptor: &RootFilesystemDescriptor,
    limits: &RootFilesystemImportLimits,
) -> Result<(), SidecarError> {
    let mut explicit_entry_count = descriptor.bootstrap_entries.len();
    let mut inode_paths = BTreeSet::new();
    collect_root_protocol_entry_paths(&descriptor.bootstrap_entries, &mut inode_paths);
    let mut bytes = root_protocol_entry_content_bytes(&descriptor.bootstrap_entries)?;

    for lower in &descriptor.lowers {
        match lower {
            RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(inner) => {
                let entries = &inner.entries;
                explicit_entry_count = explicit_entry_count.saturating_add(entries.len());
                collect_root_protocol_entry_paths(entries, &mut inode_paths);
                bytes = bytes.saturating_add(root_protocol_entry_content_bytes(entries)?);
            }
            RootFilesystemLowerDescriptor::BundledBaseFilesystemLower => {}
        }
    }

    if let Some(limit) = limits.max_inode_count {
        if explicit_entry_count > limit {
            return Err(root_filesystem_error(format!(
                "root filesystem descriptor contains {explicit_entry_count} entries, exceeding limit {limit}"
            )));
        }

        let entry_count = inode_paths.len();
        if entry_count > limit {
            return Err(root_filesystem_error(format!(
                "root filesystem descriptor contains {entry_count} entries, exceeding limit {limit}"
            )));
        }
    }

    if let Some(limit) = limits.max_filesystem_bytes {
        if bytes > limit {
            return Err(root_filesystem_error(format!(
                "root filesystem descriptor contains {bytes} bytes, exceeding limit {limit}"
            )));
        }
    }

    Ok(())
}

fn collect_root_protocol_entry_paths(
    entries: &[RootFilesystemEntry],
    paths: &mut BTreeSet<String>,
) {
    for entry in entries {
        collect_root_protocol_path(&entry.path, paths);
    }
}

fn collect_root_protocol_path(path: &str, paths: &mut BTreeSet<String>) {
    let normalized = normalize_guest_path(path);
    paths.insert(normalized.clone());

    let mut parent = String::new();
    let segments = normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    for segment in segments.iter().take(segments.len().saturating_sub(1)) {
        parent.push('/');
        parent.push_str(segment);
        paths.insert(parent.clone());
    }
}

fn root_protocol_entry_content_bytes(entries: &[RootFilesystemEntry]) -> Result<u64, SidecarError> {
    entries.iter().try_fold(0_u64, |total, entry| {
        let bytes = match entry.kind {
            crate::protocol::RootFilesystemEntryKind::Directory => 0,
            crate::protocol::RootFilesystemEntryKind::File => {
                root_protocol_file_content_bytes(entry)?
            }
            crate::protocol::RootFilesystemEntryKind::Symlink => entry
                .target
                .as_ref()
                .map(|target| usize_to_u64(target.len()))
                .unwrap_or(0),
        };
        Ok(total.saturating_add(bytes))
    })
}

fn root_protocol_file_content_bytes(entry: &RootFilesystemEntry) -> Result<u64, SidecarError> {
    let Some(content) = entry.content.as_deref() else {
        return Ok(0);
    };

    let bytes = match entry
        .encoding
        .clone()
        .unwrap_or(RootFilesystemEntryEncoding::Utf8)
    {
        RootFilesystemEntryEncoding::Utf8 => content.len(),
        RootFilesystemEntryEncoding::Base64 => estimated_base64_decoded_len(content),
    };
    Ok(usize_to_u64(bytes))
}

fn estimated_base64_decoded_len(content: &str) -> usize {
    let padding = content
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count()
        .min(2);
    content
        .len()
        .div_ceil(4)
        .saturating_mul(3)
        .saturating_sub(padding)
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn materialize_shadow_entries(
    shadow_root: &Path,
    entries: &[RootFilesystemEntry],
) -> Result<(), SidecarError> {
    let mut ordered = entries.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|entry| {
        let depth = entry.path.matches('/').count();
        let kind_rank = match entry.kind {
            crate::protocol::RootFilesystemEntryKind::Directory => 0,
            crate::protocol::RootFilesystemEntryKind::File => 1,
            crate::protocol::RootFilesystemEntryKind::Symlink => 2,
        };
        (kind_rank, depth, entry.path.as_str())
    });

    for entry in ordered {
        let shadow_path = shadow_path_for_guest(shadow_root, &entry.path);
        if let Some(parent) = shadow_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to create shadow parent for {}: {error}",
                    entry.path
                ))
            })?;
        }
        prepare_shadow_destination(&shadow_path, &entry.kind, &entry.path)?;

        match entry.kind {
            crate::protocol::RootFilesystemEntryKind::Directory => {
                fs::create_dir_all(&shadow_path).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to materialize shadow directory {}: {error}",
                        entry.path
                    ))
                })?;
            }
            crate::protocol::RootFilesystemEntryKind::File => {
                let bytes = decode_root_entry_content(entry)?;
                fs::write(&shadow_path, bytes).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to materialize shadow file {}: {error}",
                        entry.path
                    ))
                })?;
            }
            crate::protocol::RootFilesystemEntryKind::Symlink => {
                std::os::unix::fs::symlink(
                    entry.target.as_deref().ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "root filesystem symlink {} requires a target",
                            entry.path
                        ))
                    })?,
                    &shadow_path,
                )
                .map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to materialize shadow symlink {}: {error}",
                        entry.path
                    ))
                })?;
                continue;
            }
        }

        let mode = entry.mode.unwrap_or(match entry.kind {
            crate::protocol::RootFilesystemEntryKind::Directory => 0o755,
            crate::protocol::RootFilesystemEntryKind::File => {
                if entry.executable {
                    0o755
                } else {
                    0o644
                }
            }
            crate::protocol::RootFilesystemEntryKind::Symlink => 0o777,
        });
        fs::set_permissions(&shadow_path, fs::Permissions::from_mode(mode & 0o7777)).map_err(
            |error| {
                SidecarError::Io(format!(
                    "failed to set shadow mode on {}: {error}",
                    entry.path
                ))
            },
        )?;
    }

    Ok(())
}

fn prepare_shadow_destination(
    path: &Path,
    desired_kind: &crate::protocol::RootFilesystemEntryKind,
    guest_path: &str,
) -> Result<(), SidecarError> {
    let existing = match fs::symlink_metadata(path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to inspect shadow entry {guest_path}: {error}"
            )));
        }
    };
    let file_type = existing.file_type();
    let already_compatible = match desired_kind {
        crate::protocol::RootFilesystemEntryKind::Directory => {
            file_type.is_dir() && !file_type.is_symlink()
        }
        crate::protocol::RootFilesystemEntryKind::File => {
            file_type.is_file() && !file_type.is_symlink()
        }
        crate::protocol::RootFilesystemEntryKind::Symlink => false,
    };
    if already_compatible {
        return Ok(());
    }

    let result = if file_type.is_dir() && !file_type.is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|error| {
        SidecarError::Io(format!(
            "failed to replace incompatible shadow entry {guest_path}: {error}"
        ))
    })
}

fn decode_root_entry_content(entry: &RootFilesystemEntry) -> Result<Vec<u8>, SidecarError> {
    let content = entry.content.as_deref().unwrap_or_default();
    match entry
        .encoding
        .clone()
        .unwrap_or(crate::protocol::RootFilesystemEntryEncoding::Utf8)
    {
        crate::protocol::RootFilesystemEntryEncoding::Utf8 => Ok(content.as_bytes().to_vec()),
        crate::protocol::RootFilesystemEntryEncoding::Base64 => {
            base64::engine::general_purpose::STANDARD
                .decode(content)
                .map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "invalid base64 root filesystem content for {}: {error}",
                        entry.path
                    ))
                })
        }
    }
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

    for guest_path in command_guest_paths.values() {
        let Some(parent) = Path::new(guest_path)
            .parent()
            .and_then(|path| path.to_str())
        else {
            continue;
        };
        let normalized = normalize_path(parent);
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

    guest_env.insert(String::from("PATH"), merged.join(":"));
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
        bootstrap_native_root_filesystem, bootstrap_shadow_root, close_vm_admission,
        create_vm_unix_socket_host_dir, initialize_vm_shadow_root,
        materialize_shadow_root_snapshot_entries, native_root_plugin_from_config,
        prune_kernel_command_stub, retire_vm_fairness, shadow_path_for_guest, vm_quarantine_reason,
        vm_resource_ledger, wait_for_vm_reconciliation, CA_CERTIFICATES_BUNDLE,
        CA_CERTIFICATES_GUEST_PATH, CA_CERTIFICATES_SYMLINK_PATH, CA_CERTIFICATES_SYMLINK_TARGET,
        KERNEL_COMMAND_STUB,
    };
    use crate::bridge::MountPluginContext;
    use crate::plugins::chunked_local::ChunkedLocalMountPlugin;
    use crate::protocol::{
        ConfigureVmRequest, CreateLayerRequest, CreateVmRequest, DisposeReason, DisposeVmRequest,
        GuestRuntimeKind, OwnershipScope, RequestFrame, RequestPayload, RootFilesystemDescriptor,
        RootFilesystemEntry, RootFilesystemEntryKind, RootFilesystemLowerDescriptor,
    };
    use crate::service::NativeSidecar;
    use crate::state::{
        ConnectionState, QuarantinedVmGeneration, SessionState, VmQuarantineReason,
        VmReconciliationSnapshot,
    };
    use crate::stdio::LocalBridge;
    use agentos_bridge::FilesystemSnapshot;
    use agentos_kernel::kernel::{KernelVm, KernelVmConfig};
    use agentos_kernel::mount_plugin::{FileSystemPluginFactory, OpenFileSystemPluginRequest};
    use agentos_kernel::mount_table::{MountOptions, MountTable};
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::resource_accounting::ResourceLimits;
    use agentos_kernel::root_fs::{encode_snapshot, FilesystemEntry, RootFilesystemSnapshot};
    use agentos_kernel::vfs::VirtualFileSystem;
    use agentos_runtime::accounting::{ResourceClass, ResourceLedger, ResourceLimit};
    use agentos_runtime::capability::{CapabilityKind, CapabilityRegistry};
    use agentos_runtime::fairness::FairBudget;
    use agentos_runtime::metrics::ResourceMetricClass;
    use agentos_runtime::{RuntimeContext, SidecarRuntime, TaskClass};
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn reconciliation_handles(
        generation: u64,
    ) -> (Arc<ResourceLedger>, RuntimeContext, CapabilityRegistry) {
        let process = SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
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
    fn vm_runtime_bounds_every_resource_class_by_default() {
        let process = SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
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
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("teardown test runtime")
            .block_on(future)
    }

    #[test]
    fn vm_lifecycle_wrappers_return_static_futures_without_borrowing_the_sidecar() {
        fn assert_static<F: std::future::Future + 'static>(_: &F) {}

        let mut sidecar = NativeSidecar::new(LocalBridge::default()).expect("test sidecar");
        let request = RequestFrame::new(
            1,
            OwnershipScope::vm("connection-missing", "session-missing", "vm-missing"),
            RequestPayload::CreateLayer(CreateLayerRequest {}),
        );
        let operation = sidecar.create_layer(&request, CreateLayerRequest {});
        assert_static(&operation);

        // This mutation is a compile-time assertion that `operation` did not
        // retain the method receiver's `&mut NativeSidecar` borrow.
        sidecar.next_vm_id = 41;
        let result = block_on(operation);
        assert!(matches!(result, Err(crate::SidecarError::InvalidState(_))));
        assert_eq!(sidecar.next_vm_id, 41);

        let configure_payload = ConfigureVmRequest {
            mounts: Vec::new(),
            software: Vec::new(),
            permissions: None,
            module_access_cwd: None,
            instructions: Vec::new(),
            projected_modules: Vec::new(),
            command_permissions: std::collections::HashMap::new(),
            loopback_exempt_ports: Vec::new(),
            packages: Vec::new(),
            packages_mount_at: String::new(),
            bootstrap_commands: Vec::new(),
            binding_shim_commands: Vec::new(),
        };
        let configure_request = RequestFrame::new(
            2,
            OwnershipScope::vm("connection-missing", "session-missing", "vm-missing"),
            RequestPayload::ConfigureVm(configure_payload.clone()),
        );
        let configure_operation = sidecar.configure_vm(&configure_request, configure_payload);
        assert_static(&configure_operation);
        sidecar.next_vm_id = 42;
        let result = block_on(configure_operation);
        assert!(matches!(result, Err(crate::SidecarError::InvalidState(_))));
        assert_eq!(sidecar.next_vm_id, 42);
    }

    #[test]
    fn create_and_dispose_release_central_state_during_owned_work() {
        let mut sidecar = NativeSidecar::new(LocalBridge::default()).expect("test sidecar");
        let connection_id = String::from("connection-owned-lifecycle");
        let session_id = String::from("session-owned-lifecycle");
        sidecar.connections.insert(
            connection_id.clone(),
            ConnectionState {
                auth_token: String::new(),
                sessions: BTreeSet::from([session_id.clone()]),
            },
        );
        sidecar.sessions.insert(
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

        let create_payload = CreateVmRequest::legacy_test_config(
            GuestRuntimeKind::JavaScript,
            Default::default(),
            Default::default(),
            None,
        );
        let create_request = RequestFrame::new(
            100,
            OwnershipScope::session(&connection_id, &session_id),
            RequestPayload::CreateVm(create_payload.clone()),
        );
        let prepared = sidecar
            .prepare_create_vm(&create_request, create_payload)
            .expect("prepare owned VM create");
        assert!(sidecar.vms.is_empty());
        sidecar.next_sidecar_request_id = -41;
        let completed = block_on(prepared.execute()).expect("execute owned VM create");
        assert!(sidecar.vms.is_empty());
        assert_eq!(sidecar.next_sidecar_request_id, -41);
        sidecar
            .complete_create_vm(completed)
            .expect("publish completed VM create");
        let vm_id = sidecar
            .sessions
            .get(&session_id)
            .and_then(|session| session.vm_ids.iter().next())
            .cloned()
            .expect("created VM session membership");
        assert!(sidecar.vms.contains_key(&vm_id));

        let dispose_payload = DisposeVmRequest {
            reason: DisposeReason::Requested,
        };
        let dispose_request = RequestFrame::new(
            101,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::DisposeVm(dispose_payload.clone()),
        );
        let plan = sidecar
            .prepare_dispose_vm(&dispose_request, dispose_payload)
            .expect("prepare owned VM dispose");
        let prepared = sidecar
            .detach_vm_for_disposal(plan)
            .expect("detach VM after operation drain");
        assert!(!sidecar.vms.contains_key(&vm_id));
        assert!(sidecar
            .sessions
            .get(&session_id)
            .is_some_and(|session| session.vm_ids.contains(&vm_id)));
        sidecar.next_sidecar_request_id = -42;
        let completed = block_on(prepared.execute());
        assert_eq!(sidecar.next_sidecar_request_id, -42);
        sidecar
            .complete_dispose_vm(completed)
            .expect("finalize completed VM dispose");
        assert!(sidecar
            .sessions
            .get(&session_id)
            .is_some_and(|session| !session.vm_ids.contains(&vm_id)));
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
        let process = SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
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
                [(resource, ResourceLimit::new(maximum, process_path))],
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
    fn bootstrap_shadow_root_seeds_standard_directories() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("agentos-native-sidecar-shadow-test-{unique}"));
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

    #[test]
    fn bootstrap_shadow_root_seeds_ca_bundle_when_present() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("agentos-native-sidecar-ca-test-{unique}"));
        fs::create_dir_all(&root).expect("temp shadow root should be created");

        bootstrap_shadow_root(&root).expect("shadow bootstrap should succeed");

        let bundle = shadow_path_for_guest(&root, CA_CERTIFICATES_GUEST_PATH);
        let symlink = shadow_path_for_guest(&root, CA_CERTIFICATES_SYMLINK_PATH);

        assert!(!CA_CERTIFICATES_BUNDLE.is_empty());
        let seeded = fs::read(&bundle).expect("CA bundle should be seeded");
        assert_eq!(
            seeded, CA_CERTIFICATES_BUNDLE,
            "seeded CA bundle should match the embedded asset"
        );
        let target = fs::read_link(&symlink).expect("cert.pem symlink should be seeded");
        assert_eq!(
            target,
            Path::new(CA_CERTIFICATES_SYMLINK_TARGET),
            "cert.pem should point at certs/ca-certificates.crt"
        );

        fs::remove_dir_all(&root).expect("temp shadow root should be removed");
    }

    #[test]
    fn failed_shadow_bootstrap_removes_temporary_root() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("agentos-native-sidecar-shadow-failure-{unique}"));
        fs::create_dir_all(&root).expect("temp shadow root should be created");
        fs::write(root.join("dev"), b"blocks directory creation")
            .expect("blocking file should be created");

        initialize_vm_shadow_root(root.clone())
            .expect_err("invalid shadow scaffold should fail bootstrap");
        assert!(
            !root.exists(),
            "failed bootstrap must not leak its temporary shadow root"
        );
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
        assert!(mount_table.exists("/home/agentos"));
        let home = mount_table
            .stat("/home/agentos")
            .expect("native AgentOS home metadata should be readable");
        assert_eq!((home.uid, home.gid), (1000, 1000));
        assert_eq!(home.mode & 0o7777, 0o2755);
        let workspace = mount_table
            .stat("/workspace")
            .expect("native workspace metadata should be readable");
        assert_eq!((workspace.uid, workspace.gid), (1000, 1000));
        assert_eq!(workspace.mode & 0o7777, 0o755);
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

    #[test]
    fn custom_shadow_ca_files_replace_seeded_defaults_without_following_symlinks() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("agentos-native-sidecar-shadow-custom-ca-{unique}"));
        fs::create_dir_all(&root).expect("temp shadow root should be created");
        bootstrap_shadow_root(&root).expect("shadow bootstrap should succeed");

        let descriptor = RootFilesystemDescriptor {
            bootstrap_entries: vec![
                RootFilesystemEntry {
                    path: "/custom/ca.pem".to_string(),
                    kind: RootFilesystemEntryKind::File,
                    content: Some("custom bundle\n".to_string()),
                    ..Default::default()
                },
                RootFilesystemEntry {
                    path: CA_CERTIFICATES_GUEST_PATH.to_string(),
                    kind: RootFilesystemEntryKind::Symlink,
                    target: Some("../../../custom/ca.pem".to_string()),
                    ..Default::default()
                },
                RootFilesystemEntry {
                    path: CA_CERTIFICATES_SYMLINK_PATH.to_string(),
                    kind: RootFilesystemEntryKind::File,
                    content: Some("custom cert.pem\n".to_string()),
                    ..Default::default()
                },
            ],
            ..RootFilesystemDescriptor::default()
        };

        materialize_shadow_root_snapshot_entries(
            &root,
            &descriptor,
            None,
            &ResourceLimits::default(),
        )
        .expect("custom CA entries should materialize");

        let bundle = shadow_path_for_guest(&root, CA_CERTIFICATES_GUEST_PATH);
        let cert_pem = shadow_path_for_guest(&root, CA_CERTIFICATES_SYMLINK_PATH);
        assert_eq!(
            fs::read(&bundle).expect("read custom bundle through custom symlink"),
            b"custom bundle\n"
        );
        assert_eq!(
            fs::read_link(&bundle).expect("read custom CA bundle symlink"),
            Path::new("../../../custom/ca.pem"),
            "a custom symlink must replace the seeded regular bundle"
        );
        assert_eq!(
            fs::read(&cert_pem).expect("read custom regular cert.pem"),
            b"custom cert.pem\n"
        );
        assert!(
            !fs::symlink_metadata(cert_pem)
                .expect("lstat custom cert.pem")
                .file_type()
                .is_symlink(),
            "custom cert.pem must replace rather than follow the seeded symlink"
        );

        fs::remove_dir_all(&root).expect("temp shadow root should be removed");
    }

    #[test]
    fn materialize_shadow_root_snapshot_entries_rejects_oversized_legacy_restored_snapshots() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("agentos-native-sidecar-shadow-limit-{unique}"));
        fs::create_dir_all(&root).expect("temp shadow root should be created");
        bootstrap_shadow_root(&root).expect("shadow bootstrap should succeed");

        let snapshot = RootFilesystemSnapshot {
            entries: vec![FilesystemEntry::file("/large.txt", b"four".to_vec())],
        };
        let loaded_snapshot = FilesystemSnapshot {
            format: String::from("agentos_filesystem_snapshot_v1"),
            bytes: encode_snapshot(&snapshot).expect("encode restored snapshot"),
        };
        let resource_limits = ResourceLimits {
            max_filesystem_bytes: Some(3),
            ..ResourceLimits::default()
        };

        let error = materialize_shadow_root_snapshot_entries(
            &root,
            &RootFilesystemDescriptor::default(),
            Some(&loaded_snapshot),
            &resource_limits,
        )
        .expect_err("oversized restored snapshot should be rejected");

        assert!(error.to_string().contains("exceeding limit 3"));
        fs::remove_dir_all(&root).expect("temp shadow root should be removed");
    }

    #[test]
    fn materialize_shadow_root_snapshot_entries_rejects_oversized_descriptor_before_writes() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("agentos-native-sidecar-shadow-descriptor-{unique}"));
        fs::create_dir_all(&root).expect("temp shadow root should be created");
        bootstrap_shadow_root(&root).expect("shadow bootstrap should succeed");

        let descriptor = RootFilesystemDescriptor {
            lowers: vec![RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(
                crate::protocol::SnapshotRootFilesystemLower {
                    entries: vec![RootFilesystemEntry {
                        path: String::from("/large.txt"),
                        kind: RootFilesystemEntryKind::File,
                        mode: Some(0o644),
                        uid: Some(0),
                        gid: Some(0),
                        content: Some(String::from("four")),
                        encoding: Some(crate::protocol::RootFilesystemEntryEncoding::Utf8),
                        target: None,
                        executable: false,
                    }],
                },
            )],
            ..RootFilesystemDescriptor::default()
        };
        let resource_limits = ResourceLimits {
            max_filesystem_bytes: Some(3),
            ..ResourceLimits::default()
        };

        let error =
            materialize_shadow_root_snapshot_entries(&root, &descriptor, None, &resource_limits)
                .expect_err("oversized descriptor should be rejected");

        assert!(error.to_string().contains("exceeding limit 3"));
        assert!(
            !shadow_path_for_guest(&root, "/large.txt").exists(),
            "oversized descriptor must be rejected before materializing files"
        );
        fs::remove_dir_all(&root).expect("temp shadow root should be removed");
    }

    #[test]
    fn materialize_shadow_root_snapshot_entries_counts_implicit_parent_directories() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("agentos-native-sidecar-shadow-parents-{unique}"));
        fs::create_dir_all(&root).expect("temp shadow root should be created");
        bootstrap_shadow_root(&root).expect("shadow bootstrap should succeed");

        let descriptor = RootFilesystemDescriptor {
            lowers: vec![RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(
                crate::protocol::SnapshotRootFilesystemLower {
                    entries: vec![RootFilesystemEntry {
                        path: String::from("/deep/nested/file.txt"),
                        kind: RootFilesystemEntryKind::File,
                        mode: Some(0o644),
                        uid: Some(0),
                        gid: Some(0),
                        content: Some(String::from("x")),
                        encoding: Some(crate::protocol::RootFilesystemEntryEncoding::Utf8),
                        target: None,
                        executable: false,
                    }],
                },
            )],
            ..RootFilesystemDescriptor::default()
        };
        let resource_limits = ResourceLimits {
            max_inode_count: Some(1),
            ..ResourceLimits::default()
        };

        let error =
            materialize_shadow_root_snapshot_entries(&root, &descriptor, None, &resource_limits)
                .expect_err("implicit parents should be rejected");

        assert!(error.to_string().contains("exceeding limit 1"));
        assert!(
            !shadow_path_for_guest(&root, "/deep").exists(),
            "implicit parents must not be materialized after rejection"
        );
        fs::remove_dir_all(&root).expect("temp shadow root should be removed");
    }

    #[test]
    fn materialize_shadow_root_snapshot_entries_rejects_duplicate_descriptor_entries() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("agentos-native-sidecar-shadow-duplicates-{unique}"));
        fs::create_dir_all(&root).expect("temp shadow root should be created");
        bootstrap_shadow_root(&root).expect("shadow bootstrap should succeed");

        let duplicate_entry = RootFilesystemEntry {
            path: String::from("/dup.txt"),
            kind: RootFilesystemEntryKind::File,
            mode: Some(0o644),
            uid: Some(0),
            gid: Some(0),
            content: Some(String::new()),
            encoding: Some(crate::protocol::RootFilesystemEntryEncoding::Utf8),
            target: None,
            executable: false,
        };
        let descriptor = RootFilesystemDescriptor {
            lowers: vec![RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(
                crate::protocol::SnapshotRootFilesystemLower {
                    entries: vec![duplicate_entry.clone(), duplicate_entry],
                },
            )],
            ..RootFilesystemDescriptor::default()
        };
        let resource_limits = ResourceLimits {
            max_inode_count: Some(1),
            ..ResourceLimits::default()
        };

        let error =
            materialize_shadow_root_snapshot_entries(&root, &descriptor, None, &resource_limits)
                .expect_err("duplicate descriptor entries should be rejected");

        assert!(error.to_string().contains("exceeding limit 1"));
        assert!(
            !shadow_path_for_guest(&root, "/dup.txt").exists(),
            "duplicate descriptor must be rejected before materializing files"
        );
        fs::remove_dir_all(&root).expect("temp shadow root should be removed");
    }

    #[test]
    fn materialize_shadow_root_snapshot_entries_copies_custom_snapshot_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("agentos-native-sidecar-shadow-snapshot-{unique}"));
        fs::create_dir_all(&root).expect("temp shadow root should be created");
        bootstrap_shadow_root(&root).expect("shadow bootstrap should succeed");

        let descriptor = RootFilesystemDescriptor {
            lowers: vec![RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(
                crate::protocol::SnapshotRootFilesystemLower {
                    entries: vec![
                        RootFilesystemEntry {
                            path: String::from("/"),
                            kind: RootFilesystemEntryKind::Directory,
                            mode: Some(0o755),
                            uid: Some(0),
                            gid: Some(0),
                            content: None,
                            encoding: None,
                            target: None,
                            executable: false,
                        },
                        RootFilesystemEntry {
                            path: String::from("/hello.txt"),
                            kind: RootFilesystemEntryKind::File,
                            mode: Some(0o644),
                            uid: Some(0),
                            gid: Some(0),
                            content: Some(String::from("hello from snapshot\n")),
                            encoding: Some(crate::protocol::RootFilesystemEntryEncoding::Utf8),
                            target: None,
                            executable: false,
                        },
                    ],
                },
            )],
            ..RootFilesystemDescriptor::default()
        };

        materialize_shadow_root_snapshot_entries(
            &root,
            &descriptor,
            None,
            &ResourceLimits::default(),
        )
        .expect("snapshot entries should materialize into the shadow root");

        assert_eq!(
            fs::read_to_string(shadow_path_for_guest(&root, "/hello.txt"))
                .expect("shadow file should be readable"),
            "hello from snapshot\n"
        );

        fs::remove_dir_all(&root).expect("temp shadow root should be removed");
    }
}
