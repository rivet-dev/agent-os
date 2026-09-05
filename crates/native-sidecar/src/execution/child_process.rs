use super::*;

const SYNTHETIC_V8_TERMINATION_STDERR: &[u8] = b"Error: Execution terminated\n";

type OwnedChildExecutionStart =
    Pin<Box<dyn Future<Output = Result<ActiveExecution, SidecarError>> + 'static>>;

fn settle_owned_javascript_process_event_target<B>(
    target: OwnedJavascriptEventService,
    response: Result<JavascriptSyncRpcServiceResponse, SidecarError>,
) -> Pin<Box<dyn Future<Output = Result<(), SidecarError>> + 'static>>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    Box::pin(async move {
        let result = settle_owned_fetch_sync_rpc::<B>(
            &target.vm,
            &target.process_id,
            &target.child_path,
            target.request.clone(),
            response,
        )
        .await;
        drop(target);
        result
    })
}

/// A request-owned view of one VM. Child-process startup captures this view
/// before it leaves the protocol dispatcher, so the startup future never
/// retains `NativeSidecar` while still preserving the old short-borrow call
/// sites during the staged migration.
#[derive(Clone)]
struct OwnedChildProcessVm {
    vm_id: String,
    handle: VmHandle,
}

struct PendingOwnedChildSpawn {
    vm: VmHandle,
    kernel_handle: Option<KernelProcessHandle>,
    execution: Option<ActiveExecution>,
}

impl PendingOwnedChildSpawn {
    fn new(vm: VmHandle, kernel_handle: KernelProcessHandle) -> Self {
        Self {
            vm,
            kernel_handle: Some(kernel_handle),
            execution: None,
        }
    }

    fn set_execution(&mut self, execution: ActiveExecution) {
        self.execution = Some(execution);
    }

    fn take_execution(&mut self) -> ActiveExecution {
        self.execution
            .take()
            .expect("owned child startup completed before registration")
    }

    fn disarm(&mut self) {
        self.kernel_handle = None;
        self.execution = None;
    }
}

impl Drop for PendingOwnedChildSpawn {
    fn drop(&mut self) {
        let Some(kernel_handle) = self.kernel_handle.take() else {
            return;
        };
        let mut execution = self.execution.take();
        if let Err(error) = self
            .vm
            .try_command("roll back cancelled child startup", |vm| {
                rollback_unregistered_spawn_child(
                    &mut vm.kernel,
                    &kernel_handle,
                    execution.as_mut(),
                    "owned child_process.spawn",
                );
                Ok(())
            })
        {
            tracing::error!(
                %error,
                pid = kernel_handle.pid(),
                "ERR_AGENTOS_CHILD_PROCESS_ROLLBACK: cancelled child startup could not be rolled back"
            );
        }
    }
}

/// Rolls back a child that has been published into its caller's process tree
/// but whose higher-level spawnSync/subprocess completion registration has not
/// committed yet.
pub(super) struct PendingOwnedRegisteredChild {
    vm: VmHandle,
    process_id: String,
    parent_path: Vec<String>,
    child_process_id: Option<String>,
}

impl PendingOwnedRegisteredChild {
    pub(super) fn new(
        vm: VmHandle,
        process_id: String,
        parent_path: Vec<String>,
        child_process_id: String,
    ) -> Self {
        Self {
            vm,
            process_id,
            parent_path,
            child_process_id: Some(child_process_id),
        }
    }

    pub(super) fn disarm(&mut self) {
        self.child_process_id = None;
    }
}

impl Drop for PendingOwnedRegisteredChild {
    fn drop(&mut self) {
        let Some(child_process_id) = self.child_process_id.take() else {
            return;
        };
        let process_id = self.process_id.clone();
        let parent_path = self.parent_path.clone();
        if let Err(error) = self
            .vm
            .try_command("roll back registered child startup", |vm| {
                let Some(root) = vm.active_processes.get_mut(&process_id) else {
                    return Ok(());
                };
                let mut parent = root;
                for segment in &parent_path {
                    let Some(child) = parent.child_processes.get_mut(segment) else {
                        return Ok(());
                    };
                    parent = child;
                }
                parent.pending_child_process_sync.remove(&child_process_id);
                let Some(mut child) = parent.child_processes.remove(&child_process_id) else {
                    return Ok(());
                };
                terminate_child_process_tree(
                    &mut vm.kernel,
                    &mut child,
                    &vm.kernel_socket_readiness,
                    &vm.unix_address_registry,
                );
                child.kernel_handle.finish(1);
                let _ = vm.kernel.wait_and_reap(child.kernel_pid);
                vm.signal_states.remove(&child_process_id);
                Ok(())
            })
        {
            tracing::error!(
                %error,
                process_id,
                child_process_id,
                "ERR_AGENTOS_CHILD_PROCESS_ROLLBACK: failed to clean up a partially registered child"
            );
        }
    }
}

impl OwnedChildProcessVm {
    fn get(&self, vm_id: &str) -> Option<std::cell::Ref<'_, VmState>> {
        (self.vm_id == vm_id).then(|| self.handle.borrow())
    }

    fn get_mut(&self, vm_id: &str) -> Option<std::cell::RefMut<'_, VmState>> {
        (self.vm_id == vm_id).then(|| self.handle.borrow_mut())
    }
}

fn resolve_owned_javascript_child_process_with_shebang<B>(
    vms: &OwnedChildProcessVm,
    vm_id: &str,
    parent_env: &BTreeMap<String, String>,
    parent_guest_cwd: &str,
    parent_host_cwd: &Path,
    request: &mut JavascriptChildProcessSpawnRequest,
) -> Result<ResolvedChildProcessExecution, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    const MAX_SHEBANG_REDIRECTS: usize = 4;

    let mut resolved = {
        let vm = vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        NativeSidecar::<B>::resolve_javascript_child_process_execution_with_mode(
            &vm,
            parent_env,
            parent_guest_cwd,
            parent_host_cwd,
            request,
            false,
            None,
        )?
    };
    for redirects in 0..=MAX_SHEBANG_REDIRECTS {
        let redirected = {
            let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            rewrite_javascript_shebang_request(&mut vm, &resolved, request)?
        };
        if !redirected {
            return Ok(resolved);
        }
        if redirects == MAX_SHEBANG_REDIRECTS {
            return Err(SidecarError::Execution(format!(
                "ELOOP: exceeded {MAX_SHEBANG_REDIRECTS} shebang redirects"
            )));
        }
        resolved = {
            let vm = vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            NativeSidecar::<B>::resolve_javascript_child_process_execution_with_mode(
                &vm,
                parent_env,
                parent_guest_cwd,
                parent_host_cwd,
                request,
                false,
                None,
            )?
        };
    }
    Ok(resolved)
}

pub(crate) struct OwnedChildBridgeEventService {
    ownership: OwnershipScope,
    vm_id: String,
    root_process_id: String,
    parent_path: Vec<String>,
    event_type: &'static str,
    payload: Value,
    vm: VmHandle,
    deadline: Instant,
    _relay: Option<ChildBridgeRelayLease>,
    _reservation: Option<PendingExecutionEventReservation>,
}

struct ChildBridgeRelayLease {
    in_flight: Arc<AtomicBool>,
    process_event_notify: Arc<tokio::sync::Notify>,
}

impl ChildBridgeRelayLease {
    fn acquire(
        in_flight: Arc<AtomicBool>,
        process_event_notify: Arc<tokio::sync::Notify>,
        child_process_id: &str,
    ) -> Result<Self, SidecarError> {
        in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                SidecarError::InvalidState(format!(
                    "ERR_AGENTOS_CHILD_BRIDGE_RELAY_OVERLAP: child {child_process_id} already owns an in-flight parent relay"
                ))
            })?;
        Ok(Self {
            in_flight,
            process_event_notify,
        })
    }
}

impl Drop for ChildBridgeRelayLease {
    fn drop(&mut self) {
        if self.in_flight.swap(false, Ordering::AcqRel) {
            // The source child's later stdout/stderr/exit events stayed
            // durably queued while this exact relay owned ordering. Rearm the
            // central pump only after delivery, failure, or cancellation has
            // released that ownership.
            self.process_event_notify.notify_one();
        }
    }
}

struct ClaimedDescendantBridgeEvent {
    event: Value,
    reservation: Option<PendingExecutionEventReservation>,
}

impl OwnedChildBridgeEventService {
    pub(crate) fn ownership(&self) -> &OwnershipScope {
        &self.ownership
    }
}

pub(crate) async fn service_owned_child_bridge_event(
    target: OwnedChildBridgeEventService,
) -> Result<(), SidecarError> {
    let mut retry_delay = Duration::from_millis(2);
    let mut capacity_warning_emitted = false;
    loop {
        let result = target
            .vm
            .try_read("deliver owned child bridge event", |vm| {
                let Some(root) = vm.active_processes.get(&target.root_process_id) else {
                    return Ok(());
                };
                let mut parent = root;
                for child_id in &target.parent_path {
                    let Some(child) = parent.child_processes.get(child_id) else {
                        return Ok(());
                    };
                    parent = child;
                }
                parent
                    .execution
                    .send_javascript_stream_event(target.event_type, target.payload.clone())
            })?;
        match result {
            Ok(()) => return Ok(()),
            Err(SidecarError::Execution(message))
                if message.contains("ERR_AGENTOS_SESSION_COMMAND_LIMIT") =>
            {
                if !capacity_warning_emitted && Instant::now() >= target.deadline {
                    tracing::warn!(
                        vm_id = %target.vm_id,
                        process_id = %target.root_process_id,
                        "ERR_AGENTOS_SESSION_COMMAND_LIMIT: child bridge event remains durably leased while runtime command capacity is unavailable"
                    );
                    capacity_warning_emitted = true;
                }
                tokio::time::sleep(retry_delay).await;
                retry_delay = retry_delay.saturating_mul(2).min(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
}

#[derive(Debug)]
pub(super) enum TransferredHostNetSocket {
    Tcp {
        socket: Box<ActiveTcpSocket>,
        metadata: TransferredHostNetMetadata,
    },
    TcpListener {
        listener: ActiveTcpListener,
        metadata: TransferredHostNetMetadata,
    },
    Udp {
        socket: ActiveUdpSocket,
        metadata: TransferredHostNetMetadata,
    },
    Unix {
        socket: ActiveUnixSocket,
        metadata: TransferredHostNetMetadata,
    },
    UnixListener {
        listener: ActiveUnixListener,
        metadata: TransferredHostNetMetadata,
    },
    Pending {
        metadata: TransferredHostNetMetadata,
        description_handles: Arc<()>,
    },
}

#[cfg(test)]
mod scm_rights_tests {
    use super::*;

    fn canonical_tcp_metadata(nonblocking: bool) -> TransferredHostNetMetadata {
        TransferredHostNetMetadata {
            domain: HOST_NET_AF_INET,
            socket_type: HOST_NET_SOCK_STREAM,
            protocol: HOST_NET_IPPROTO_TCP,
            nonblocking,
            recv_timeout_ms: Some(250),
            bind_options: None,
            local_info: Some(json!({ "address": "127.0.0.1", "port": 41000 })),
            local_unix_address: None,
            local_reservation: None,
            remote_info: Some(json!({ "address": "127.0.0.1", "port": 8080 })),
            remote_unix_address: None,
            listening: false,
        }
    }

    #[test]
    fn scm_rights_rejects_forged_ids_metadata_and_unbounded_open_state() {
        let canonical = canonical_tcp_metadata(false).as_value();
        let mut ambiguous = canonical.clone();
        ambiguous["socketId"] = json!("socket-1");
        ambiguous["serverId"] = json!("listener-1");
        assert!(scm_rights_host_net_source(&ambiguous)
            .expect_err("SCM_RIGHTS class ids must be unambiguous")
            .to_string()
            .contains("at most one resource id"));

        let mut wrong_class = canonical.clone();
        wrong_class["class"] = json!("listener");
        assert!(validate_host_net_metadata(
            &wrong_class,
            &canonical_tcp_metadata(false),
            "tcp",
            "SCM_RIGHTS host-network",
        )
        .expect_err("SCM class forgery")
        .to_string()
        .contains("class"));

        let mut wrong_address = canonical.clone();
        wrong_address["remoteInfo"] = json!({ "address": "203.0.113.8", "port": 22 });
        assert!(validate_host_net_metadata(
            &wrong_address,
            &canonical_tcp_metadata(false),
            "tcp",
            "SCM_RIGHTS host-network",
        )
        .expect_err("SCM address forgery")
        .to_string()
        .contains("remoteInfo"));

        let mut bad_nonblocking = canonical.clone();
        bad_nonblocking["nonblocking"] = json!("yes");
        assert!(
            host_net_open_description_options(&bad_nonblocking, "SCM_RIGHTS host-network")
                .expect_err("nonblocking must be strict boolean")
                .to_string()
                .contains("must be boolean")
        );

        let mut bad_timeout = canonical.clone();
        bad_timeout["recvTimeoutMs"] = json!(HOST_NET_RECV_TIMEOUT_MAX_MS + 1);
        assert!(
            host_net_open_description_options(&bad_timeout, "SCM_RIGHTS host-network")
                .expect_err("timeout must be range bounded")
                .to_string()
                .contains("exceeds")
        );

        let mut oversized = canonical;
        oversized["extra"] = json!("x".repeat(HOST_NET_METADATA_MAX_STRING_BYTES + 1));
        assert!(
            host_net_open_description_options(&oversized, "SCM_RIGHTS host-network")
                .expect_err("metadata strings must be bounded")
                .to_string()
                .contains("ENAMETOOLONG")
        );
    }

    #[test]
    fn scm_pending_accepts_only_canonical_unconnected_socket_tuples() {
        let pending = json!({
            "kind": "hostNet",
            "domain": HOST_NET_AF_UNIX,
            "socketType": HOST_NET_SOCK_STREAM,
            "protocol": 0,
            "nonblocking": true,
            "recvTimeoutMs": null,
            "bindOptions": null,
            "localInfo": null,
            "localUnixAddress": "unix-unnamed",
            "localReservation": null,
            "remoteInfo": null,
            "remoteUnixAddress": null,
            "listening": false,
        });
        let options = host_net_open_description_options(&pending, "SCM_RIGHTS pending socket")
            .expect("pending options");
        let canonical =
            TransferredHostNetMetadata::pending(&pending, options, "SCM_RIGHTS pending socket")
                .expect("valid unconnected Unix stream");
        assert_eq!(
            canonical.as_value()["localUnixAddress"],
            json!("unix-unnamed")
        );

        for (field, replacement) in [
            ("listening", json!(true)),
            ("bindOptions", json!({ "path": "/forged" })),
            ("remoteUnixAddress", json!("unix:/forged-peer")),
        ] {
            let mut forged = pending.clone();
            forged[field] = replacement;
            let options = host_net_open_description_options(&forged, "SCM_RIGHTS pending socket")
                .expect("open state remains syntactically valid");
            assert!(TransferredHostNetMetadata::pending(
                &forged,
                options,
                "SCM_RIGHTS pending socket",
            )
            .is_err());
        }

        let unsupported = json!({
            "domain": HOST_NET_AF_UNIX,
            "socketType": HOST_NET_SOCK_DGRAM,
            "protocol": 0,
            "listening": false,
        });
        let options = HostNetOpenDescriptionOptions {
            nonblocking: false,
            recv_timeout_ms: None,
        };
        assert!(TransferredHostNetMetadata::pending(
            &unsupported,
            options,
            "SCM_RIGHTS pending socket",
        )
        .expect_err("unsupported tuple")
        .to_string()
        .contains("EPROTONOSUPPORT"));
    }

    #[test]
    fn duplicate_rights_share_one_description_and_queue_lifecycle_is_counted() {
        let registry = Arc::new(Mutex::new(BTreeMap::new()));
        let resource = TransferredHostNetSocket::Pending {
            metadata: canonical_tcp_metadata(false),
            description_handles: Arc::new(()),
        };
        let duplicate = resource
            .clone_for_fd_transfer()
            .expect("duplicate one open-file description");
        register_host_net_transfer_description(&registry, &resource);
        register_host_net_transfer_description(&registry, &duplicate);

        let mut queued = BTreeMap::new();
        add_live_host_net_transfer_descriptions(&registry, &mut queued);
        assert_eq!(queued.len(), 1, "duplicate rights are one open description");
        check_spawn_host_net_resource_limit(
            Some(1),
            1,
            0,
            "EMFILE",
            "SCM_RIGHTS socket descriptions",
            "maxSockets",
        )
        .expect("transferring an existing description at the maximum is allowed");

        drop(resource);
        queued.clear();
        add_live_host_net_transfer_descriptions(&registry, &mut queued);
        assert_eq!(
            queued.len(),
            1,
            "queued/received alias keeps the description live"
        );

        drop(duplicate);
        queued.clear();
        add_live_host_net_transfer_descriptions(&registry, &mut queued);
        assert!(
            queued.is_empty(),
            "dropping the final right releases the queue lease"
        );

        assert!(check_spawn_host_net_resource_limit(
            Some(1),
            1,
            1,
            "EMFILE",
            "SCM_RIGHTS pending socket descriptions",
            "maxSockets",
        )
        .is_err());
    }
}

#[cfg(test)]
mod descendant_rpc_route_tests {
    #[test]
    fn descendant_dispatch_routes_exec_and_fd_image_commit() {
        let source = include_str!("child_process.rs");
        let start = source
            .rfind("async fn poll_descendant_javascript_child_process")
            .expect("descendant pump must exist");
        let end = source[start..]
            .find("fn write_descendant_javascript_child_process_stdin")
            .map(|offset| start + offset)
            .expect("descendant pump end must exist");
        let dispatcher = &source[start..end];

        for (method, handler) in [
            ("process.exec", "self.exec_javascript_process_image"),
            (
                "process.exec_fd_image_commit",
                "self.commit_wasm_fd_process_image",
            ),
        ] {
            assert!(
                dispatcher.contains(&format!("request.method == \"{method}\"")),
                "descendant dispatcher does not route {method}"
            );
            assert!(
                dispatcher.contains(handler),
                "descendant dispatcher does not call {handler}"
            );
        }
    }
}

impl TransferredHostNetSocket {
    fn class(&self) -> &'static str {
        match self {
            Self::Tcp { .. } => "tcp",
            Self::TcpListener { .. } => "listener",
            Self::Udp { .. } => "udp",
            Self::Unix { .. } => "unix",
            Self::UnixListener { .. } => "unix-listener",
            Self::Pending { .. } => "pending",
        }
    }

    fn metadata(&self) -> &TransferredHostNetMetadata {
        match self {
            Self::Tcp { metadata, .. }
            | Self::TcpListener { metadata, .. }
            | Self::Udp { metadata, .. }
            | Self::Unix { metadata, .. }
            | Self::UnixListener { metadata, .. }
            | Self::Pending { metadata, .. } => metadata,
        }
    }

    fn description_identity(&self) -> (&Arc<()>, bool, bool) {
        match self {
            Self::Tcp { socket, .. } => (
                &socket.description_handles,
                true,
                socket.kernel_socket_id.is_some(),
            ),
            Self::TcpListener { listener, .. } => (
                &listener.description_handles,
                false,
                listener.kernel_socket_id.is_some(),
            ),
            Self::Udp { socket, .. } => (
                &socket.description_handles,
                false,
                socket.kernel_socket_id.is_some(),
            ),
            Self::Unix { socket, .. } => (&socket.description_handles, true, false),
            Self::UnixListener { listener, .. } => (&listener.description_handles, false, false),
            Self::Pending {
                description_handles,
                ..
            } => (description_handles, false, false),
        }
    }

    pub(super) fn clone_for_fd_transfer(&self) -> Result<Self, SidecarError> {
        match self {
            Self::Tcp { socket, metadata } => Ok(Self::Tcp {
                socket: Box::new(socket.clone_for_fd_transfer()),
                metadata: metadata.clone(),
            }),
            Self::TcpListener { listener, metadata } => Ok(Self::TcpListener {
                listener: listener.clone_for_fd_transfer()?,
                metadata: metadata.clone(),
            }),
            Self::Udp { socket, metadata } => Ok(Self::Udp {
                socket: socket.clone_for_fd_transfer()?,
                metadata: metadata.clone(),
            }),
            Self::Unix { socket, metadata } => Ok(Self::Unix {
                socket: socket.clone_for_fd_transfer(),
                metadata: metadata.clone(),
            }),
            Self::UnixListener { listener, metadata } => Ok(Self::UnixListener {
                listener: listener.clone_for_fd_transfer()?,
                metadata: metadata.clone(),
            }),
            Self::Pending {
                metadata,
                description_handles,
            } => Ok(Self::Pending {
                metadata: metadata.clone(),
                description_handles: Arc::clone(description_handles),
            }),
        }
    }
}

pub(super) fn register_host_net_transfer_description(
    registry: &HostNetTransferDescriptionRegistry,
    resource: &TransferredHostNetSocket,
) {
    let (handles, connected, kernel_backed) = resource.description_identity();
    // Adopted kernel sockets remain present in the kernel resource snapshot
    // while queued. Only sidecar-only descriptions need this weak queue lease.
    if kernel_backed {
        return;
    }
    let description_id = Arc::as_ptr(handles) as usize;
    let mut descriptions = registry.lock().unwrap_or_else(|error| error.into_inner());
    descriptions.retain(|_, description| description.handles.upgrade().is_some());
    descriptions
        .entry(description_id)
        .and_modify(|description| description.connected |= connected)
        .or_insert_with(|| HostNetTransferDescription {
            handles: Arc::downgrade(handles),
            connected,
        });
}

#[derive(Debug, Clone)]
pub(super) struct TransferredHostNetMetadata {
    domain: u32,
    socket_type: u32,
    protocol: u32,
    nonblocking: bool,
    recv_timeout_ms: Option<u64>,
    bind_options: Option<Value>,
    local_info: Option<Value>,
    local_unix_address: Option<String>,
    local_reservation: Option<String>,
    remote_info: Option<Value>,
    remote_unix_address: Option<String>,
    listening: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HostNetOpenDescriptionOptions {
    nonblocking: bool,
    recv_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum SpawnHostNetSource {
    Tcp(String),
    TcpListener(String),
    Udp(String),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ResolvedHostNetSourceClass {
    Tcp,
    Unix,
    TcpListener,
    UnixListener,
    Udp,
}

#[derive(Debug)]
pub(super) struct PreparedSpawnHostNetDescription {
    guest_fds: Vec<u32>,
    resource: TransferredHostNetSocket,
    metadata: Value,
}

#[derive(Debug, Default)]
pub(super) struct PreparedSpawnHostNetFds {
    descriptions: Vec<PreparedSpawnHostNetDescription>,
    kernel_actions: Vec<JavascriptPosixSpawnFileAction>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SpawnHostNetFdState {
    description: usize,
    close_on_exec: bool,
}

pub(super) const HOST_NET_AF_INET: u32 = 1;
pub(super) const HOST_NET_AF_INET6: u32 = 2;
pub(super) const HOST_NET_AF_UNIX: u32 = 3;
pub(super) const HOST_NET_SOCK_DGRAM: u32 = 5;
pub(super) const HOST_NET_SOCK_STREAM: u32 = 6;
pub(super) const HOST_NET_SOCKET_TYPE_MASK: u32 = 0x0f;
pub(super) const HOST_NET_SOCK_CLOEXEC: u32 = 0x2000;
pub(super) const HOST_NET_SOCK_NONBLOCK: u32 = 0x4000;
pub(super) const HOST_NET_IPPROTO_TCP: u32 = 6;
pub(super) const HOST_NET_IPPROTO_UDP: u32 = 17;
pub(super) const HOST_NET_METADATA_MAX_BYTES: usize = 16 * 1024;
pub(super) const HOST_NET_METADATA_MAX_STRING_BYTES: usize = 4 * 1024;
pub(super) const HOST_NET_RECV_TIMEOUT_MAX_MS: u64 = u32::MAX as u64;
pub(super) const LINUX_SCM_MAX_FD: usize = 253;

pub(super) fn validate_host_net_metadata_size(
    value: &Value,
    label: &str,
) -> Result<(), SidecarError> {
    let encoded_len = serde_json::to_vec(value)
        .map_err(|error| {
            SidecarError::InvalidState(format!("EINVAL: invalid {label} metadata: {error}"))
        })?
        .len();
    if encoded_len > HOST_NET_METADATA_MAX_BYTES {
        return Err(SidecarError::InvalidState(format!(
            "E2BIG: {label} metadata is {encoded_len} bytes, exceeding the {HOST_NET_METADATA_MAX_BYTES}-byte limit"
        )));
    }
    fn validate_strings(value: &Value, label: &str) -> Result<(), SidecarError> {
        match value {
            Value::String(value) if value.len() > HOST_NET_METADATA_MAX_STRING_BYTES => {
                Err(SidecarError::InvalidState(format!(
                    "ENAMETOOLONG: {label} metadata string exceeds {HOST_NET_METADATA_MAX_STRING_BYTES} bytes"
                )))
            }
            Value::Array(values) => {
                for value in values {
                    validate_strings(value, label)?;
                }
                Ok(())
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if key.len() > HOST_NET_METADATA_MAX_STRING_BYTES {
                        return Err(SidecarError::InvalidState(format!(
                            "ENAMETOOLONG: {label} metadata key exceeds {HOST_NET_METADATA_MAX_STRING_BYTES} bytes"
                        )));
                    }
                    validate_strings(value, label)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    validate_strings(value, label)
}

pub(super) fn host_net_open_description_options(
    value: &Value,
    label: &str,
) -> Result<HostNetOpenDescriptionOptions, SidecarError> {
    validate_host_net_metadata_size(value, label)?;
    let object = value.as_object().ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "EINVAL: host-network metadata must be an object",
        ))
    })?;
    let nonblocking = match object.get("nonblocking") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(SidecarError::InvalidState(format!(
                "EINVAL: {label} metadata nonblocking must be boolean"
            )))
        }
    };
    let recv_timeout_ms = match object.get("recvTimeoutMs") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let timeout = value.as_u64().ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "EINVAL: {label} metadata recvTimeoutMs must be a non-negative integer or null"
                ))
            })?;
            if timeout > HOST_NET_RECV_TIMEOUT_MAX_MS {
                return Err(SidecarError::InvalidState(format!(
                    "EINVAL: {label} metadata recvTimeoutMs exceeds {HOST_NET_RECV_TIMEOUT_MAX_MS}"
                )));
            }
            Some(timeout)
        }
    };
    Ok(HostNetOpenDescriptionOptions {
        nonblocking,
        recv_timeout_ms,
    })
}

pub(super) fn host_net_domain(address: &SocketAddr) -> u32 {
    if address.is_ipv4() {
        HOST_NET_AF_INET
    } else {
        HOST_NET_AF_INET6
    }
}

pub(super) fn host_net_address_info(address: SocketAddr) -> Value {
    json!({
        "address": address.ip().to_string(),
        "port": address.port(),
    })
}

pub(super) fn host_net_bind_options(address: SocketAddr) -> Value {
    json!({
        "host": address.ip().to_string(),
        "port": address.port(),
    })
}

pub(super) fn host_net_unix_options(
    path: Option<&str>,
    abstract_path_hex: Option<&str>,
) -> Option<Value> {
    if let Some(abstract_path_hex) = abstract_path_hex {
        Some(json!({ "abstractPathHex": abstract_path_hex }))
    } else {
        path.map(|path| json!({ "path": path }))
    }
}

pub(super) fn host_net_unix_address(path: Option<&str>, abstract_path_hex: Option<&str>) -> String {
    if let Some(abstract_path_hex) = abstract_path_hex {
        format!("unix-abstract:{}", abstract_path_hex.to_ascii_lowercase())
    } else if let Some(path) = path {
        format!("unix:{path}")
    } else {
        String::from("unix-unnamed")
    }
}

impl TransferredHostNetMetadata {
    fn tcp_socket(socket: &ActiveTcpSocket, options: HostNetOpenDescriptionOptions) -> Self {
        Self {
            domain: host_net_domain(&socket.guest_remote_addr),
            socket_type: HOST_NET_SOCK_STREAM,
            protocol: HOST_NET_IPPROTO_TCP,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: None,
            local_info: Some(host_net_address_info(socket.guest_local_addr)),
            local_unix_address: None,
            local_reservation: None,
            remote_info: Some(host_net_address_info(socket.guest_remote_addr)),
            remote_unix_address: None,
            listening: false,
        }
    }

    fn tcp_listener(listener: &ActiveTcpListener, options: HostNetOpenDescriptionOptions) -> Self {
        let local = listener.guest_local_addr();
        Self {
            domain: host_net_domain(&local),
            socket_type: HOST_NET_SOCK_STREAM,
            protocol: HOST_NET_IPPROTO_TCP,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: Some(host_net_bind_options(local)),
            local_info: Some(host_net_address_info(local)),
            local_unix_address: None,
            local_reservation: None,
            remote_info: None,
            remote_unix_address: None,
            listening: true,
        }
    }

    fn udp_socket(socket: &ActiveUdpSocket, options: HostNetOpenDescriptionOptions) -> Self {
        let domain = match socket.family {
            JavascriptUdpFamily::Ipv4 => HOST_NET_AF_INET,
            JavascriptUdpFamily::Ipv6 => HOST_NET_AF_INET6,
        };
        Self {
            domain,
            socket_type: HOST_NET_SOCK_DGRAM,
            protocol: HOST_NET_IPPROTO_UDP,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: socket.guest_local_addr.map(host_net_bind_options),
            local_info: socket.guest_local_addr.map(host_net_address_info),
            local_unix_address: None,
            local_reservation: None,
            remote_info: None,
            remote_unix_address: None,
            listening: false,
        }
    }

    fn unix_socket(socket: &ActiveUnixSocket, options: HostNetOpenDescriptionOptions) -> Self {
        Self {
            domain: HOST_NET_AF_UNIX,
            socket_type: HOST_NET_SOCK_STREAM,
            protocol: 0,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: host_net_unix_options(
                socket.local_path.as_deref(),
                socket.local_abstract_path_hex.as_deref(),
            ),
            local_info: None,
            local_unix_address: Some(host_net_unix_address(
                socket.local_path.as_deref(),
                socket.local_abstract_path_hex.as_deref(),
            )),
            local_reservation: None,
            remote_info: None,
            remote_unix_address: Some(host_net_unix_address(
                socket.remote_path.as_deref(),
                socket.remote_abstract_path_hex.as_deref(),
            )),
            listening: false,
        }
    }

    fn unix_listener(
        listener: &ActiveUnixListener,
        options: HostNetOpenDescriptionOptions,
    ) -> Self {
        Self {
            domain: HOST_NET_AF_UNIX,
            socket_type: HOST_NET_SOCK_STREAM,
            protocol: 0,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: host_net_unix_options(
                Some(listener.path.as_str()),
                listener.abstract_path_hex.as_deref(),
            ),
            local_info: None,
            local_unix_address: Some(host_net_unix_address(
                Some(listener.path.as_str()),
                listener.abstract_path_hex.as_deref(),
            )),
            local_reservation: None,
            remote_info: None,
            remote_unix_address: None,
            listening: listener.listener.is_some(),
        }
    }

    pub(super) fn pending(
        value: &Value,
        options: HostNetOpenDescriptionOptions,
        label: &str,
    ) -> Result<Self, SidecarError> {
        let object = value
            .as_object()
            .expect("open-description options validated object");
        let domain = required_host_net_u32(object, "domain", label)?;
        let raw_socket_type = required_host_net_u32(object, "socketType", label)?;
        if raw_socket_type
            & !(HOST_NET_SOCKET_TYPE_MASK | HOST_NET_SOCK_NONBLOCK | HOST_NET_SOCK_CLOEXEC)
            != 0
        {
            return Err(SidecarError::InvalidState(format!(
                "EINVAL: {label} metadata socketType contains unsupported flags"
            )));
        }
        let socket_type = raw_socket_type & HOST_NET_SOCKET_TYPE_MASK;
        let requested_protocol = required_host_net_u32(object, "protocol", label)?;
        let protocol = match (domain, socket_type, requested_protocol) {
            (HOST_NET_AF_INET | HOST_NET_AF_INET6, HOST_NET_SOCK_STREAM, 0 | HOST_NET_IPPROTO_TCP) => {
                HOST_NET_IPPROTO_TCP
            }
            (HOST_NET_AF_INET | HOST_NET_AF_INET6, HOST_NET_SOCK_DGRAM, 0 | HOST_NET_IPPROTO_UDP) => {
                HOST_NET_IPPROTO_UDP
            }
            (HOST_NET_AF_UNIX, HOST_NET_SOCK_STREAM, 0) => 0,
            _ => {
                return Err(SidecarError::InvalidState(format!(
                    "EPROTONOSUPPORT: {label} metadata does not describe a supported unconnected socket"
                )))
            }
        };
        let metadata = Self {
            domain,
            socket_type,
            protocol,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: None,
            local_info: None,
            local_unix_address: (domain == HOST_NET_AF_UNIX).then(|| String::from("unix-unnamed")),
            local_reservation: None,
            remote_info: None,
            remote_unix_address: None,
            listening: false,
        };
        validate_host_net_metadata(value, &metadata, "pending", label)?;
        Ok(metadata)
    }

    fn as_value(&self) -> Value {
        json!({
            "domain": self.domain,
            "socketType": self.socket_type,
            "protocol": self.protocol,
            "nonblocking": self.nonblocking,
            "recvTimeoutMs": self.recv_timeout_ms,
            "bindOptions": self.bind_options,
            "localInfo": self.local_info,
            "localUnixAddress": self.local_unix_address,
            "localReservation": self.local_reservation,
            "remoteInfo": self.remote_info,
            "remoteUnixAddress": self.remote_unix_address,
            "listening": self.listening,
        })
    }
}

pub(super) fn required_host_net_u32(
    object: &Map<String, Value>,
    name: &str,
    label: &str,
) -> Result<u32, SidecarError> {
    object
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("EINVAL: {label} metadata field {name} must be u32"))
        })
}

pub(super) fn validate_host_net_metadata(
    value: &Value,
    expected: &TransferredHostNetMetadata,
    expected_class: &str,
    label: &str,
) -> Result<(), SidecarError> {
    let options = host_net_open_description_options(value, label)?;
    if options.nonblocking != expected.nonblocking
        || options.recv_timeout_ms != expected.recv_timeout_ms
    {
        return Err(host_net_metadata_mismatch(label, "open-description state"));
    }
    let object = value.as_object().ok_or_else(|| {
        SidecarError::InvalidState(format!("EINVAL: {label} metadata must be an object"))
    })?;
    let domain = required_host_net_u32(object, "domain", label)?;
    if domain != expected.domain {
        return Err(host_net_metadata_mismatch(label, "domain"));
    }
    let socket_type = required_host_net_u32(object, "socketType", label)?;
    if socket_type & !(HOST_NET_SOCKET_TYPE_MASK | HOST_NET_SOCK_NONBLOCK | HOST_NET_SOCK_CLOEXEC)
        != 0
        || socket_type & HOST_NET_SOCKET_TYPE_MASK != expected.socket_type
    {
        return Err(SidecarError::InvalidState(format!(
            "EINVAL: {label} metadata socketType {socket_type:#x} does not match the sidecar-owned socket type {:#x}",
            expected.socket_type
        )));
    }
    let protocol = required_host_net_u32(object, "protocol", label)?;
    if protocol != 0 && protocol != expected.protocol {
        return Err(host_net_metadata_mismatch(label, "protocol"));
    }
    if let Some(class) = object.get("class") {
        if class.as_str() != Some(expected_class) {
            return Err(host_net_metadata_mismatch(label, "class"));
        }
    }
    let expected_value = expected.as_value();
    let expected_object = expected_value
        .as_object()
        .expect("canonical host-network metadata is an object");
    for name in [
        "bindOptions",
        "localInfo",
        "localUnixAddress",
        "localReservation",
        "remoteInfo",
        "remoteUnixAddress",
        "listening",
    ] {
        let actual = object.get(name).unwrap_or(&Value::Null);
        let canonical = expected_object.get(name).unwrap_or(&Value::Null);
        if actual != canonical {
            return Err(host_net_metadata_mismatch(label, name));
        }
    }
    Ok(())
}

pub(super) fn host_net_metadata_mismatch(label: &str, field: &str) -> SidecarError {
    SidecarError::InvalidState(format!(
        "EINVAL: {label} metadata {field} does not match the sidecar-owned socket"
    ))
}

pub(super) fn spawn_host_net_source(
    fd: &JavascriptSpawnHostNetFd,
) -> Result<SpawnHostNetSource, SidecarError> {
    let mut sources = Vec::new();
    if let Some(id) = fd.socket_id.as_deref().filter(|id| !id.is_empty()) {
        validate_host_net_resource_id(id, "inherited socket id")?;
        sources.push(SpawnHostNetSource::Tcp(id.to_owned()));
    }
    if let Some(id) = fd.server_id.as_deref().filter(|id| !id.is_empty()) {
        validate_host_net_resource_id(id, "inherited listener id")?;
        sources.push(SpawnHostNetSource::TcpListener(id.to_owned()));
    }
    if let Some(id) = fd.udp_socket_id.as_deref().filter(|id| !id.is_empty()) {
        validate_host_net_resource_id(id, "inherited UDP socket id")?;
        sources.push(SpawnHostNetSource::Udp(id.to_owned()));
    }
    if sources.len() != 1 {
        return Err(SidecarError::InvalidState(String::from(
            "EINVAL: inherited host-network fd requires exactly one resource id",
        )));
    }
    Ok(sources.pop().expect("one source checked"))
}

pub(super) fn validate_host_net_resource_id(id: &str, label: &str) -> Result<(), SidecarError> {
    if id.len() > 256 {
        return Err(SidecarError::InvalidState(format!(
            "ENAMETOOLONG: {label} exceeds 256 bytes"
        )));
    }
    Ok(())
}

pub(super) fn scm_rights_host_net_source(
    value: &Value,
) -> Result<Option<SpawnHostNetSource>, SidecarError> {
    validate_host_net_metadata_size(value, "SCM_RIGHTS host-network")?;
    let object = value.as_object().ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "EINVAL: SCM_RIGHTS host-network entry must be an object",
        ))
    })?;
    let mut sources = Vec::new();
    for (name, source) in [("socketId", 0u8), ("serverId", 1u8), ("udpSocketId", 2u8)] {
        let Some(value) = object.get(name) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let id = value.as_str().filter(|id| !id.is_empty()).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "EINVAL: SCM_RIGHTS host-network {name} must be a non-empty string or null"
            ))
        })?;
        validate_host_net_resource_id(id, name)?;
        sources.push(match source {
            0 => SpawnHostNetSource::Tcp(id.to_owned()),
            1 => SpawnHostNetSource::TcpListener(id.to_owned()),
            2 => SpawnHostNetSource::Udp(id.to_owned()),
            _ => unreachable!(),
        });
    }
    if sources.len() > 1 {
        return Err(SidecarError::InvalidState(String::from(
            "EINVAL: SCM_RIGHTS host-network entry requires at most one resource id",
        )));
    }
    Ok(sources.pop())
}

pub(super) fn posix_spawn_action_guest_fd(
    action: &JavascriptPosixSpawnFileAction,
    label: &str,
) -> Result<u32, SidecarError> {
    u32::try_from(action.guest_fd.unwrap_or(action.fd)).map_err(|_| {
        SidecarError::InvalidState(format!(
            "EBADF: invalid posix_spawn {label} fd {}",
            action.guest_fd.unwrap_or(action.fd)
        ))
    })
}

pub(super) fn posix_spawn_action_guest_source_fd(
    action: &JavascriptPosixSpawnFileAction,
) -> Result<u32, SidecarError> {
    u32::try_from(action.guest_source_fd.unwrap_or(action.source_fd)).map_err(|_| {
        SidecarError::InvalidState(format!(
            "EBADF: invalid posix_spawn dup2 source {}",
            action.guest_source_fd.unwrap_or(action.source_fd)
        ))
    })
}

impl PreparedSpawnHostNetFds {
    fn inherited_fd_count(&self) -> usize {
        self.descriptions
            .iter()
            .map(|description| description.guest_fds.len())
            .sum()
    }

    fn bootstrap_json(&self) -> Value {
        Value::Array(
            self.descriptions
                .iter()
                .enumerate()
                .map(|(index, description)| {
                    let mut value = json!({
                        "guestFds": description.guest_fds,
                        "metadata": description.metadata,
                    });
                    let object = value
                        .as_object_mut()
                        .expect("spawn host-network bootstrap value is an object");
                    let (key, id) = match description.resource {
                        TransferredHostNetSocket::Tcp { .. } => {
                            ("socketId", format!("spawn-tcp-{index}"))
                        }
                        TransferredHostNetSocket::TcpListener { .. } => {
                            ("serverId", format!("spawn-listener-{index}"))
                        }
                        TransferredHostNetSocket::Udp { .. } => {
                            ("udpSocketId", format!("spawn-udp-{index}"))
                        }
                        TransferredHostNetSocket::Unix { .. } => {
                            ("socketId", format!("spawn-unix-{index}"))
                        }
                        TransferredHostNetSocket::UnixListener { .. } => {
                            ("serverId", format!("spawn-unix-listener-{index}"))
                        }
                        TransferredHostNetSocket::Pending { .. } => unreachable!(
                            "pending host-network descriptions are rejected before spawn"
                        ),
                    };
                    object.insert(key.to_owned(), Value::String(id));
                    value
                })
                .collect(),
        )
    }

    fn install(self, child: &mut ActiveProcess) {
        for (index, description) in self.descriptions.into_iter().enumerate() {
            match description.resource {
                TransferredHostNetSocket::Tcp { mut socket, .. } => {
                    socket.listener_id = None;
                    child
                        .tcp_sockets
                        .insert(format!("spawn-tcp-{index}"), *socket);
                }
                TransferredHostNetSocket::TcpListener { listener, .. } => {
                    child
                        .tcp_listeners
                        .insert(format!("spawn-listener-{index}"), listener);
                }
                TransferredHostNetSocket::Udp { socket, .. } => {
                    child
                        .udp_sockets
                        .insert(format!("spawn-udp-{index}"), socket);
                }
                TransferredHostNetSocket::Unix { mut socket, .. } => {
                    socket.listener_id = None;
                    child
                        .unix_sockets
                        .insert(format!("spawn-unix-{index}"), socket);
                }
                TransferredHostNetSocket::UnixListener { listener, .. } => {
                    child
                        .unix_listeners
                        .insert(format!("spawn-unix-listener-{index}"), listener);
                }
                TransferredHostNetSocket::Pending { .. } => {
                    unreachable!("pending host-network descriptions are rejected before spawn")
                }
            }
        }
    }
}

pub(super) fn transferred_hostnet_value(
    class: &str,
    metadata: TransferredHostNetMetadata,
    id: Option<(&str, String)>,
    capability_identity: Option<(
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::CapabilityGeneration,
    )>,
    local: Option<SocketAddr>,
    remote: Option<SocketAddr>,
) -> Value {
    let mut value = json!({
        "kind": "hostNet",
        "class": class,
        "domain": metadata.domain,
        "socketType": metadata.socket_type,
        "protocol": metadata.protocol,
        "nonblocking": metadata.nonblocking,
        "recvTimeoutMs": metadata.recv_timeout_ms,
        "bindOptions": metadata.bind_options,
        "localInfo": metadata.local_info,
        "localUnixAddress": metadata.local_unix_address,
        "localReservation": metadata.local_reservation,
        "remoteInfo": metadata.remote_info,
        "remoteUnixAddress": metadata.remote_unix_address,
        "listening": metadata.listening,
    });
    let object = value
        .as_object_mut()
        .expect("transferred host-net value is an object");
    if let Some((key, id)) = id {
        object.insert(key.to_owned(), Value::String(id));
    }
    if let Some((capability_id, capability_generation)) = capability_identity {
        object.insert(String::from("capabilityId"), Value::from(capability_id));
        object.insert(
            String::from("capabilityGeneration"),
            Value::from(capability_generation),
        );
    }
    if let Some(local) = local {
        object.insert(
            String::from("localAddress"),
            Value::String(local.ip().to_string()),
        );
        object.insert(String::from("localPort"), Value::from(local.port()));
    }
    if let Some(remote) = remote {
        object.insert(
            String::from("remoteAddress"),
            Value::String(remote.ip().to_string()),
        );
        object.insert(String::from("remotePort"), Value::from(remote.port()));
    }
    value
}

pub(super) fn adopt_kernel_socket_transfer_guard(
    kernel: &mut SidecarKernel,
    pid: u32,
    socket_id: SocketId,
    nonblocking: bool,
) -> Result<agentos_kernel::fd_table::TransferredFd, SidecarError> {
    let flags = if nonblocking {
        agentos_kernel::fd_table::O_NONBLOCK
    } else {
        0
    };
    kernel
        .fd_adopt_socket_transfer(EXECUTION_DRIVER_NAME, pid, socket_id, flags)
        .map_err(kernel_error)
}

pub(super) fn prepare_transferred_host_net_resource(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    source: &SpawnHostNetSource,
    value: &Value,
    label: &str,
) -> Result<TransferredHostNetSocket, SidecarError> {
    // Resolve the sidecar-owned resource before reading any guest-controlled
    // metadata. Metadata may describe open-description flags, but it never
    // selects the resource class or lifecycle.
    let resolved_class = match source {
        SpawnHostNetSource::Tcp(socket_id) if process.tcp_sockets.contains_key(socket_id) => {
            ResolvedHostNetSourceClass::Tcp
        }
        SpawnHostNetSource::Tcp(socket_id) if process.unix_sockets.contains_key(socket_id) => {
            ResolvedHostNetSourceClass::Unix
        }
        SpawnHostNetSource::Tcp(socket_id) => {
            return Err(SidecarError::InvalidState(format!(
                "EBADF: unknown transferable socket {socket_id}"
            )))
        }
        SpawnHostNetSource::TcpListener(listener_id)
            if process.tcp_listeners.contains_key(listener_id) =>
        {
            ResolvedHostNetSourceClass::TcpListener
        }
        SpawnHostNetSource::TcpListener(listener_id)
            if process.unix_listeners.contains_key(listener_id) =>
        {
            ResolvedHostNetSourceClass::UnixListener
        }
        SpawnHostNetSource::TcpListener(listener_id) => {
            return Err(SidecarError::InvalidState(format!(
                "EBADF: unknown transferable listener {listener_id}"
            )))
        }
        SpawnHostNetSource::Udp(socket_id) if process.udp_sockets.contains_key(socket_id) => {
            ResolvedHostNetSourceClass::Udp
        }
        SpawnHostNetSource::Udp(socket_id) => {
            return Err(SidecarError::InvalidState(format!(
                "EBADF: unknown transferable UDP socket {socket_id}"
            )))
        }
    };
    let options = host_net_open_description_options(value, label)?;
    let resource = match (source, resolved_class) {
        (SpawnHostNetSource::Tcp(socket_id), ResolvedHostNetSourceClass::Tcp) => {
            let socket = process
                .tcp_sockets
                .get_mut(socket_id)
                .expect("resolved TCP socket remains present");
            let metadata = TransferredHostNetMetadata::tcp_socket(socket, options);
            validate_host_net_metadata(value, &metadata, "tcp", label)?;
            if socket.kernel_transfer_guard.is_none() {
                if let Some(kernel_socket_id) = socket.kernel_socket_id {
                    socket.kernel_transfer_guard = Some(adopt_kernel_socket_transfer_guard(
                        kernel,
                        process.kernel_pid,
                        kernel_socket_id,
                        options.nonblocking,
                    )?);
                }
            }
            TransferredHostNetSocket::Tcp {
                socket: Box::new(socket.clone_for_fd_transfer()),
                metadata,
            }
        }
        (SpawnHostNetSource::Tcp(socket_id), ResolvedHostNetSourceClass::Unix) => {
            let socket = process
                .unix_sockets
                .get(socket_id)
                .expect("resolved Unix socket remains present");
            let metadata = TransferredHostNetMetadata::unix_socket(socket, options);
            validate_host_net_metadata(value, &metadata, "unix", label)?;
            TransferredHostNetSocket::Unix {
                socket: socket.clone_for_fd_transfer(),
                metadata,
            }
        }
        (SpawnHostNetSource::TcpListener(listener_id), ResolvedHostNetSourceClass::TcpListener) => {
            let listener = process
                .tcp_listeners
                .get_mut(listener_id)
                .expect("resolved TCP listener remains present");
            let metadata = TransferredHostNetMetadata::tcp_listener(listener, options);
            validate_host_net_metadata(value, &metadata, "listener", label)?;
            if listener.kernel_transfer_guard.is_none() {
                if let Some(kernel_socket_id) = listener.kernel_socket_id {
                    listener.kernel_transfer_guard = Some(adopt_kernel_socket_transfer_guard(
                        kernel,
                        process.kernel_pid,
                        kernel_socket_id,
                        options.nonblocking,
                    )?);
                }
            }
            TransferredHostNetSocket::TcpListener {
                listener: listener.clone_for_fd_transfer()?,
                metadata,
            }
        }
        (
            SpawnHostNetSource::TcpListener(listener_id),
            ResolvedHostNetSourceClass::UnixListener,
        ) => {
            let listener = process
                .unix_listeners
                .get(listener_id)
                .expect("resolved Unix listener remains present");
            let metadata = TransferredHostNetMetadata::unix_listener(listener, options);
            validate_host_net_metadata(value, &metadata, "unix-listener", label)?;
            TransferredHostNetSocket::UnixListener {
                listener: listener.clone_for_fd_transfer()?,
                metadata,
            }
        }
        (SpawnHostNetSource::Udp(socket_id), ResolvedHostNetSourceClass::Udp) => {
            let socket = process.udp_sockets.get_mut(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "EBADF: unknown transferable UDP socket {socket_id}"
                ))
            })?;
            let metadata = TransferredHostNetMetadata::udp_socket(socket, options);
            validate_host_net_metadata(value, &metadata, "udp", label)?;
            if socket.kernel_transfer_guard.is_none() {
                if let Some(kernel_socket_id) = socket.kernel_socket_id {
                    socket.kernel_transfer_guard = Some(adopt_kernel_socket_transfer_guard(
                        kernel,
                        process.kernel_pid,
                        kernel_socket_id,
                        options.nonblocking,
                    )?);
                }
            }
            TransferredHostNetSocket::Udp {
                socket: socket.clone_for_fd_transfer()?,
                metadata,
            }
        }
        _ => unreachable!("resource source and resolved class must agree"),
    };
    Ok(resource)
}

pub(super) const POSIX_SPAWN_SETPGROUP: u32 = 1 << 1;
pub(super) const POSIX_SPAWN_SETSCHEDPARAM: u32 = 1 << 4;
pub(super) const POSIX_SPAWN_SETSCHEDULER: u32 = 1 << 5;
pub(super) const POSIX_SPAWN_SETSID: u32 = 1 << 7;
pub(super) const SUPPORTED_POSIX_SPAWN_FLAGS: u32 = (1 << 0)
    | POSIX_SPAWN_SETPGROUP
    | (1 << 2)
    | (1 << 3)
    | POSIX_SPAWN_SETSCHEDPARAM
    | POSIX_SPAWN_SETSCHEDULER
    | (1 << 6)
    | POSIX_SPAWN_SETSID;

pub(super) fn kernel_open_flags_from_wasi(oflag: i32) -> u32 {
    let oflag = oflag as u32;
    let mut flags = if oflag & 0x1000_0000 != 0 {
        if oflag & 0x0400_0000 != 0 {
            agentos_kernel::fd_table::O_RDWR
        } else {
            agentos_kernel::fd_table::O_WRONLY
        }
    } else {
        agentos_kernel::fd_table::O_RDONLY
    };
    if oflag & 0x0000_0001 != 0 {
        flags |= agentos_kernel::fd_table::O_APPEND;
    }
    if oflag & 0x0000_0004 != 0 {
        flags |= agentos_kernel::fd_table::O_NONBLOCK;
    }
    if oflag & (1 << 12) != 0 {
        flags |= agentos_kernel::fd_table::O_CREAT;
    }
    if oflag & (2 << 12) != 0 {
        flags |= agentos_kernel::fd_table::O_DIRECTORY;
    }
    if oflag & (4 << 12) != 0 {
        flags |= agentos_kernel::fd_table::O_EXCL;
    }
    if oflag & (8 << 12) != 0 {
        flags |= agentos_kernel::fd_table::O_TRUNC;
    }
    if oflag & 0x0100_0000 != 0 {
        flags |= agentos_kernel::fd_table::O_NOFOLLOW;
    }
    flags
}

#[derive(Default)]
pub(super) struct AppliedPosixSpawnFileActions {
    fd_mappings: Vec<[u32; 2]>,
    closed_guest_fds: Vec<u32>,
}

/// JavaScript and Python issue stdio bridge calls with the POSIX fd numbers
/// 0/1/2 directly. Unlike the WASM runner, they do not consume the
/// guest-to-kernel fd mapping emitted by posix_spawn file actions. Install the
/// mapped descriptions at their canonical stdio numbers before execution
/// starts so pipes and redirections observe the same descriptors as the guest.
fn materialize_direct_runtime_stdio_mappings(
    kernel: &mut SidecarKernel,
    pid: u32,
    applied: &AppliedPosixSpawnFileActions,
) -> Result<(), SidecarError> {
    for guest_fd in 0..=2 {
        if let Some(source_fd) = applied
            .fd_mappings
            .iter()
            .find_map(|mapping| (mapping[0] == guest_fd).then_some(mapping[1]))
        {
            if source_fd != guest_fd {
                kernel
                    .fd_dup2(EXECUTION_DRIVER_NAME, pid, source_fd, guest_fd)
                    .map_err(kernel_error)?;
            }
        } else if applied.closed_guest_fds.contains(&guest_fd) {
            if let Err(error) = kernel.fd_close(EXECUTION_DRIVER_NAME, pid, guest_fd) {
                if error.code() != "EBADF" {
                    return Err(kernel_error(error));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod direct_runtime_stdio_mapping_tests {
    use super::*;
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::{KernelVmConfig, SpawnOptions};
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::MemoryFileSystem;

    #[test]
    fn materializes_guest_fd_mappings_at_canonical_fds() {
        let mut config = KernelVmConfig::new("vm-python-stdio-mappings");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .unwrap();
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        let (read_fd, write_fd) = kernel
            .open_pipe(EXECUTION_DRIVER_NAME, process.pid())
            .unwrap();
        kernel
            .fd_write(
                EXECUTION_DRIVER_NAME,
                process.pid(),
                write_fd,
                b"python-stdin",
            )
            .unwrap();

        materialize_direct_runtime_stdio_mappings(
            &mut kernel,
            process.pid(),
            &AppliedPosixSpawnFileActions {
                fd_mappings: vec![[0, read_fd]],
                closed_guest_fds: vec![1],
            },
        )
        .unwrap();

        assert_eq!(
            kernel
                .fd_read(EXECUTION_DRIVER_NAME, process.pid(), 0, 64)
                .unwrap(),
            b"python-stdin"
        );
        assert_eq!(
            kernel
                .fd_path(EXECUTION_DRIVER_NAME, process.pid(), 1)
                .expect_err("closed stdout must remain closed")
                .code(),
            "EBADF"
        );
    }

    fn assert_closed_stdin_canonicalization_is_idempotent(materialize_direct_runtime_first: bool) {
        let mut config = KernelVmConfig::new("vm-closed-stdin-canonicalization");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .unwrap();
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        let close_stdin = JavascriptPosixSpawnFileAction {
            command: 1,
            guest_fd: Some(0),
            fd: 0,
            source_fd: -1,
            guest_source_fd: None,
            oflag: 0,
            mode: 0,
            path: String::new(),
            close_from_guest_fds: Vec::new(),
        };
        let (applied, _) =
            apply_posix_spawn_file_actions(&mut kernel, process.pid(), "/", &[], &[close_stdin])
                .expect("apply close(0) file action");

        if materialize_direct_runtime_first {
            materialize_direct_runtime_stdio_mappings(&mut kernel, process.pid(), &applied)
                .expect("direct-runtime stdio materialization accepts already-closed stdin");
        }
        canonicalize_host_runtime_posix_stdin(&mut kernel, process.pid(), &applied)
            .expect("host runtime canonicalization accepts already-closed stdin");
        assert_eq!(
            kernel
                .fd_path(EXECUTION_DRIVER_NAME, process.pid(), 0)
                .expect_err("stdin must remain closed")
                .code(),
            "EBADF"
        );
    }

    #[test]
    fn canonicalization_accepts_posix_spawn_close_stdin() {
        assert_closed_stdin_canonicalization_is_idempotent(false);
    }

    #[test]
    fn direct_runtime_launch_accepts_posix_spawn_close_stdin() {
        assert_closed_stdin_canonicalization_is_idempotent(true);
    }
}

pub(super) struct PreparedPosixSpawnFd {
    fd: u32,
    fd_flags: u32,
    transfer: TransferredFd,
}

pub(super) struct PreparedPosixSpawnFileActions {
    applied: AppliedPosixSpawnFileActions,
    fds: Vec<PreparedPosixSpawnFd>,
    cwd: String,
}

pub(super) fn prepare_spawn_host_net_fds(
    kernel: &mut SidecarKernel,
    parent: &mut ActiveProcess,
    current_network_counts: NetworkResourceCounts,
    inherited_fds: &[JavascriptSpawnHostNetFd],
    inherited_kernel_mappings: &[[u32; 2]],
    actions: &[JavascriptPosixSpawnFileAction],
) -> Result<PreparedSpawnHostNetFds, SidecarError> {
    const LINUX_GUEST_FD_LIMIT: u32 = 1 << 20;
    if let Some(limit) = kernel.resource_limits().max_open_fds {
        if inherited_fds.len() > limit {
            return Err(SidecarError::InvalidState(format!(
                "EMFILE: inherited host-network fd list has {} entries, exceeding limits.resources.maxOpenFds ({limit}); raise limits.resources.maxOpenFds",
                inherited_fds.len()
            )));
        }
    }

    let inherited_kernel_guest_fds = inherited_kernel_mappings
        .iter()
        .map(|mapping| mapping[0])
        .collect::<BTreeSet<_>>();
    let mut fd_states = BTreeMap::<u32, SpawnHostNetFdState>::new();
    let mut source_descriptions = BTreeMap::<SpawnHostNetSource, usize>::new();
    let mut description_metadata = Vec::<Value>::new();
    let mut description_resources = Vec::<Option<TransferredHostNetSocket>>::new();

    for inherited in inherited_fds {
        if inherited.guest_fd >= LINUX_GUEST_FD_LIMIT {
            return Err(SidecarError::InvalidState(format!(
                "EBADF: inherited host-network guest fd {} exceeds the Linux descriptor limit",
                inherited.guest_fd
            )));
        }
        if inherited_kernel_guest_fds.contains(&inherited.guest_fd)
            || fd_states.contains_key(&inherited.guest_fd)
        {
            return Err(SidecarError::InvalidState(format!(
                "EINVAL: duplicate inherited guest fd {}",
                inherited.guest_fd
            )));
        }

        let source = spawn_host_net_source(inherited)?;
        let description = if let Some(index) = source_descriptions.get(&source).copied() {
            let existing = description_resources[index]
                .as_ref()
                .expect("spawn host-network description resource exists");
            if validate_host_net_metadata(
                &inherited.metadata,
                existing.metadata(),
                existing.class(),
                "spawn host-network",
            )
            .is_err()
            {
                return Err(SidecarError::InvalidState(String::from(
                    "EINVAL: aliases of one inherited host-network description disagree on metadata",
                )));
            }
            index
        } else {
            let resource = prepare_transferred_host_net_resource(
                kernel,
                parent,
                &source,
                &inherited.metadata,
                "spawn host-network",
            )?;
            let index = description_resources.len();
            source_descriptions.insert(source, index);
            description_metadata.push(resource.metadata().as_value());
            description_resources.push(Some(resource));
            index
        };
        fd_states.insert(
            inherited.guest_fd,
            SpawnHostNetFdState {
                description,
                close_on_exec: inherited.close_on_exec,
            },
        );
    }

    let kernel_actions = apply_spawn_host_net_file_actions(&mut fd_states, actions)?;
    fd_states.retain(|_, state| !state.close_on_exec);

    // fork/exec inheritance installs new descriptor references to the same
    // Linux open-file descriptions. maxOpenFds bounds those references; the
    // socket/connection limits continue to count each description once.
    check_spawn_host_net_resource_limit(
        kernel.resource_limits().max_sockets,
        current_network_counts.sockets,
        0,
        "EMFILE",
        "socket descriptions",
        "maxSockets",
    )?;
    check_spawn_host_net_resource_limit(
        kernel.resource_limits().max_connections,
        current_network_counts.connections,
        0,
        "EAGAIN",
        "connected socket descriptions",
        "maxConnections",
    )?;

    let mut final_guest_fds = vec![Vec::new(); description_resources.len()];
    for (guest_fd, state) in fd_states {
        final_guest_fds[state.description].push(guest_fd);
    }
    let mut descriptions = Vec::new();
    for (index, guest_fds) in final_guest_fds.into_iter().enumerate() {
        if guest_fds.is_empty() {
            continue;
        }
        descriptions.push(PreparedSpawnHostNetDescription {
            guest_fds,
            resource: description_resources[index]
                .take()
                .expect("spawn host-network description resource exists"),
            metadata: description_metadata[index].clone(),
        });
    }
    Ok(PreparedSpawnHostNetFds {
        descriptions,
        kernel_actions,
    })
}

pub(super) fn check_spawn_host_net_resource_limit(
    limit: Option<usize>,
    current: usize,
    additional: usize,
    errno: &str,
    label: &str,
    config_name: &str,
) -> Result<(), SidecarError> {
    let Some(limit) = limit else {
        return Ok(());
    };
    let requested = current.saturating_add(additional);
    if additional > 0 && requested > limit {
        return Err(SidecarError::InvalidState(format!(
            "{errno}: inheriting {additional} host-network {label} would raise recursive VM usage from {current} to {requested}, exceeding limits.resources.{config_name} ({limit}); raise limits.resources.{config_name}"
        )));
    }
    Ok(())
}

pub(super) fn apply_spawn_host_net_file_actions(
    fd_states: &mut BTreeMap<u32, SpawnHostNetFdState>,
    actions: &[JavascriptPosixSpawnFileAction],
) -> Result<Vec<JavascriptPosixSpawnFileAction>, SidecarError> {
    let mut kernel_actions = Vec::with_capacity(actions.len());
    for action in actions {
        match action.command {
            1 => {
                let guest_fd = posix_spawn_action_guest_fd(action, "close")?;
                let removed_host_net = fd_states.remove(&guest_fd).is_some();
                if !removed_host_net || guest_fd <= 2 {
                    kernel_actions.push(action.clone());
                }
            }
            2 => {
                let guest_fd = posix_spawn_action_guest_fd(action, "dup2 target")?;
                let source_fd = posix_spawn_action_guest_source_fd(action)?;
                if let Some(mut state) = fd_states.get(&source_fd).copied() {
                    // POSIX spawn dup2 actions clear FD_CLOEXEC even for a
                    // same-fd action; direct dup2(2) remains a no-op.
                    state.close_on_exec = false;
                    if guest_fd == source_fd {
                        fd_states.insert(guest_fd, state);
                        continue;
                    }
                    fd_states.insert(guest_fd, state);
                    let mut close_target = action.clone();
                    close_target.command = 1;
                    close_target.guest_fd = Some(i32::try_from(guest_fd).map_err(|_| {
                        SidecarError::InvalidState(format!(
                            "EBADF: posix_spawn dup2 target {guest_fd} exceeds i32"
                        ))
                    })?);
                    close_target.source_fd = 0;
                    close_target.guest_source_fd = None;
                    close_target.path.clear();
                    close_target.close_from_guest_fds.clear();
                    kernel_actions.push(close_target);
                } else {
                    fd_states.remove(&guest_fd);
                    kernel_actions.push(action.clone());
                }
            }
            3 => {
                let guest_fd = posix_spawn_action_guest_fd(action, "open")?;
                fd_states.remove(&guest_fd);
                kernel_actions.push(action.clone());
            }
            4 => kernel_actions.push(action.clone()),
            5 => {
                let guest_fd = posix_spawn_action_guest_fd(action, "fchdir")?;
                if fd_states.contains_key(&guest_fd) {
                    return Err(SidecarError::InvalidState(format!(
                        "ENOTDIR: posix_spawn fchdir fd {guest_fd} is a socket"
                    )));
                }
                kernel_actions.push(action.clone());
            }
            6 => {
                let low_fd = posix_spawn_action_guest_fd(action, "closefrom")?;
                fd_states.retain(|guest_fd, _| *guest_fd < low_fd);
                kernel_actions.push(action.clone());
            }
            command => {
                return Err(SidecarError::InvalidState(format!(
                    "EINVAL: unknown posix_spawn file action {command}"
                )));
            }
        }
    }
    Ok(kernel_actions)
}

pub(super) fn apply_posix_spawn_file_actions(
    kernel: &mut SidecarKernel,
    pid: u32,
    initial_cwd: &str,
    inherited_mappings: &[[u32; 2]],
    actions: &[JavascriptPosixSpawnFileAction],
) -> Result<(AppliedPosixSpawnFileActions, String), SidecarError> {
    let inherited_kernel_fds = kernel
        .fd_snapshot(EXECUTION_DRIVER_NAME, pid)
        .map_err(kernel_error)?
        .into_iter()
        .map(|entry| entry.fd)
        .collect::<BTreeSet<_>>();
    let mut mappings = BTreeMap::new();
    let mut mapped_kernel_fds = BTreeSet::new();
    let mut closed_guest_fds = BTreeSet::new();
    let mut cwd = kernel
        .realpath_for_process(EXECUTION_DRIVER_NAME, pid, initial_cwd)
        .map(|path| normalize_path(&path))
        .map_err(kernel_error)?;
    for [guest_fd, kernel_fd] in inherited_mappings {
        // Runner-local descriptors can appear in the guest mapping without a
        // kernel entry. Kernel-backed FD_CLOEXEC descriptors remain present
        // until file actions finish, matching fork + exec ordering on Linux.
        if !inherited_kernel_fds.contains(kernel_fd) {
            continue;
        }
        if mappings.insert(*guest_fd, *kernel_fd).is_some() || !mapped_kernel_fds.insert(*kernel_fd)
        {
            return Err(SidecarError::InvalidState(String::from(
                "EINVAL: duplicate posix_spawn guest/kernel fd mapping",
            )));
        }
    }

    let action_guest_fd = |action: &JavascriptPosixSpawnFileAction| {
        u32::try_from(action.guest_fd.unwrap_or(action.fd)).map_err(|_| {
            SidecarError::InvalidState(format!(
                "EBADF: invalid posix_spawn guest fd {}",
                action.guest_fd.unwrap_or(action.fd)
            ))
        })
    };
    for action in actions {
        match action.command {
            1 => {
                let guest_fd = action_guest_fd(action)?;
                closed_guest_fds.insert(guest_fd);
                let fd = if let Some(fd) = mappings.remove(&guest_fd) {
                    Some(fd)
                } else if action.guest_fd.is_some() && guest_fd > 2 {
                    // Runner-local descriptors do not exist in the kernel
                    // namespace; never fall through to an unrelated canonical
                    // fd with the same number.
                    None
                } else {
                    let raw_fd = u32::try_from(action.fd).map_err(|_| {
                        SidecarError::InvalidState(format!(
                            "EBADF: invalid posix_spawn close fd {}",
                            action.fd
                        ))
                    })?;
                    if mapped_kernel_fds.contains(&raw_fd) {
                        return Err(SidecarError::InvalidState(format!(
                            "EBADF: posix_spawn guest fd {guest_fd} collides with another mapped descriptor"
                        )));
                    }
                    Some(raw_fd)
                };
                if let Some(fd) = fd {
                    mapped_kernel_fds.remove(&fd);
                    kernel
                        .fd_close(EXECUTION_DRIVER_NAME, pid, fd)
                        .map_err(kernel_error)?;
                }
            }
            2 => {
                let guest_fd = action_guest_fd(action)?;
                let guest_source_fd = u32::try_from(
                    action.guest_source_fd.unwrap_or(action.source_fd),
                )
                .map_err(|_| {
                    SidecarError::InvalidState(format!(
                        "EBADF: invalid posix_spawn dup2 source {}",
                        action.guest_source_fd.unwrap_or(action.source_fd)
                    ))
                })?;
                if guest_source_fd == guest_fd {
                    let fd = if let Some(fd) = mappings.get(&guest_source_fd).copied() {
                        fd
                    } else if action.guest_source_fd.is_some() && guest_source_fd > 2 {
                        return Err(SidecarError::InvalidState(format!(
                            "EBADF: posix_spawn dup2 source guest fd {guest_source_fd} is not kernel-backed"
                        )));
                    } else {
                        u32::try_from(action.source_fd).map_err(|_| {
                            SidecarError::InvalidState(format!(
                                "EBADF: invalid posix_spawn dup2 source {}",
                                action.source_fd
                            ))
                        })?
                    };
                    // POSIX spawn dup2(fd, fd) clears FD_CLOEXEC.
                    kernel
                        .fd_fcntl(
                            EXECUTION_DRIVER_NAME,
                            pid,
                            fd,
                            agentos_kernel::fd_table::F_SETFD,
                            0,
                        )
                        .map_err(kernel_error)?;
                    closed_guest_fds.remove(&guest_fd);
                    continue;
                }
                let source_fd = if let Some(fd) = mappings.get(&guest_source_fd).copied() {
                    fd
                } else if action.guest_source_fd.is_some() && guest_source_fd > 2 {
                    return Err(SidecarError::InvalidState(format!(
                        "EBADF: posix_spawn dup2 source guest fd {guest_source_fd} is not kernel-backed"
                    )));
                } else {
                    let raw_fd = u32::try_from(action.source_fd).map_err(|_| {
                        SidecarError::InvalidState(format!(
                            "EBADF: invalid posix_spawn dup2 source {}",
                            action.source_fd
                        ))
                    })?;
                    if mapped_kernel_fds.contains(&raw_fd) {
                        return Err(SidecarError::InvalidState(format!(
                            "EBADF: posix_spawn dup2 source guest fd {guest_source_fd} collides with another mapped descriptor"
                        )));
                    }
                    raw_fd
                };
                let fd = if let Some(fd) = mappings.get(&guest_fd).copied() {
                    kernel
                        .fd_dup2(EXECUTION_DRIVER_NAME, pid, source_fd, fd)
                        .map_err(kernel_error)?;
                    fd
                } else {
                    kernel
                        .fd_dup(EXECUTION_DRIVER_NAME, pid, source_fd)
                        .map_err(kernel_error)?
                };
                mappings.insert(guest_fd, fd);
                mapped_kernel_fds.insert(fd);
                closed_guest_fds.remove(&guest_fd);
            }
            3 => {
                let guest_fd = action_guest_fd(action)?;
                if let Some(fd) = mappings.remove(&guest_fd) {
                    mapped_kernel_fds.remove(&fd);
                    kernel
                        .fd_close(EXECUTION_DRIVER_NAME, pid, fd)
                        .map_err(kernel_error)?;
                }
                let action_path = resolve_posix_spawn_action_path(&cwd, &action.path);
                let opened_fd = kernel
                    .fd_open(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        &action_path,
                        kernel_open_flags_from_wasi(action.oflag),
                        Some(action.mode),
                    )
                    .map_err(kernel_error)?;
                if action.oflag as u32 & (2 << 12) != 0 {
                    let stat = kernel
                        .fd_stat(EXECUTION_DRIVER_NAME, pid, opened_fd)
                        .map_err(kernel_error)?;
                    if stat.filetype != agentos_kernel::fd_table::FILETYPE_DIRECTORY {
                        kernel
                            .fd_close(EXECUTION_DRIVER_NAME, pid, opened_fd)
                            .map_err(kernel_error)?;
                        return Err(SidecarError::InvalidState(format!(
                            "ENOTDIR: posix_spawn open path is not a directory: {}",
                            action.path
                        )));
                    }
                }
                mappings.insert(guest_fd, opened_fd);
                mapped_kernel_fds.insert(opened_fd);
                closed_guest_fds.remove(&guest_fd);
            }
            4 => {
                let action_path = resolve_posix_spawn_action_path(&cwd, &action.path);
                let stat = kernel
                    .stat_for_process(EXECUTION_DRIVER_NAME, pid, &action_path)
                    .map_err(kernel_error)?;
                if !stat.is_directory {
                    return Err(SidecarError::InvalidState(format!(
                        "ENOTDIR: posix_spawn chdir path is not a directory: {}",
                        action.path
                    )));
                }
                cwd = kernel
                    .realpath_for_process(EXECUTION_DRIVER_NAME, pid, &action_path)
                    .map(|path| normalize_path(&path))
                    .map_err(kernel_error)?;
            }
            5 => {
                let guest_fd = action_guest_fd(action)?;
                let fd = if let Some(fd) = mappings.get(&guest_fd).copied() {
                    fd
                } else if action.guest_fd.is_some() && guest_fd > 2 {
                    return Err(SidecarError::InvalidState(format!(
                        "EBADF: posix_spawn fchdir guest fd {guest_fd} is not kernel-backed"
                    )));
                } else {
                    u32::try_from(action.fd).map_err(|_| {
                        SidecarError::InvalidState(format!(
                            "EBADF: invalid posix_spawn fchdir fd {}",
                            action.fd
                        ))
                    })?
                };
                let stat = kernel
                    .fd_stat(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_error)?;
                if stat.filetype != agentos_kernel::fd_table::FILETYPE_DIRECTORY {
                    return Err(SidecarError::InvalidState(format!(
                        "ENOTDIR: posix_spawn fchdir fd {guest_fd} is not a directory"
                    )));
                }
                cwd = normalize_path(
                    &kernel
                        .fd_path(EXECUTION_DRIVER_NAME, pid, fd)
                        .map_err(kernel_error)?,
                );
            }
            6 => {
                let low_fd = action_guest_fd(action)?;
                if let Some(limit) = kernel.resource_limits().max_open_fds {
                    if action.close_from_guest_fds.len() > limit {
                        return Err(SidecarError::InvalidState(format!(
                            "EMFILE: posix_spawn closefrom guest fd list has {} entries, exceeding limits.resources.maxOpenFds ({limit}); raise limits.resources.maxOpenFds",
                            action.close_from_guest_fds.len()
                        )));
                    }
                }
                for guest_fd in &action.close_from_guest_fds {
                    if *guest_fd < low_fd {
                        return Err(SidecarError::InvalidState(format!(
                            "EINVAL: posix_spawn closefrom guest fd {guest_fd} is below cutoff {low_fd}"
                        )));
                    }
                    closed_guest_fds.insert(*guest_fd);
                }
                let open_kernel_fds = kernel
                    .fd_snapshot(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_error)?
                    .into_iter()
                    .map(|entry| entry.fd)
                    .collect::<BTreeSet<_>>();

                // File actions operate in the guest descriptor namespace.
                let mapped_fds = mappings
                    .iter()
                    .map(|(guest_fd, kernel_fd)| (*guest_fd, *kernel_fd))
                    .collect::<Vec<_>>();
                let mut to_close = BTreeMap::new();
                for (guest_fd, kernel_fd) in mapped_fds {
                    if guest_fd >= low_fd && open_kernel_fds.contains(&kernel_fd) {
                        to_close.insert(guest_fd, kernel_fd);
                    }
                }
                for kernel_fd in open_kernel_fds {
                    if !mapped_kernel_fds.contains(&kernel_fd) && kernel_fd >= low_fd {
                        to_close.insert(kernel_fd, kernel_fd);
                    }
                }

                for (guest_fd, kernel_fd) in to_close {
                    closed_guest_fds.insert(guest_fd);
                    if mappings.remove(&guest_fd).is_some() {
                        mapped_kernel_fds.remove(&kernel_fd);
                    }
                    kernel
                        .fd_close(EXECUTION_DRIVER_NAME, pid, kernel_fd)
                        .map_err(kernel_error)?;
                }
            }
            command => {
                return Err(SidecarError::InvalidState(format!(
                    "EINVAL: unknown posix_spawn file action {command}"
                )));
            }
        }
    }
    let child_kernel_fds = kernel
        .fd_snapshot(EXECUTION_DRIVER_NAME, pid)
        .map_err(kernel_error)?
        .into_iter()
        .map(|entry| entry.fd)
        .collect::<BTreeSet<_>>();
    Ok((
        AppliedPosixSpawnFileActions {
            fd_mappings: mappings
                .into_iter()
                .filter(|(_, kernel_fd)| child_kernel_fds.contains(kernel_fd))
                .map(|(guest_fd, kernel_fd)| [guest_fd, kernel_fd])
                .collect(),
            closed_guest_fds: closed_guest_fds.into_iter().collect(),
        },
        cwd,
    ))
}

pub(super) fn resolve_posix_spawn_action_path(cwd: &str, action_path: &str) -> String {
    if action_path.starts_with('/') {
        normalize_path(action_path)
    } else {
        normalize_path(&format!("{cwd}/{action_path}"))
    }
}

pub(super) fn apply_posix_spawn_file_actions_or_rollback(
    kernel: &mut SidecarKernel,
    process: &KernelProcessHandle,
    cwd: &str,
    inherited_mappings: &[[u32; 2]],
    actions: &[JavascriptPosixSpawnFileAction],
) -> Result<AppliedPosixSpawnFileActions, SidecarError> {
    match apply_posix_spawn_file_actions(kernel, process.pid(), cwd, inherited_mappings, actions) {
        Ok((mappings, _)) => Ok(mappings),
        Err(error) => {
            process.finish(127);
            if let Err(cleanup_error) = kernel.waitpid(process.pid()) {
                eprintln!(
                    "[agentos] failed to reap rejected posix_spawn child {}: {}",
                    process.pid(),
                    cleanup_error
                );
            }
            Err(error)
        }
    }
}

pub(super) fn rollback_unregistered_spawn_child(
    kernel: &mut SidecarKernel,
    process: &KernelProcessHandle,
    execution: Option<&mut ActiveExecution>,
    context: &str,
) {
    if let Some(execution) = execution {
        if let ActiveExecution::Binding(binding) = execution {
            binding.cancelled.store(true, Ordering::Relaxed);
        } else if let Err(error) = execution.terminate() {
            eprintln!(
                "[agentos] failed to terminate rejected {context} runtime for PID {}: {error}",
                process.pid()
            );
        }
    }
    process.finish(127);
    if let Err(error) = kernel.waitpid(process.pid()) {
        eprintln!(
            "[agentos] failed to reap rejected {context} child {}: {error}",
            process.pid()
        );
    }
}

pub(super) fn apply_spawn_session_or_rollback(
    kernel: &mut SidecarKernel,
    process: &KernelProcessHandle,
    create_session: bool,
) -> Result<(), SidecarError> {
    if !create_session {
        return Ok(());
    }
    if let Err(error) = kernel.setsid(EXECUTION_DRIVER_NAME, process.pid()) {
        process.finish(127);
        if let Err(cleanup_error) = kernel.waitpid(process.pid()) {
            eprintln!(
                "[agentos] failed to reap rejected setsid child {}: {}",
                process.pid(),
                cleanup_error
            );
        }
        return Err(kernel_error(error));
    }
    Ok(())
}

fn canonicalize_host_runtime_posix_stdin(
    kernel: &mut SidecarKernel,
    pid: u32,
    applied: &AppliedPosixSpawnFileActions,
) -> Result<u32, SidecarError> {
    if applied.closed_guest_fds.contains(&0) {
        if let Err(error) = kernel.fd_close(EXECUTION_DRIVER_NAME, pid, 0) {
            // POSIX spawn file actions already closed the descriptor. Host
            // runtimes canonicalize stdio afterward, so an already-closed fd
            // is the desired state rather than a launch failure.
            if error.code() != "EBADF" {
                return Err(kernel_error(error));
            }
        }
        return Ok(0);
    }
    let Some(source_fd) = applied
        .fd_mappings
        .iter()
        .find_map(|mapping| (mapping[0] == 0).then_some(mapping[1]))
    else {
        return Ok(0);
    };
    kernel
        .fd_dup2(EXECUTION_DRIVER_NAME, pid, source_fd, 0)
        .map_err(kernel_error)?;
    Ok(0)
}

pub(super) fn preapply_posix_spawn_file_actions(
    kernel: &mut SidecarKernel,
    parent_pid: u32,
    cwd: &str,
    requested_pgid: Option<u32>,
    inherited_mappings: &[[u32; 2]],
    actions: &[JavascriptPosixSpawnFileAction],
) -> Result<PreparedPosixSpawnFileActions, SidecarError> {
    let process = kernel
        .spawn_process_with_process_group_preserving_cloexec(
            WASM_COMMAND,
            vec![String::from("posix-spawn-file-actions")],
            SpawnOptions {
                requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                parent_pid: Some(parent_pid),
                env: BTreeMap::new(),
                cwd: Some(cwd.to_owned()),
            },
            requested_pgid,
        )
        .map_err(kernel_error)?;
    let prepared = (|| {
        let (mut applied, cwd) = apply_posix_spawn_file_actions(
            kernel,
            process.pid(),
            cwd,
            inherited_mappings,
            actions,
        )?;
        kernel
            .close_process_cloexec_fds(EXECUTION_DRIVER_NAME, process.pid())
            .map_err(kernel_error)?;
        let snapshot = kernel
            .fd_snapshot(EXECUTION_DRIVER_NAME, process.pid())
            .map_err(kernel_error)?;
        let surviving_fds = snapshot
            .iter()
            .map(|entry| entry.fd)
            .collect::<BTreeSet<_>>();
        applied
            .fd_mappings
            .retain(|mapping| surviving_fds.contains(&mapping[1]));
        let mut fds = Vec::with_capacity(snapshot.len());
        for entry in snapshot {
            fds.push(PreparedPosixSpawnFd {
                fd: entry.fd,
                fd_flags: entry.fd_flags,
                transfer: kernel
                    .fd_transfer(EXECUTION_DRIVER_NAME, process.pid(), entry.fd)
                    .map_err(kernel_error)?,
            });
        }
        Ok(PreparedPosixSpawnFileActions { applied, fds, cwd })
    })();
    process.finish(if prepared.is_ok() { 0 } else { 127 });
    let reap_result = kernel.waitpid(process.pid()).map_err(kernel_error);
    match prepared {
        Ok(prepared) => {
            reap_result?;
            Ok(prepared)
        }
        Err(error) => {
            if let Err(reap_error) = reap_result {
                eprintln!(
                    "[agentos] failed to reap rejected preapplied posix_spawn child {}: {}",
                    process.pid(),
                    reap_error
                );
            }
            Err(error)
        }
    }
}

pub(super) fn install_preapplied_posix_spawn_file_actions(
    kernel: &mut SidecarKernel,
    process: &KernelProcessHandle,
    prepared: PreparedPosixSpawnFileActions,
) -> Result<AppliedPosixSpawnFileActions, SidecarError> {
    let result = (|| {
        let inherited_fds = kernel
            .fd_snapshot(EXECUTION_DRIVER_NAME, process.pid())
            .map_err(kernel_error)?;
        for entry in inherited_fds {
            kernel
                .fd_close(EXECUTION_DRIVER_NAME, process.pid(), entry.fd)
                .map_err(kernel_error)?;
        }
        for entry in &prepared.fds {
            kernel
                .fd_install_transfer_at(
                    EXECUTION_DRIVER_NAME,
                    process.pid(),
                    entry.fd,
                    entry.fd_flags,
                    &entry.transfer,
                )
                .map_err(kernel_error)?;
        }
        Ok(prepared.applied)
    })();
    if result.is_err() {
        process.finish(127);
        if let Err(cleanup_error) = kernel.waitpid(process.pid()) {
            eprintln!(
                "[agentos] failed to reap rejected preapplied posix_spawn target {}: {}",
                process.pid(),
                cleanup_error
            );
        }
    }
    result
}

#[derive(Debug)]
pub(super) struct JavascriptSpawnAttributes {
    process_group: Option<u32>,
    new_session: bool,
}

pub(super) fn javascript_spawn_attributes(
    options: &JavascriptChildProcessSpawnOptions,
) -> Result<JavascriptSpawnAttributes, SidecarError> {
    if options.spawn_attr_flags & !SUPPORTED_POSIX_SPAWN_FLAGS != 0 {
        return Err(SidecarError::InvalidState(format!(
            "unsupported POSIX spawn attribute flags: {:#x}",
            options.spawn_attr_flags & !SUPPORTED_POSIX_SPAWN_FLAGS
        )));
    }
    for signal in options
        .spawn_signal_defaults
        .iter()
        .chain(options.spawn_signal_mask.iter())
    {
        if !(1..=64).contains(signal) {
            return Err(SidecarError::InvalidState(format!(
                "invalid POSIX spawn signal number {signal}"
            )));
        }
    }

    let new_session = options.spawn_attr_flags & POSIX_SPAWN_SETSID != 0;
    if new_session && options.spawn_attr_flags & POSIX_SPAWN_SETPGROUP != 0 {
        return Err(SidecarError::InvalidState(String::from(
            "EPERM: POSIX_SPAWN_SETSID cannot be combined with POSIX_SPAWN_SETPGROUP",
        )));
    }
    if new_session && options.detached {
        return Err(SidecarError::InvalidState(String::from(
            "EINVAL: POSIX_SPAWN_SETSID cannot be combined with detached child-process mode",
        )));
    }
    if options.spawn_attr_flags & (POSIX_SPAWN_SETSCHEDPARAM | POSIX_SPAWN_SETSCHEDULER) != 0
        && options.spawn_sched_priority.unwrap_or_default() != 0
    {
        return Err(SidecarError::InvalidState(String::from(
            "EINVAL: SCHED_OTHER requires scheduling priority zero",
        )));
    }
    if options.spawn_attr_flags & POSIX_SPAWN_SETSCHEDULER != 0
        && options.spawn_sched_policy.unwrap_or_default() != 0
    {
        return Err(SidecarError::InvalidState(String::from(
            "EPERM: requested POSIX spawn scheduler policy requires host privilege",
        )));
    }

    if options.spawn_attr_flags & POSIX_SPAWN_SETPGROUP == 0 {
        if options.spawn_pgroup.unwrap_or(0) != 0 {
            return Err(SidecarError::InvalidState(String::from(
                "spawnPgroup requires POSIX_SPAWN_SETPGROUP",
            )));
        }
        return Ok(JavascriptSpawnAttributes {
            process_group: None,
            new_session,
        });
    }
    if options.detached {
        return Err(SidecarError::InvalidState(String::from(
            "POSIX_SPAWN_SETPGROUP cannot be combined with detached child-process mode",
        )));
    }
    let pgid = options.spawn_pgroup.unwrap_or(0);
    let process_group = u32::try_from(pgid).map_err(|_| {
        SidecarError::InvalidState(format!("invalid POSIX spawn process group {pgid}"))
    })?;
    Ok(JavascriptSpawnAttributes {
        process_group: Some(process_group),
        new_session,
    })
}

pub(super) fn apply_child_process_argv0(
    resolved: &mut ResolvedChildProcessExecution,
    argv0: Option<&str>,
) {
    let Some(argv0) = argv0 else {
        return;
    };
    if resolved.process_args.is_empty() {
        resolved.process_args.push(argv0.to_owned());
    } else {
        resolved.process_args[0] = argv0.to_owned();
    }
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) fn pump_child_process_events_nowait(
        &mut self,
        vm_id: &str,
        javascript_services: &mut Vec<OwnedJavascriptEventService>,
        python_services: &mut Vec<OwnedPythonEventService>,
        python_socket_completions: &mut Vec<OwnedPythonSocketCompletionService>,
        child_bridge_services: &mut Vec<OwnedChildBridgeEventService>,
        max_service_claims: usize,
    ) -> Result<bool, SidecarError> {
        let root_process_ids = self
            .vms
            .get(vm_id)
            .map(|vm| vm.active_processes.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut child_candidates = Vec::new();

        for process_id in root_process_ids {
            if self
                .vms
                .get(vm_id)
                .is_some_and(|vm| vm.detached_child_processes.contains(&process_id))
            {
                continue;
            }
            let child_paths = self
                .vms
                .get(vm_id)
                .map(|vm| {
                    let mut child_paths = Vec::new();
                    if let Some(root) = vm.active_processes.get(&process_id) {
                        Self::collect_attached_child_paths(root, &mut Vec::new(), &mut child_paths);
                    }
                    child_paths
                })
                .unwrap_or_default();
            child_candidates.extend(
                child_paths
                    .into_iter()
                    .map(|child_path| (process_id.clone(), child_path)),
            );
        }

        if child_candidates.is_empty() {
            return Ok(false);
        }
        let start = self
            .vms
            .get(vm_id)
            .map(|vm| vm.attached_child_event_cursor % child_candidates.len())
            .unwrap_or_default();
        child_candidates.rotate_left(start);
        if let Some(mut vm) = self.vms.get_mut(vm_id) {
            vm.attached_child_event_cursor = (start + 1) % child_candidates.len();
        }

        let vm_work_limit = self.config.runtime.fairness.vm_quantum_operations;
        let child_work_limit = self.config.runtime.fairness.capability_quantum_operations;
        let mut emitted_any = false;
        let mut work = 0usize;
        let mut child_work = vec![0usize; child_candidates.len()];
        let mut yielded = false;

        loop {
            let mut emitted_this_round = false;
            for (candidate_index, (process_id, child_path)) in child_candidates.iter().enumerate() {
                if javascript_services
                    .len()
                    .saturating_add(python_services.len())
                    .saturating_add(python_socket_completions.len())
                    .saturating_add(child_bridge_services.len())
                    >= max_service_claims
                {
                    yielded = true;
                    break;
                }
                if work >= vm_work_limit {
                    yielded = true;
                    break;
                }
                if child_work[candidate_index] >= child_work_limit {
                    yielded = true;
                    continue;
                }

                let Some(child_process_id) = child_path.last().cloned() else {
                    continue;
                };
                let parent_path = child_path[..child_path.len() - 1]
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();

                let bridge_relay_in_flight = self.vms.get(vm_id).is_some_and(|vm| {
                    vm.active_processes
                        .get(process_id)
                        .and_then(|root| Self::active_process_by_path(root, &parent_path))
                        .and_then(|parent| parent.child_processes.get(&child_process_id))
                        .is_some_and(|child| {
                            child.child_bridge_relay_in_flight.load(Ordering::Acquire)
                        })
                });
                if bridge_relay_in_flight {
                    continue;
                }

                // Deadline and capacity wakes must service the child's parked
                // synchronous RPC even when a standalone WASM parent owns
                // output delivery through child_process.poll.
                self.recheck_child_deferred_kernel_wait_rpc(
                    vm_id,
                    process_id,
                    &parent_path,
                    &child_process_id,
                )?;

                // The standalone WASM runner pulls descendant output through
                // child_process.poll while implementing waitpid. Keep stream
                // and exit delivery single-owner; the parked kernel wait was
                // already rechecked above without leasing either event lane.
                let parent_is_pull_driven_wasm = self
                    .vms
                    .get(vm_id)
                    .map(|vm| {
                        vm.active_processes
                            .get(process_id)
                            .and_then(|root| Self::active_process_by_path(root, &parent_path))
                            .is_some_and(|parent| parent.runtime == GuestRuntimeKind::WebAssembly)
                    })
                    .unwrap_or(false);
                if parent_is_pull_driven_wasm {
                    // Preserve child_process.poll as the only stream and exit
                    // delivery path, while allowing a terminal child event to
                    // release its kernel-owned descriptors before the parent
                    // reaches waitpid.
                    self.poll_descendant_javascript_child_process_nowait(
                        vm_id,
                        process_id,
                        &parent_path,
                        &child_process_id,
                        true,
                        javascript_services,
                        python_services,
                        python_socket_completions,
                        None,
                    )?;
                    continue;
                }
                self.expire_child_process_sync_if_needed(
                    vm_id,
                    process_id,
                    &parent_path,
                    &child_process_id,
                )?;

                let claimed = match self.poll_descendant_javascript_child_process_nowait(
                    vm_id,
                    process_id,
                    &parent_path,
                    &child_process_id,
                    false,
                    javascript_services,
                    python_services,
                    python_socket_completions,
                    None,
                ) {
                    Ok(event) => event,
                    Err(error) if is_javascript_child_process_gone_error(&error) => continue,
                    Err(error) => return Err(error),
                };
                let ClaimedDescendantBridgeEvent { event, reservation } = claimed;
                if event.is_null() {
                    continue;
                }
                if !self.route_child_process_bridge_event(
                    vm_id,
                    process_id,
                    &parent_path,
                    &child_process_id,
                    event,
                    reservation,
                    child_bridge_services,
                )? {
                    yielded = true;
                    break;
                }
                emitted_any = true;
                emitted_this_round = true;
                work += 1;
                child_work[candidate_index] += 1;
            }
            if yielded || !emitted_this_round {
                break;
            }
        }

        if yielded {
            self.process_event_notify.notify_one();
        }
        Ok(emitted_any)
    }

    pub(crate) async fn pump_child_process_events(
        &mut self,
        vm_id: &str,
    ) -> Result<bool, SidecarError> {
        let mut javascript_services = Vec::new();
        let mut python_services = Vec::new();
        let mut python_socket_completions = Vec::new();
        let mut child_bridge_services = Vec::new();
        let max_service_claims = self.config.runtime.protocol.max_process_events.max(1);
        let emitted = self.pump_child_process_events_nowait(
            vm_id,
            &mut javascript_services,
            &mut python_services,
            &mut python_socket_completions,
            &mut child_bridge_services,
            max_service_claims,
        )?;
        for target in javascript_services {
            let service = self.prepare_owned_javascript_process_event_service(target);
            service.await?;
        }
        for target in python_services {
            let service = prepare_owned_python_process_event_service(self, target);
            service.await?;
        }
        for target in python_socket_completions {
            let (_ownership, vm_id, process_id, child_path, vm, responder, completion, reservation) =
                target.into_parts();
            let _reservation = reservation;
            service_owned_python_socket_connect_completion(
                vm, vm_id, process_id, child_path, responder, completion,
            )
            .await?;
        }
        for target in child_bridge_services {
            service_owned_child_bridge_event(target).await?;
        }
        Ok(emitted)
    }

    fn expire_child_process_sync_if_needed(
        &mut self,
        vm_id: &str,
        process_id: &str,
        parent_path: &[&str],
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let signal = {
            let Some(mut vm) = self.vms.get_mut(vm_id) else {
                return Ok(());
            };
            let Some(root) = vm.active_processes.get_mut(process_id) else {
                return Ok(());
            };
            let Some(parent) = Self::active_process_by_path_mut(root, parent_path) else {
                return Ok(());
            };
            let Some(pending) = parent.pending_child_process_sync.get_mut(child_process_id) else {
                return Ok(());
            };
            if pending.kill_sent
                || pending
                    .deadline
                    .is_none_or(|deadline| Instant::now() < deadline)
            {
                None
            } else {
                pending.kill_sent = true;
                pending.timed_out = true;
                Some(pending.timeout_signal.clone())
            }
        };
        if let Some(signal) = signal {
            self.kill_descendant_javascript_child_process(
                vm_id,
                process_id,
                parent_path,
                child_process_id,
                &signal,
            )?;
        }
        Ok(())
    }

    fn route_child_process_bridge_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
        parent_path: &[&str],
        child_process_id: &str,
        event: Value,
        reservation: Option<PendingExecutionEventReservation>,
        child_bridge_services: &mut Vec<OwnedChildBridgeEventService>,
    ) -> Result<bool, SidecarError> {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let chunk = match event_type {
            "stdout" | "stderr" => Some(javascript_sync_rpc_bytes_arg(
                &[event.get("data").cloned().unwrap_or(Value::Null)],
                0,
                "child process event data",
            )?),
            _ => None,
        };
        let mut kill_for_buffer = false;
        let mut direct_delivery = None;
        let completion = {
            let Some(mut vm) = self.vms.get_mut(vm_id) else {
                return Ok(true);
            };
            let Some(root) = vm.active_processes.get_mut(process_id) else {
                return Ok(true);
            };
            let Some(parent) = Self::active_process_by_path_mut(root, parent_path) else {
                return Ok(true);
            };
            if let Some(pending) = parent.pending_child_process_sync.get_mut(child_process_id) {
                match event_type {
                    "stdout" | "stderr" => {
                        let output = if event_type == "stdout" {
                            &mut pending.stdout
                        } else {
                            &mut pending.stderr
                        };
                        let remaining = pending
                            .max_buffer
                            .saturating_add(1)
                            .saturating_sub(output.len());
                        if let Some(chunk) = chunk.as_deref() {
                            output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                        }
                        if output.len() > pending.max_buffer && !pending.kill_sent {
                            pending.max_buffer_exceeded = true;
                            pending.kill_sent = true;
                            kill_for_buffer = true;
                        }
                        None
                    }
                    "exit" => parent
                        .pending_child_process_sync
                        .remove(child_process_id)
                        .map(|pending| {
                            let exit_code = event
                                .get("exitCode")
                                .and_then(Value::as_i64)
                                .map(|value| value as i32)
                                .unwrap_or(1);
                            (pending, exit_code)
                        }),
                    _ => None,
                }
            } else {
                let payload = match event_type {
                    "stdout" => json!({
                        "sessionId": child_process_id,
                        "dataBase64": base64::engine::general_purpose::STANDARD.encode(
                            chunk.as_deref().unwrap_or_default(),
                        ),
                    }),
                    "stderr" => json!({
                        "sessionId": child_process_id,
                        "dataBase64": base64::engine::general_purpose::STANDARD.encode(
                            chunk.as_deref().unwrap_or_default(),
                        ),
                    }),
                    "exit" => json!({
                        "sessionId": child_process_id,
                        "code": event.get("exitCode").and_then(Value::as_i64).unwrap_or(1),
                        "signal": event.get("signal").cloned().unwrap_or(Value::Null),
                    }),
                    _ => return Ok(true),
                };
                let bridge_event_type = match event_type {
                    "stdout" => "child_stdout",
                    "stderr" => "child_stderr",
                    _ => "child_exit",
                };
                let delivery = parent
                    .execution
                    .send_javascript_stream_event(bridge_event_type, payload.clone());
                direct_delivery = Some((bridge_event_type, payload, delivery));
                None
            }
        };

        if let Some((bridge_event_type, payload, delivery)) = direct_delivery {
            match delivery {
                Ok(()) => {
                    tracing::trace!(
                        vm_id,
                        process_id,
                        child_process_id,
                        event_type = bridge_event_type,
                        "child bridge event admitted to parent V8 session"
                    );
                    return Ok(true);
                }
                Err(SidecarError::Execution(message))
                    if message.contains("ERR_AGENTOS_SESSION_COMMAND_LIMIT") =>
                {
                    tracing::trace!(
                        vm_id,
                        process_id,
                        child_process_id,
                        event_type = bridge_event_type,
                        "child bridge event retained for parent V8 session capacity"
                    );
                    let relay = if bridge_event_type == "child_exit" {
                        None
                    } else {
                        let in_flight = self
                            .vms
                            .get(vm_id)
                            .and_then(|vm| {
                                vm.active_processes
                                    .get(process_id)
                                    .and_then(|root| {
                                        Self::active_process_by_path(root, parent_path)
                                    })
                                    .and_then(|parent| {
                                        parent.child_processes.get(child_process_id)
                                    })
                                    .map(|child| Arc::clone(&child.child_bridge_relay_in_flight))
                            })
                            .ok_or_else(|| {
                                SidecarError::InvalidState(format!(
                                    "ERR_AGENTOS_CHILD_BRIDGE_RELAY_SOURCE_MISSING: child {child_process_id} disappeared while retaining {bridge_event_type} delivery"
                                ))
                            })?;
                        Some(ChildBridgeRelayLease::acquire(
                            in_flight,
                            Arc::clone(&self.process_event_notify),
                            child_process_id,
                        )?)
                    };
                    let Some((connection_id, session_id, deadline, vm)) =
                        self.vms.get(vm_id).and_then(|state| {
                            self.vms.handle(vm_id).map(|handle| {
                                (
                                    state.connection_id.clone(),
                                    state.session_id.clone(),
                                    Instant::now()
                                        + Duration::from_millis(
                                            state.limits.reactor.operation_deadline_ms,
                                        ),
                                    handle,
                                )
                            })
                        })
                    else {
                        return Ok(true);
                    };
                    child_bridge_services.push(OwnedChildBridgeEventService {
                        ownership: OwnershipScope::vm(connection_id, session_id, vm_id),
                        vm_id: vm_id.to_owned(),
                        root_process_id: process_id.to_owned(),
                        parent_path: parent_path
                            .iter()
                            .map(|segment| (*segment).to_owned())
                            .collect(),
                        event_type: bridge_event_type,
                        payload,
                        vm,
                        deadline,
                        _relay: relay,
                        _reservation: reservation,
                    });
                    return Ok(true);
                }
                Err(error) => return Err(error),
            }
        }

        if kill_for_buffer {
            self.kill_descendant_javascript_child_process(
                vm_id,
                process_id,
                parent_path,
                child_process_id,
                "SIGTERM",
            )?;
        }

        if let Some((pending, exit_code)) = completion {
            let result = json!({
                "pid": pending.pid,
                "stdout": String::from_utf8_lossy(&pending.stdout),
                "stderr": String::from_utf8_lossy(&pending.stderr),
                "code": exit_code,
                "signal": if pending.timed_out {
                    Value::String(pending.timeout_signal.clone())
                } else {
                    Value::Null
                },
                "timedOut": pending.timed_out,
                "maxBufferExceeded": pending.max_buffer_exceeded,
            });
            match pending.completion {
                PendingChildProcessSyncCompletion::Javascript(respond_to) => {
                    if respond_to.send(Ok(result)).is_err() {
                        eprintln!(
                            "ERR_AGENTOS_CHILD_PROCESS_SYNC_CANCELLED: spawnSync caller stopped waiting before child exit"
                        );
                    }
                }
                PendingChildProcessSyncCompletion::Python {
                    request_id,
                    responder,
                } => {
                    respond_owned_python_rpc(
                        &responder,
                        request_id,
                        Ok(PythonVfsRpcResponsePayload::SubprocessRun {
                            exit_code,
                            stdout: String::from_utf8_lossy(&pending.stdout).into_owned(),
                            stderr: String::from_utf8_lossy(&pending.stderr).into_owned(),
                            max_buffer_exceeded: pending.max_buffer_exceeded,
                        }),
                    )?;
                }
            }
        }
        Ok(true)
    }

    pub(crate) fn pump_detached_child_process_events_nowait(
        &mut self,
        vm_id: &str,
        javascript_services: &mut Vec<OwnedJavascriptEventService>,
        python_services: &mut Vec<OwnedPythonEventService>,
        python_socket_completions: &mut Vec<OwnedPythonSocketCompletionService>,
        child_bridge_services: &mut Vec<OwnedChildBridgeEventService>,
        max_service_claims: usize,
    ) -> Result<bool, SidecarError> {
        let mut detached_process_ids = self
            .vms
            .get(vm_id)
            .map(|vm| {
                vm.detached_child_processes
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if detached_process_ids.is_empty() {
            return Ok(false);
        }
        let start = self
            .vms
            .get(vm_id)
            .map(|vm| vm.detached_child_event_cursor % detached_process_ids.len())
            .unwrap_or_default();
        detached_process_ids.rotate_left(start);
        if let Some(mut vm) = self.vms.get_mut(vm_id) {
            vm.detached_child_event_cursor = (start + 1) % detached_process_ids.len();
        }
        let vm_work_limit = self.config.runtime.fairness.vm_quantum_operations;
        let child_work_limit = self.config.runtime.fairness.capability_quantum_operations;
        let mut emitted_any = false;
        let mut work = 0usize;
        let mut yielded = false;
        for detached_process_id in detached_process_ids {
            if work >= vm_work_limit {
                yielded = true;
                break;
            }
            if javascript_services
                .len()
                .saturating_add(python_services.len())
                .saturating_add(python_socket_completions.len())
                .saturating_add(child_bridge_services.len())
                >= max_service_claims
            {
                yielded = true;
                break;
            }
            let mut detached_work = 0usize;
            let Some((root_process_id, child_path)) = self.vms.get(vm_id).and_then(|vm| {
                Self::resolve_detached_child_process_path(&vm, &detached_process_id)
            }) else {
                if let Some(mut vm) = self.vms.get_mut(vm_id) {
                    vm.detached_child_processes.remove(&detached_process_id);
                }
                continue;
            };
            if child_path.is_empty() {
                loop {
                    if javascript_services
                        .len()
                        .saturating_add(python_services.len())
                        .saturating_add(python_socket_completions.len())
                        .saturating_add(child_bridge_services.len())
                        >= max_service_claims
                    {
                        yielded = true;
                        break;
                    }
                    if work >= vm_work_limit || detached_work >= child_work_limit {
                        yielded = true;
                        break;
                    }
                    enum ProcessPollResult {
                        Event(Box<Option<PolledExecutionEvent>>),
                        RecoverClosedChannel,
                    }
                    let poll_result = {
                        let Some(mut vm) = self.vms.get_mut(vm_id) else {
                            break;
                        };
                        let Some(process) = vm.active_processes.get_mut(&root_process_id) else {
                            break;
                        };
                        if let Some(event) = process.lease_pending_execution_event() {
                            ProcessPollResult::Event(Box::new(Some(event)))
                        } else {
                            match process.try_poll_execution_event() {
                                Ok(event) => ProcessPollResult::Event(Box::new(event)),
                                Err(SidecarError::Execution(message))
                                    if (process.runtime == GuestRuntimeKind::JavaScript
                                        && closed_javascript_event_channel(&message))
                                        || (process.runtime == GuestRuntimeKind::Python
                                            && closed_python_event_channel(&message))
                                        || (process.runtime == GuestRuntimeKind::WebAssembly
                                            && closed_wasm_event_channel(&message)) =>
                                {
                                    ProcessPollResult::RecoverClosedChannel
                                }
                                Err(error) => return Err(error),
                            }
                        }
                    };
                    let event = match poll_result {
                        ProcessPollResult::Event(event) => *event,
                        ProcessPollResult::RecoverClosedChannel => self
                            .recover_closed_root_runtime_process_event(vm_id, &root_process_id)?
                            .map(PolledExecutionEvent::unreserved),
                    };
                    let Some(event) = event else {
                        break;
                    };
                    work += 1;
                    detached_work += 1;
                    if matches!(event.event(), ActiveExecutionEvent::Exited(_)) {
                        record_execute_response_to_exit_milestone(
                            "execute_response_to_detached_exit_event_polled",
                            vm_id,
                            &detached_process_id,
                        );
                    }
                    let Some((connection_id, session_id)) = self
                        .vms
                        .get(vm_id)
                        .map(|vm| (vm.connection_id.clone(), vm.session_id.clone()))
                    else {
                        break;
                    };
                    let PolledExecutionEvent { event, reservation } = event;
                    match event {
                        ActiveExecutionEvent::Stdout(chunk) => {
                            let envelope = ProcessEventEnvelope {
                                connection_id,
                                session_id,
                                vm_id: vm_id.to_owned(),
                                child_path: Vec::new(),
                                process_id: detached_process_id.clone(),
                                event: ActiveExecutionEvent::Stdout(chunk),
                            };
                            if let Err(error) = self.check_pending_process_event_capacity(&envelope)
                            {
                                if let Some(mut vm) = self.vms.get_mut(vm_id) {
                                    if let Some(process) =
                                        vm.active_processes.get_mut(&root_process_id)
                                    {
                                        process.requeue_pending_execution_event(
                                            PolledExecutionEvent {
                                                event: envelope.event,
                                                reservation,
                                            },
                                        )?;
                                    }
                                }
                                return Err(error);
                            }
                            if let Err((error, envelope)) =
                                self.try_queue_pending_process_event(envelope)
                            {
                                if let Some(mut vm) = self.vms.get_mut(vm_id) {
                                    if let Some(process) =
                                        vm.active_processes.get_mut(&root_process_id)
                                    {
                                        process.requeue_pending_execution_event(
                                            PolledExecutionEvent {
                                                event: envelope.event,
                                                reservation,
                                            },
                                        )?;
                                    }
                                }
                                return Err(error);
                            }
                            drop(reservation);
                            emitted_any = true;
                        }
                        ActiveExecutionEvent::Stderr(chunk) => {
                            let envelope = ProcessEventEnvelope {
                                connection_id,
                                session_id,
                                vm_id: vm_id.to_owned(),
                                child_path: Vec::new(),
                                process_id: detached_process_id.clone(),
                                event: ActiveExecutionEvent::Stderr(chunk),
                            };
                            if let Err(error) = self.check_pending_process_event_capacity(&envelope)
                            {
                                if let Some(mut vm) = self.vms.get_mut(vm_id) {
                                    if let Some(process) =
                                        vm.active_processes.get_mut(&root_process_id)
                                    {
                                        process.requeue_pending_execution_event(
                                            PolledExecutionEvent {
                                                event: envelope.event,
                                                reservation,
                                            },
                                        )?;
                                    }
                                }
                                return Err(error);
                            }
                            if let Err((error, envelope)) =
                                self.try_queue_pending_process_event(envelope)
                            {
                                if let Some(mut vm) = self.vms.get_mut(vm_id) {
                                    if let Some(process) =
                                        vm.active_processes.get_mut(&root_process_id)
                                    {
                                        process.requeue_pending_execution_event(
                                            PolledExecutionEvent {
                                                event: envelope.event,
                                                reservation,
                                            },
                                        )?;
                                    }
                                }
                                return Err(error);
                            }
                            drop(reservation);
                            emitted_any = true;
                        }
                        ActiveExecutionEvent::Exited(exit_code) => {
                            let envelope = ProcessEventEnvelope {
                                connection_id,
                                session_id,
                                vm_id: vm_id.to_owned(),
                                child_path: Vec::new(),
                                process_id: detached_process_id.clone(),
                                event: ActiveExecutionEvent::Exited(exit_code),
                            };
                            if let Err(error) = self.check_pending_process_event_capacity(&envelope)
                            {
                                if let Some(mut vm) = self.vms.get_mut(vm_id) {
                                    if let Some(process) =
                                        vm.active_processes.get_mut(&root_process_id)
                                    {
                                        process.requeue_pending_execution_event(
                                            PolledExecutionEvent {
                                                event: envelope.event,
                                                reservation,
                                            },
                                        )?;
                                    }
                                }
                                return Err(error);
                            }
                            if let Err((error, envelope)) =
                                self.try_queue_pending_process_event(envelope)
                            {
                                if let Some(mut vm) = self.vms.get_mut(vm_id) {
                                    if let Some(process) =
                                        vm.active_processes.get_mut(&root_process_id)
                                    {
                                        process.requeue_pending_execution_event(
                                            PolledExecutionEvent {
                                                event: envelope.event,
                                                reservation,
                                            },
                                        )?;
                                    }
                                }
                                return Err(error);
                            }
                            drop(reservation);
                            if let Some(mut vm) = self.vms.get_mut(vm_id) {
                                vm.detached_child_processes.remove(&detached_process_id);
                            }
                            emitted_any = true;
                            break;
                        }
                        ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
                            let Some((connection_id, session_id, vm)) =
                                self.vms.get(vm_id).and_then(|state| {
                                    self.vms.handle(vm_id).map(|handle| {
                                        (
                                            state.connection_id.clone(),
                                            state.session_id.clone(),
                                            handle,
                                        )
                                    })
                                })
                            else {
                                break;
                            };
                            javascript_services.push(OwnedJavascriptEventService::new(
                                OwnershipScope::vm(connection_id, session_id, vm_id),
                                vm_id.to_owned(),
                                root_process_id.clone(),
                                Vec::new(),
                                vm,
                                request,
                                reservation,
                            ));
                        }
                        ActiveExecutionEvent::JavascriptSyncRpcCompletion(completion) => {
                            drop(reservation);
                            self.handle_javascript_sync_rpc_completion(
                                vm_id,
                                &root_process_id,
                                completion,
                            )?;
                        }
                        ActiveExecutionEvent::PythonVfsRpcRequest(request) => {
                            let Some((connection_id, session_id, vm)) =
                                self.vms.get(vm_id).and_then(|state| {
                                    self.vms.handle(vm_id).map(|handle| {
                                        (
                                            state.connection_id.clone(),
                                            state.session_id.clone(),
                                            handle,
                                        )
                                    })
                                })
                            else {
                                break;
                            };
                            let responder = vm.try_read(
                                "claim detached Python process-event service",
                                |state| {
                                    state
                                        .active_processes
                                        .get(&root_process_id)
                                        .map(|process| process.execution.python_vfs_rpc_responder())
                                },
                            )?;
                            let Some(responder) = responder else {
                                break;
                            };
                            python_services.push(OwnedPythonEventService::new(
                                OwnershipScope::vm(connection_id, session_id, vm_id),
                                vm_id.to_owned(),
                                root_process_id.clone(),
                                Vec::new(),
                                vm,
                                responder?,
                                *request,
                                reservation,
                            ));
                        }
                        ActiveExecutionEvent::PythonSocketConnectCompletion(completion) => {
                            let Some(vm) = self.vms.handle(vm_id) else {
                                break;
                            };
                            let responder =
                                vm.try_read("claim detached Python socket completion", |state| {
                                    state
                                        .active_processes
                                        .get(&root_process_id)
                                        .map(|process| process.execution.python_vfs_rpc_responder())
                                })?;
                            let Some(responder) = responder else {
                                break;
                            };
                            python_socket_completions.push(
                                OwnedPythonSocketCompletionService::new(
                                    OwnershipScope::vm(connection_id, session_id, vm_id),
                                    vm_id.to_owned(),
                                    root_process_id.clone(),
                                    Vec::new(),
                                    vm,
                                    responder?,
                                    *completion,
                                    reservation,
                                ),
                            );
                        }
                        ActiveExecutionEvent::SignalState {
                            signal,
                            registration,
                        } => {
                            drop(reservation);
                            if let Some(mut vm) = self.vms.get_mut(vm_id) {
                                vm.signal_states
                                    .entry(root_process_id.clone())
                                    .or_default()
                                    .insert(signal, registration);
                            }
                        }
                    }
                }
                continue;
            }

            let parent_path = child_path[..child_path.len() - 1]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let child_process_id = child_path.last().expect("child path cannot be empty");

            loop {
                if javascript_services
                    .len()
                    .saturating_add(python_services.len())
                    .saturating_add(python_socket_completions.len())
                    .saturating_add(child_bridge_services.len())
                    >= max_service_claims
                {
                    yielded = true;
                    break;
                }
                if work >= vm_work_limit || detached_work >= child_work_limit {
                    yielded = true;
                    break;
                }
                let claimed = match self.poll_descendant_javascript_child_process_nowait(
                    vm_id,
                    &root_process_id,
                    &parent_path,
                    child_process_id,
                    false,
                    javascript_services,
                    python_services,
                    python_socket_completions,
                    Some(&detached_process_id),
                ) {
                    Ok(event) => event,
                    Err(SidecarError::InvalidState(message))
                        if message.contains("unknown child process")
                            || message.contains("unknown child process path") =>
                    {
                        if let Some(mut vm) = self.vms.get_mut(vm_id) {
                            vm.detached_child_processes.remove(&detached_process_id);
                        }
                        break;
                    }
                    Err(error) if is_javascript_child_process_gone_error(&error) => {
                        if let Some(mut vm) = self.vms.get_mut(vm_id) {
                            vm.detached_child_processes.remove(&detached_process_id);
                        }
                        break;
                    }
                    Err(error) => return Err(error),
                };
                let ClaimedDescendantBridgeEvent { event, reservation } = claimed;

                let Some(event_type) = event.get("type").and_then(Value::as_str) else {
                    break;
                };
                let public_event_queued = event
                    .get("publicEventQueued")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                work += 1;
                detached_work += 1;
                let Some((connection_id, session_id)) = self
                    .vms
                    .get(vm_id)
                    .map(|vm| (vm.connection_id.clone(), vm.session_id.clone()))
                else {
                    break;
                };

                let envelope = match event_type {
                    "stdout" => Some(ProcessEventEnvelope {
                        connection_id: connection_id.clone(),
                        session_id: session_id.clone(),
                        vm_id: vm_id.to_owned(),
                        child_path: Vec::new(),
                        process_id: detached_process_id.clone(),
                        event: ActiveExecutionEvent::Stdout(javascript_sync_rpc_bytes_arg(
                            &[event.get("data").cloned().unwrap_or(Value::Null)],
                            0,
                            "detached child_process stdout",
                        )?),
                    }),
                    "stderr" => Some(ProcessEventEnvelope {
                        connection_id: connection_id.clone(),
                        session_id: session_id.clone(),
                        vm_id: vm_id.to_owned(),
                        child_path: Vec::new(),
                        process_id: detached_process_id.clone(),
                        event: ActiveExecutionEvent::Stderr(javascript_sync_rpc_bytes_arg(
                            &[event.get("data").cloned().unwrap_or(Value::Null)],
                            0,
                            "detached child_process stderr",
                        )?),
                    }),
                    "exit" if !public_event_queued => Some(ProcessEventEnvelope {
                        connection_id,
                        session_id,
                        vm_id: vm_id.to_owned(),
                        child_path: Vec::new(),
                        process_id: detached_process_id.clone(),
                        event: ActiveExecutionEvent::Exited(
                            event
                                .get("exitCode")
                                .and_then(Value::as_i64)
                                .map(|value| value as i32)
                                .unwrap_or(1),
                        ),
                    }),
                    "exit" => None,
                    _ => None,
                };

                if envelope.is_none() && !public_event_queued {
                    break;
                }
                if let Some(envelope) = envelope {
                    if let Err((error, envelope)) = self.try_queue_pending_process_event(envelope) {
                        self.requeue_descendant_claimed_event(
                            vm_id,
                            &root_process_id,
                            &parent_path,
                            child_process_id,
                            envelope.event,
                            reservation,
                        )?;
                        return Err(error);
                    }
                }
                drop(reservation);
                emitted_any = true;

                if event_type == "exit" {
                    if let Some(mut vm) = self.vms.get_mut(vm_id) {
                        vm.detached_child_processes.remove(&detached_process_id);
                    }
                    break;
                }
            }
        }

        if yielded {
            self.process_event_notify.notify_one();
        }
        Ok(emitted_any)
    }

    pub(super) async fn pump_detached_child_process_events(
        &mut self,
        vm_id: &str,
    ) -> Result<bool, SidecarError> {
        let mut javascript_services = Vec::new();
        let mut python_services = Vec::new();
        let mut python_socket_completions = Vec::new();
        let mut child_bridge_services = Vec::new();
        let emitted = self.pump_detached_child_process_events_nowait(
            vm_id,
            &mut javascript_services,
            &mut python_services,
            &mut python_socket_completions,
            &mut child_bridge_services,
            self.config.runtime.protocol.max_process_events.max(1),
        )?;
        for target in javascript_services {
            let service = self.prepare_owned_javascript_process_event_service(target);
            service.await?;
        }
        for target in python_services {
            let service = prepare_owned_python_process_event_service(self, target);
            service.await?;
        }
        for target in python_socket_completions {
            let (_ownership, vm_id, process_id, child_path, vm, responder, completion, reservation) =
                target.into_parts();
            let _reservation = reservation;
            service_owned_python_socket_connect_completion(
                vm, vm_id, process_id, child_path, responder, completion,
            )
            .await?;
        }
        for target in child_bridge_services {
            service_owned_child_bridge_event(target).await?;
        }
        Ok(emitted)
    }
    pub(crate) fn drain_queued_descendant_javascript_child_process_events(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_path: &[&str],
    ) -> Result<(), SidecarError> {
        if child_path.is_empty() {
            return Ok(());
        }
        let mut child_capacity = self.vms.get(vm_id).and_then(|vm| {
            vm.active_processes
                .get(process_id)
                .and_then(|root| descendant_pending_execution_event_capacity(root, child_path))
        });

        let mut deferred = VecDeque::new();
        let mut released_public_capacity = false;
        while let Some(envelope) = self.pending_process_events.pop_front() {
            if envelope.vm_id == vm_id
                && envelope.process_id == process_id
                && envelope
                    .child_path
                    .iter()
                    .map(String::as_str)
                    .eq(child_path.iter().copied())
            {
                if matches!(child_capacity, Some(0)) {
                    self.pending_process_events.push_front(envelope);
                    while let Some(deferred_envelope) = deferred.pop_back() {
                        self.pending_process_events.push_front(deferred_envelope);
                    }
                    self.observe_pending_process_event_depth();
                    if released_public_capacity {
                        self.rearm_deferred_process_event_after_capacity_release();
                    }
                    return Err(process_event_queue_overflow_error(
                        self.config.runtime.protocol.max_process_events,
                    ));
                }
                if let Some(mut vm) = self.vms.get_mut(vm_id) {
                    if let Some(root) = vm.active_processes.get_mut(process_id) {
                        if let Some(child) = Self::active_process_by_path_mut(root, child_path) {
                            match child.try_queue_pending_execution_envelope(envelope) {
                                Ok(()) => {
                                    child_capacity = child_capacity.map(|capacity| capacity - 1);
                                    released_public_capacity = true;
                                    continue;
                                }
                                Err((error, envelope)) => {
                                    self.pending_process_events.push_front(envelope);
                                    while let Some(deferred_envelope) = deferred.pop_back() {
                                        self.pending_process_events.push_front(deferred_envelope);
                                    }
                                    self.observe_pending_process_event_depth();
                                    if released_public_capacity {
                                        self.rearm_deferred_process_event_after_capacity_release();
                                    }
                                    return Err(error);
                                }
                            }
                        }
                    }
                }
            }
            deferred.push_back(envelope);
        }
        self.pending_process_events = deferred;
        self.observe_pending_process_event_depth();
        if released_public_capacity {
            self.rearm_deferred_process_event_after_capacity_release();
        }

        let mut queued = VecDeque::new();
        {
            let transfer_capacity = self
                .pending_process_event_capacity()
                .min(child_capacity.unwrap_or(usize::MAX));
            let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
                SidecarError::InvalidState(String::from("process event receiver unavailable"))
            })?;
            loop {
                if queued.len() >= transfer_capacity {
                    if receiver.is_empty() {
                        break;
                    }
                    self.pending_process_events.append(&mut queued);
                    self.observe_pending_process_event_depth();
                    return Err(process_event_queue_overflow_error(
                        self.config.runtime.protocol.max_process_events,
                    ));
                }
                match receiver.try_recv() {
                    Ok(envelope) => queued.push_back(envelope),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
        }
        while let Some(envelope) = queued.pop_front() {
            if envelope.vm_id == vm_id
                && envelope.process_id == process_id
                && envelope
                    .child_path
                    .iter()
                    .map(String::as_str)
                    .eq(child_path.iter().copied())
            {
                if let Some(mut vm) = self.vms.get_mut(vm_id) {
                    if let Some(root) = vm.active_processes.get_mut(process_id) {
                        if let Some(child) = Self::active_process_by_path_mut(root, child_path) {
                            match child.try_queue_pending_execution_envelope(envelope) {
                                Ok(()) => continue,
                                Err((error, envelope)) => {
                                    self.pending_process_events.push_back(envelope);
                                    self.pending_process_events.append(&mut queued);
                                    self.observe_pending_process_event_depth();
                                    return Err(error);
                                }
                            }
                        }
                    }
                }
            }
            if let Err((error, envelope)) = self.try_queue_pending_process_event(envelope) {
                self.pending_process_events.push_back(envelope);
                self.pending_process_events.append(&mut queued);
                self.observe_pending_process_event_depth();
                return Err(error);
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn resolve_javascript_child_process_execution(
        vm: &VmState,
        parent_env: &BTreeMap<String, String>,
        parent_guest_cwd: &str,
        parent_host_cwd: &Path,
        request: &JavascriptChildProcessSpawnRequest,
    ) -> Result<ResolvedChildProcessExecution, SidecarError> {
        Self::resolve_javascript_child_process_execution_with_mode(
            vm,
            parent_env,
            parent_guest_cwd,
            parent_host_cwd,
            request,
            false,
            None,
        )
    }

    // Resolution keeps host/guest cwd and PATH policy explicit because they
    // are distinct security inputs, not interchangeable options.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve_javascript_child_process_execution_with_mode(
        vm: &VmState,
        parent_env: &BTreeMap<String, String>,
        parent_guest_cwd: &str,
        parent_host_cwd: &Path,
        request: &JavascriptChildProcessSpawnRequest,
        exact_exec_path: bool,
        search_path_override: Option<&str>,
    ) -> Result<ResolvedChildProcessExecution, SidecarError> {
        if exact_exec_path && search_path_override.is_some() {
            return Err(SidecarError::InvalidState(String::from(
                "EINVAL: exact spawn path cannot also request PATH search",
            )));
        }
        let mut runtime_env = parent_env.clone();
        runtime_env.extend(request.options.internal_bootstrap_env.clone());
        let (guest_cwd, host_cwd_override) = request
            .options
            .cwd
            .as_deref()
            .map(|cwd| {
                let normalized_parent_host_cwd = normalize_host_path(parent_host_cwd);
                let requested_host_cwd = normalize_host_path(Path::new(cwd));
                if path_is_within_root(&requested_host_cwd, &normalized_parent_host_cwd) {
                    let relative = requested_host_cwd
                        .strip_prefix(&normalized_parent_host_cwd)
                        .unwrap_or_else(|_| Path::new(""));
                    let relative = relative.to_string_lossy().replace('\\', "/");
                    let guest_cwd = if relative.is_empty() {
                        parent_guest_cwd.to_owned()
                    } else {
                        normalize_path(&format!("{parent_guest_cwd}/{relative}"))
                    };
                    (guest_cwd, Some(requested_host_cwd))
                } else if Path::new(cwd).is_relative() {
                    (
                        normalize_path(&format!("{parent_guest_cwd}/{cwd}")),
                        Some(normalize_host_path(&parent_host_cwd.join(cwd))),
                    )
                } else {
                    (normalize_path(cwd), None)
                }
            })
            .unwrap_or_else(|| (parent_guest_cwd.to_owned(), None));
        let inherited_host_cwd = (host_cwd_override.is_none() && guest_cwd == parent_guest_cwd)
            .then(|| normalize_host_path(parent_host_cwd));
        let host_cwd = host_cwd_override
            .or(inherited_host_cwd)
            .or_else(|| {
                host_runtime_path_for_guest_path_with_env(
                    vm,
                    &runtime_env,
                    &guest_cwd,
                    parent_host_cwd,
                )
            })
            .unwrap_or_else(|| {
                let candidate = PathBuf::from(&guest_cwd);
                if guest_cwd == parent_guest_cwd {
                    normalize_host_path(parent_host_cwd)
                } else if candidate.is_absolute() {
                    shadow_path_for_guest(vm, &guest_cwd)
                } else {
                    vm.host_cwd.clone()
                }
            });
        let mut env = parent_env.clone();
        env.extend(request.options.env.clone());
        // Child JavaScript executions must resolve their own entrypoint/eval state.
        // Reusing the parent's values makes the sidecar load the wrong source file.
        env.remove("AGENTOS_GUEST_ENTRYPOINT");
        env.remove("AGENTOS_NODE_EVAL");

        let (command, process_args) = if request.options.shell {
            let tokens = tokenize_shell_free_command(&request.command);
            let requires_shell = request.options.argv0.is_some()
                || command_requires_shell(&request.command)
                || tokens.first().is_some_and(|command| {
                    is_posix_shell_builtin(command) || shell_first_token_requires_shell(command)
                });
            if requires_shell {
                if !vm.command_guest_paths.contains_key("sh") {
                    return Err(SidecarError::InvalidState(format!(
                        "shell-mode child_process command requires /bin/sh, which is not \
                         installed in this VM (install a software package that provides sh, \
                         for example @agentos-software/coreutils): {}",
                        request.command
                    )));
                }
                (
                    String::from("sh"),
                    vec![String::from("-c"), request.command.clone()],
                )
            } else {
                let Some((command, args)) = tokens.split_first() else {
                    return Err(SidecarError::InvalidState(String::from(
                        "child_process shell command must not be empty",
                    )));
                };
                (command.clone(), args.to_vec())
            }
        } else {
            (request.command.clone(), request.args.clone())
        };
        let process_args = apply_shell_cwd_prefix(&command, process_args, &guest_cwd);
        if !exact_exec_path && is_binding_command(vm, &command) {
            let command = normalized_binding_command_name(&command).unwrap_or(command);
            return Ok(ResolvedChildProcessExecution {
                command: command.clone(),
                process_args: std::iter::once(command.clone())
                    .chain(process_args.iter().cloned())
                    .collect(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: command,
                execution_args: process_args,
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: true,
            });
        }

        if is_path_like_specifier(&command)
            && matches!(
                Path::new(&command).extension().and_then(|ext| ext.to_str()),
                Some("js" | "mjs" | "cjs" | "ts" | "mts" | "cts")
            )
        {
            let guest_entrypoint = if command.starts_with('/') {
                normalize_path(&command)
            } else if command.starts_with("file:") {
                normalize_path(command.trim_start_matches("file:"))
            } else {
                normalize_path(&format!("{guest_cwd}/{command}"))
            };
            let host_entrypoint = if command.starts_with("./") || command.starts_with("../") {
                normalize_host_path(&host_cwd.join(&command))
            } else {
                host_runtime_path_for_guest_path_with_env(
                    vm,
                    &runtime_env,
                    &guest_entrypoint,
                    parent_host_cwd,
                )
                .unwrap_or_else(|| {
                    let candidate = PathBuf::from(&guest_entrypoint);
                    if candidate.is_absolute() {
                        candidate
                    } else {
                        host_cwd.join(&guest_entrypoint)
                    }
                })
            };
            env.insert(String::from("AGENTOS_GUEST_ENTRYPOINT"), guest_entrypoint);
            let guest_entrypoint = env.get("AGENTOS_GUEST_ENTRYPOINT").cloned();
            prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;

            return Ok(ResolvedChildProcessExecution {
                command: command.clone(),
                process_args: std::iter::once(command)
                    .chain(process_args.iter().cloned())
                    .collect(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: host_entrypoint.to_string_lossy().into_owned(),
                execution_args: process_args,
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: false,
            });
        }

        let resolves_to_registered_node_runtime = exact_exec_path
            && registered_command_name_for_path(vm, &command)
                .is_some_and(|name| is_node_runtime_command(&name));
        if (!exact_exec_path || resolves_to_registered_node_runtime)
            && is_node_runtime_command(&command)
        {
            if let Some(cli) = resolve_host_node_cli_entrypoint(&command) {
                env.insert(
                    String::from("AGENTOS_NODE_EVAL"),
                    build_host_node_cli_eval(&cli),
                );
                prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, None)?;
                add_runtime_guest_path_mapping(&mut env, &cli.guest_root, &cli.package_root);
                add_runtime_host_access_path(
                    &mut env,
                    "AGENTOS_EXTRA_FS_READ_PATHS",
                    &cli.package_root,
                    true,
                );

                return Ok(ResolvedChildProcessExecution {
                    command: command.clone(),
                    process_args: std::iter::once(command.clone())
                        .chain(process_args.iter().cloned())
                        .collect(),
                    runtime: GuestRuntimeKind::JavaScript,
                    entrypoint: String::from("-e"),
                    execution_args: std::iter::once(cli.guest_entrypoint.clone())
                        .chain(process_args.iter().cloned())
                        .collect(),
                    env,
                    guest_cwd,
                    host_cwd,
                    wasm_permission_tier: None,
                    binding_command: false,
                });
            }

            if process_args.is_empty() {
                env.insert(String::from("AGENTOS_NODE_EVAL"), String::new());
                prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, None)?;

                return Ok(ResolvedChildProcessExecution {
                    command: command.clone(),
                    process_args: vec![command.clone()],
                    runtime: GuestRuntimeKind::JavaScript,
                    entrypoint: String::from("-e"),
                    execution_args: Vec::new(),
                    env,
                    guest_cwd,
                    host_cwd,
                    wasm_permission_tier: None,
                    binding_command: false,
                });
            }

            if let Some((entrypoint, execution_args)) =
                resolve_special_node_cli_invocation(&process_args, &mut env)
            {
                prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, None)?;

                return Ok(ResolvedChildProcessExecution {
                    command: command.clone(),
                    process_args: std::iter::once(command.clone())
                        .chain(process_args.iter().cloned())
                        .collect(),
                    runtime: GuestRuntimeKind::JavaScript,
                    entrypoint,
                    execution_args,
                    env,
                    guest_cwd,
                    host_cwd,
                    wasm_permission_tier: None,
                    binding_command: false,
                });
            }

            let Some(entrypoint_specifier) = process_args.first() else {
                return Err(SidecarError::InvalidState(format!(
                    "{command} child_process spawn requires an entrypoint"
                )));
            };

            let (entrypoint, execution_args) = if is_path_like_specifier(entrypoint_specifier) {
                let requested_guest_entrypoint = if entrypoint_specifier.starts_with('/') {
                    normalize_path(entrypoint_specifier)
                } else if entrypoint_specifier.starts_with("file:") {
                    normalize_path(entrypoint_specifier.trim_start_matches("file:"))
                } else {
                    normalize_path(&format!("{guest_cwd}/{entrypoint_specifier}"))
                };
                let preserve_main_symlink = process_args
                    .iter()
                    .any(|argument| argument == "--preserve-symlinks-main");
                let (guest_entrypoint, host_entrypoint) = if preserve_main_symlink {
                    let host_entrypoint = if entrypoint_specifier.starts_with("./")
                        || entrypoint_specifier.starts_with("../")
                    {
                        normalize_host_path(&host_cwd.join(entrypoint_specifier))
                    } else {
                        host_runtime_path_for_guest_path_with_env(
                            vm,
                            &runtime_env,
                            &requested_guest_entrypoint,
                            parent_host_cwd,
                        )
                        .unwrap_or_else(|| {
                            let candidate = PathBuf::from(&requested_guest_entrypoint);
                            if candidate.is_absolute() {
                                candidate
                            } else {
                                host_cwd.join(&requested_guest_entrypoint)
                            }
                        })
                    };
                    (requested_guest_entrypoint, host_entrypoint)
                } else {
                    resolve_javascript_main_entrypoint(vm, &requested_guest_entrypoint)
                };
                env.insert(String::from("AGENTOS_GUEST_ENTRYPOINT"), guest_entrypoint);
                (
                    host_entrypoint.to_string_lossy().into_owned(),
                    process_args.iter().skip(1).cloned().collect(),
                )
            } else {
                (
                    entrypoint_specifier.clone(),
                    process_args.iter().skip(1).cloned().collect(),
                )
            };
            let guest_entrypoint = env.get("AGENTOS_GUEST_ENTRYPOINT").cloned();
            prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;

            return Ok(ResolvedChildProcessExecution {
                command: command.clone(),
                process_args: std::iter::once(command)
                    .chain(process_args.iter().cloned())
                    .collect(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint,
                execution_args,
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: false,
            });
        }

        if !exact_exec_path && is_python_runtime_command(&command) {
            return resolve_python_command_execution(
                vm,
                &command,
                &process_args,
                env,
                guest_cwd,
                host_cwd,
            );
        }

        let guest_entrypoint = if exact_exec_path {
            resolve_exact_guest_command_entrypoint(vm, &guest_cwd, &command)
        } else {
            resolve_guest_command_entrypoint(
                vm,
                &guest_cwd,
                &command,
                search_path_override.or_else(|| env.get("PATH").map(String::as_str)),
            )
        }
        .ok_or_else(|| SidecarError::InvalidState(format!("command not found: {command}")))?;
        let host_entrypoint = resolve_vm_guest_path_to_host(vm, &guest_entrypoint);
        let wasm_permission_tier = vm.command_permissions.get(&command).copied().or_else(|| {
            Path::new(&guest_entrypoint)
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| vm.command_permissions.get(name).copied())
        });
        if let Some((javascript_guest_entrypoint, javascript_host_entrypoint)) =
            resolve_javascript_command_entrypoint(vm, &guest_entrypoint, &host_entrypoint)
        {
            prepare_guest_runtime_env(
                vm,
                &mut env,
                &guest_cwd,
                &host_cwd,
                Some(javascript_guest_entrypoint),
            )?;

            return Ok(ResolvedChildProcessExecution {
                command: command.clone(),
                process_args: std::iter::once(command)
                    .chain(process_args.iter().cloned())
                    .collect(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: javascript_host_entrypoint.to_string_lossy().into_owned(),
                execution_args: process_args,
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: false,
            });
        }
        prepare_guest_runtime_env(
            vm,
            &mut env,
            &guest_cwd,
            &host_cwd,
            Some(guest_entrypoint.clone()),
        )?;

        Ok(ResolvedChildProcessExecution {
            command: command.clone(),
            process_args: std::iter::once(command)
                .chain(process_args.iter().cloned())
                .collect(),
            runtime: GuestRuntimeKind::WebAssembly,
            entrypoint: host_entrypoint.to_string_lossy().into_owned(),
            execution_args: process_args,
            env,
            guest_cwd,
            host_cwd,
            wasm_permission_tier,
            binding_command: false,
        })
    }

    fn resolve_javascript_child_process_with_shebang(
        &mut self,
        vm_id: &str,
        parent_env: &BTreeMap<String, String>,
        parent_guest_cwd: &str,
        parent_host_cwd: &Path,
        request: &mut JavascriptChildProcessSpawnRequest,
    ) -> Result<ResolvedChildProcessExecution, SidecarError> {
        const MAX_SHEBANG_REDIRECTS: usize = 4;

        let mut resolved = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            Self::resolve_javascript_child_process_execution_with_mode(
                &vm,
                parent_env,
                parent_guest_cwd,
                parent_host_cwd,
                request,
                false,
                None,
            )?
        };

        for redirects in 0..=MAX_SHEBANG_REDIRECTS {
            let redirected = {
                let mut vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                rewrite_javascript_shebang_request(&mut vm, &resolved, request)?
            };
            if !redirected {
                return Ok(resolved);
            }
            if redirects == MAX_SHEBANG_REDIRECTS {
                return Err(SidecarError::Execution(format!(
                    "ELOOP: exceeded {MAX_SHEBANG_REDIRECTS} shebang redirects"
                )));
            }
            resolved = {
                let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                Self::resolve_javascript_child_process_execution_with_mode(
                    &vm,
                    parent_env,
                    parent_guest_cwd,
                    parent_host_cwd,
                    request,
                    false,
                    None,
                )?
            };
        }

        Ok(resolved)
    }

    pub(crate) async fn spawn_javascript_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        mut request: JavascriptChildProcessSpawnRequest,
    ) -> Result<Value, SidecarError> {
        let spawn_attributes = javascript_spawn_attributes(&request.options)?;
        let requested_pgid = spawn_attributes.process_group;
        let parent_sync_roots = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let parent = vm
                .active_processes
                .get(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            (parent.host_write_dirty_recursive()
                || !parent.clean_host_writes_are_observable_recursive())
            .then(|| {
                (
                    parent.host_cwd.clone(),
                    parent.guest_cwd.clone(),
                    parent.runtime != GuestRuntimeKind::JavaScript,
                )
            })
        };
        if let Some((host_cwd, guest_cwd, sync_root_shadow)) = parent_sync_roots {
            let mut vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            sync_process_host_roots_to_kernel(&mut vm, &host_cwd, &guest_cwd, sync_root_shadow)?;
        }
        let prepared_host_net_fds = {
            let mut vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let mut vm = &mut *vm;
            let current_network_counts = vm_spawn_host_net_resource_counts(&mut vm);
            let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
            let parent = active_processes
                .get_mut(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            prepare_spawn_host_net_fds(
                kernel,
                parent,
                current_network_counts,
                &request.options.spawn_host_net_fds,
                &request.options.spawn_fd_mappings,
                &request.options.spawn_file_actions,
            )?
        };
        let prepared_spawn_actions = if !prepared_host_net_fds.kernel_actions.is_empty() {
            let (parent_pid, parent_cwd) = {
                let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                let parent = vm
                    .active_processes
                    .get(process_id)
                    .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                let initial_cwd = request
                    .options
                    .cwd
                    .as_deref()
                    .map(|cwd| {
                        if cwd.starts_with('/') {
                            normalize_path(cwd)
                        } else {
                            normalize_path(&format!("{}/{cwd}", parent.guest_cwd))
                        }
                    })
                    .unwrap_or_else(|| parent.guest_cwd.clone());
                (parent.kernel_pid, initial_cwd)
            };
            let mut vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            Some(preapply_posix_spawn_file_actions(
                &mut vm.kernel,
                parent_pid,
                &parent_cwd,
                requested_pgid,
                &request.options.spawn_fd_mappings,
                &prepared_host_net_fds.kernel_actions,
            )?)
        } else {
            None
        };
        if let Some(prepared) = prepared_spawn_actions.as_ref() {
            request.options.cwd = Some(prepared.cwd.clone());
        }
        {
            let parent_guest_cwd = self
                .vms
                .get(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?
                .active_processes
                .get(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?
                .guest_cwd
                .clone();
            let mut vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            resolve_posix_spawn_program(&mut vm, &parent_guest_cwd, &mut request)?;
        }
        let total_start = Instant::now();
        let process_event_capacity = self.config.runtime.protocol.max_process_events;
        let phase_start = Instant::now();
        let (parent_env, parent_guest_cwd, parent_host_cwd) = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let parent = vm
                .active_processes
                .get(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            (
                parent.env.clone(),
                parent.guest_cwd.clone(),
                parent.host_cwd.clone(),
            )
        };
        let mut resolved =
            if !request.options.spawn_exact_path && request.options.spawn_search_path.is_none() {
                self.resolve_javascript_child_process_with_shebang(
                    vm_id,
                    &parent_env,
                    &parent_guest_cwd,
                    &parent_host_cwd,
                    &mut request,
                )?
            } else {
                let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                Self::resolve_javascript_child_process_execution_with_mode(
                    &vm,
                    &parent_env,
                    &parent_guest_cwd,
                    &parent_host_cwd,
                    &request,
                    request.options.spawn_exact_path,
                    request.options.spawn_search_path.as_deref(),
                )?
            };
        apply_child_process_argv0(&mut resolved, request.options.argv0.as_deref());
        {
            let mut vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            stage_agentos_package_command(&mut vm, &mut resolved)?;
        }
        tracing::debug!(
            vm_id,
            process_id,
            command = %resolved.command,
            runtime = ?resolved.runtime,
            entrypoint = %resolved.entrypoint,
            execution_args = ?resolved.execution_args,
            parent_guest_cwd = %parent_guest_cwd,
            requested_cwd = ?request.options.cwd,
            guest_cwd = %resolved.guest_cwd,
            host_cwd = %resolved.host_cwd.display(),
            "resolved JavaScript child process"
        );
        let resolved = resolved;
        if prepared_host_net_fds.inherited_fd_count() != 0
            && (resolved.runtime != GuestRuntimeKind::WebAssembly || resolved.binding_command)
        {
            return Err(SidecarError::InvalidState(String::from(
                "ENOTSUP: inherited host-network fds require a WebAssembly child runtime",
            )));
        }
        record_execute_phase("child_process_resolve_execution", phase_start.elapsed());
        let (parent_kernel_pid, child_process_id) = {
            let mut vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let process = vm
                .active_processes
                .get_mut(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            (process.kernel_pid, process.allocate_child_process_id())
        };
        let sidecar_requests = self.sidecar_requests.clone();
        let mut vm = self
            .vms
            .get_mut(vm_id)
            .ok_or_else(|| missing_vm_error(vm_id))?;
        let execution_engines = vm.execution_engines.clone();
        enforce_resolved_wasm_execute_dac(&mut vm, parent_kernel_pid, &resolved)?;
        let vm_pending_stdin_bytes_budget = Arc::clone(&vm.pending_stdin_bytes_budget);
        let vm_pending_event_bytes_budget = Arc::clone(&vm.pending_event_bytes_budget);
        let phase_start = Instant::now();
        let (
            kernel_pid,
            kernel_handle,
            execution,
            kernel_stdin_writer_fd,
            kernel_stdin_reader_fd,
            direct_posix_stdin,
        ) = if resolved.binding_command {
            let binding_resolution = resolve_binding_command(
                &mut vm,
                &resolved.command,
                &resolved.execution_args,
                Some(&resolved.guest_cwd),
            )?
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "binding command no longer resolves: {}",
                    resolved.command
                ))
            })?;
            let kernel_handle = vm
                .kernel
                .create_virtual_process_with_process_group(
                    EXECUTION_DRIVER_NAME,
                    BINDING_DRIVER_NAME,
                    &resolved.command,
                    resolved.process_args.clone(),
                    VirtualProcessOptions {
                        parent_pid: Some(parent_kernel_pid),
                        env: resolved.env.clone(),
                        cwd: Some(resolved.guest_cwd.clone()),
                    },
                    requested_pgid,
                )
                .map_err(kernel_error)?;
            let kernel_pid = kernel_handle.pid();
            if let Some(prepared) = prepared_spawn_actions {
                install_preapplied_posix_spawn_file_actions(
                    &mut vm.kernel,
                    &kernel_handle,
                    prepared,
                )?;
            } else {
                apply_posix_spawn_file_actions_or_rollback(
                    &mut vm.kernel,
                    &kernel_handle,
                    &resolved.guest_cwd,
                    &request.options.spawn_fd_mappings,
                    &prepared_host_net_fds.kernel_actions,
                )?;
            }
            apply_spawn_session_or_rollback(
                &mut vm.kernel,
                &kernel_handle,
                spawn_attributes.new_session || request.options.detached,
            )?;
            let binding_execution = BindingExecution::with_event_notify(
                Arc::clone(&self.process_event_notify),
                process_event_capacity,
            )
            .with_vm_pending_event_bytes_budget(Arc::clone(&vm_pending_event_bytes_budget));
            let cancelled = binding_execution.cancelled.clone();
            let pending_events = binding_execution.pending_events.clone();
            let event_overflow_reason = binding_execution.event_overflow_reason.clone();
            let pending_event_bytes = binding_execution.pending_event_bytes.clone();
            let pending_event_count_limit = binding_execution.pending_event_count_limit.clone();
            let pending_event_bytes_limit = binding_execution.pending_event_bytes_limit.clone();
            let binding_vm_pending_event_bytes_budget =
                binding_execution.vm_pending_event_bytes_budget.clone();
            let event_notify = binding_execution.event_notify.clone();
            spawn_binding_process_events(BindingProcessEventRequest {
                runtime_context: vm.runtime_context.clone(),
                sidecar_requests: sidecar_requests.clone(),
                connection_id: vm.connection_id.clone(),
                session_id: vm.session_id.clone(),
                vm_id: vm_id.to_owned(),
                binding_resolution,
                cancelled,
                pending_events,
                event_overflow_reason,
                pending_event_bytes,
                pending_event_count_limit,
                pending_event_bytes_limit,
                vm_pending_event_bytes_budget: binding_vm_pending_event_bytes_budget,
                event_notify,
            });
            (
                kernel_pid,
                kernel_handle,
                ActiveExecution::Binding(binding_execution),
                None,
                0,
                false,
            )
        } else {
            let kernel_command = match resolved.runtime {
                GuestRuntimeKind::JavaScript => JAVASCRIPT_COMMAND,
                GuestRuntimeKind::WebAssembly => WASM_COMMAND,
                GuestRuntimeKind::Python => PYTHON_COMMAND,
            };
            let kernel_handle = vm
                .kernel
                .spawn_process_with_process_group(
                    kernel_command,
                    resolved.process_args.clone(),
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        parent_pid: Some(parent_kernel_pid),
                        env: resolved.env.clone(),
                        cwd: Some(resolved.guest_cwd.clone()),
                    },
                    requested_pgid,
                )
                .map_err(kernel_error)?;
            let kernel_pid = kernel_handle.pid();
            let applied_spawn_actions = if let Some(prepared) = prepared_spawn_actions {
                install_preapplied_posix_spawn_file_actions(
                    &mut vm.kernel,
                    &kernel_handle,
                    prepared,
                )?
            } else {
                apply_posix_spawn_file_actions_or_rollback(
                    &mut vm.kernel,
                    &kernel_handle,
                    &resolved.guest_cwd,
                    &request.options.spawn_fd_mappings,
                    &prepared_host_net_fds.kernel_actions,
                )?
            };
            let posix_spawn_controls_stdin = !request.options.spawn_file_actions.is_empty()
                && (applied_spawn_actions
                    .fd_mappings
                    .iter()
                    .any(|mapping| mapping[0] == 0)
                    || applied_spawn_actions.closed_guest_fds.contains(&0)
                    || prepared_host_net_fds
                        .descriptions
                        .iter()
                        .any(|description| description.guest_fds.contains(&0)));
            if matches!(
                resolved.runtime,
                GuestRuntimeKind::JavaScript | GuestRuntimeKind::Python
            ) {
                materialize_direct_runtime_stdio_mappings(
                    &mut vm.kernel,
                    kernel_pid,
                    &applied_spawn_actions,
                )?;
            }
            let kernel_stdin_reader_fd = if resolved.runtime != GuestRuntimeKind::WebAssembly {
                canonicalize_host_runtime_posix_stdin(
                    &mut vm.kernel,
                    kernel_pid,
                    &applied_spawn_actions,
                )?
            } else {
                0
            };
            apply_spawn_session_or_rollback(
                &mut vm.kernel,
                &kernel_handle,
                spawn_attributes.new_session || request.options.detached,
            )?;
            let mut execution_env = resolved.env.clone();
            if resolved.runtime == GuestRuntimeKind::JavaScript
                && (posix_spawn_controls_stdin
                    || javascript_child_process_stdin_mode(&request) != "pipe")
            {
                execution_env.insert(
                    String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
                    String::from("1"),
                );
            }
            if resolved.runtime == GuestRuntimeKind::WebAssembly {
                execution_env.insert(
                    String::from("AGENTOS_WASM_INHERITED_FD_MAPPINGS"),
                    serde_json::to_string(&applied_spawn_actions.fd_mappings).map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "failed to serialize inherited WASM fd mappings: {error}"
                        ))
                    })?,
                );
                execution_env.insert(
                    String::from("AGENTOS_WASM_CLOSED_INHERITED_FDS"),
                    serde_json::to_string(&applied_spawn_actions.closed_guest_fds).map_err(
                        |error| {
                            SidecarError::InvalidState(format!(
                                "failed to serialize closed inherited WASM fds: {error}"
                            ))
                        },
                    )?,
                );
                execution_env.insert(
                    String::from("AGENTOS_WASM_INHERITED_HOSTNET_FDS"),
                    serde_json::to_string(&prepared_host_net_fds.bootstrap_json()).map_err(
                        |error| {
                            SidecarError::InvalidState(format!(
                                "failed to serialize inherited WASM host-network fds: {error}"
                            ))
                        },
                    )?,
                );
            }
            execution_env.insert(
                String::from(EXECUTION_SANDBOX_ROOT_ENV),
                normalize_host_path(&vm.cwd).to_string_lossy().into_owned(),
            );

            let execution = match resolved.runtime {
                GuestRuntimeKind::JavaScript => {
                    let mut javascript_engine =
                        execution_engines.javascript("start JavaScript child process")?;
                    execution_env.extend(sanitize_javascript_child_process_internal_bootstrap_env(
                        &request.options.internal_bootstrap_env,
                    ));
                    execution_env
                        .insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
                    let launch_entrypoint = resolve_agentos_package_javascript_launch_entrypoint(
                        &mut vm,
                        &mut execution_env,
                    )
                    .unwrap_or_else(|| resolved.entrypoint.clone());
                    let inline_code = load_javascript_entrypoint_source(
                        &mut vm,
                        &resolved.host_cwd,
                        &launch_entrypoint,
                        &execution_env,
                    );
                    prepare_javascript_shadow(&mut vm, &resolved, &execution_env)?;

                    let built_reader = build_module_reader(&mut vm, &resolved);
                    let guest_reader = built_reader.clone().map(|reader| {
                        Box::new(crate::plugins::host_dir::SessionModuleReader::new(reader))
                            as Box<dyn GuestModuleReader>
                    });
                    let module_reader = built_reader
                        .map(|reader| Box::new(reader) as Box<dyn ModuleFsReader + Send>);
                    let context =
                        javascript_engine.create_context(CreateJavascriptContextRequest {
                            vm_id: vm_id.to_owned(),
                            bootstrap_module: None,
                            compile_cache_root: Some(self.cache_root.join("node-compile-cache")),
                        });
                    let context_id = context.context_id;
                    let execution_result = javascript_engine
                        .start_execution_with_module_reader_and_runtime(
                            StartJavascriptExecutionRequest {
                                guest_runtime: guest_runtime_identity(
                                    &mut vm,
                                    Some(u64::from(kernel_pid)),
                                    Some(u64::from(parent_kernel_pid)),
                                ),
                                vm_id: vm_id.to_owned(),
                                context_id: context_id.clone(),
                                argv: std::iter::once(launch_entrypoint)
                                    .chain(resolved.execution_args.clone())
                                    .collect(),
                                argv0: request.options.argv0.clone(),
                                env: execution_env,
                                cwd: resolved.host_cwd.clone(),
                                limits: javascript_execution_limits(&mut vm),
                                inline_code,
                                wasm_module_bytes: None,
                            },
                            module_reader,
                            guest_reader,
                            vm.runtime_context.clone(),
                        );
                    javascript_engine.dispose_context(&context_id);
                    let execution = execution_result.map_err(javascript_error)?;
                    ActiveExecution::Javascript(execution)
                }
                GuestRuntimeKind::WebAssembly => {
                    let mut wasm_engine =
                        execution_engines.wasm("start WebAssembly child process")?;
                    // These values configure the trusted WASM runner, not
                    // the guest-visible Linux environment.
                    execution_env.extend(sanitize_javascript_child_process_internal_bootstrap_env(
                        &request.options.internal_bootstrap_env,
                    ));
                    execution_env.insert(String::from(WASM_STDIO_SYNC_RPC_ENV), String::from("1"));
                    execution_env.insert(String::from(WASM_EXEC_COMMIT_RPC_ENV), String::from("1"));
                    let wasm_limits = wasm_execution_limits(&mut vm);
                    let wasm_guest_runtime = guest_runtime_identity(
                        &mut vm,
                        Some(u64::from(kernel_pid)),
                        Some(u64::from(parent_kernel_pid)),
                    );
                    let context = wasm_engine.create_context(CreateWasmContextRequest {
                        vm_id: vm_id.to_owned(),
                        module_path: Some(resolved.entrypoint.clone()),
                    });
                    let context_id = context.context_id;
                    let execution_result = wasm_engine
                        .start_execution_with_runtime_async(
                            StartWasmExecutionRequest {
                                vm_id: vm_id.to_owned(),
                                context_id: context_id.clone(),
                                argv: resolved.process_args.clone(),
                                env: execution_env,
                                cwd: resolved.host_cwd.clone(),
                                permission_tier: execution_wasm_permission_tier(
                                    resolved
                                        .wasm_permission_tier
                                        .unwrap_or(WasmPermissionTier::Full),
                                ),
                                limits: wasm_limits,
                                guest_runtime: wasm_guest_runtime,
                            },
                            vm.runtime_context.clone(),
                        )
                        .await;
                    wasm_engine.dispose_context(&context_id);
                    let execution = execution_result.map_err(wasm_error)?;
                    ActiveExecution::Wasm(Box::new(execution))
                }
                GuestRuntimeKind::Python => {
                    let mut python_engine =
                        execution_engines.python("start Python child process")?;
                    // Nested `python` child_process: set up the Pyodide context the
                    // same way the top-level execute path does, so a guest shell or
                    // node parent can spawn `python` exactly like `node`.
                    let python_file_path = if execution_env.contains_key("AGENTOS_PYTHON_ARGV") {
                        execution_env.get("AGENTOS_PYTHON_FILE").map(PathBuf::from)
                    } else {
                        python_file_entrypoint(&resolved.entrypoint)
                    };
                    let pyodide_dist_path = python_engine
                        .bundled_pyodide_dist_path_for_vm(vm_id)
                        .map_err(python_error)?;
                    let pyodide_cache_path = pyodide_dist_path
                        .parent()
                        .and_then(Path::parent)
                        .unwrap_or(pyodide_dist_path.as_path())
                        .join("pyodide-package-cache");
                    add_runtime_guest_path_mapping(
                        &mut execution_env,
                        PYTHON_PYODIDE_GUEST_ROOT,
                        &pyodide_dist_path,
                    );
                    add_runtime_guest_path_mapping(
                        &mut execution_env,
                        PYTHON_PYODIDE_CACHE_GUEST_ROOT,
                        &pyodide_cache_path,
                    );
                    add_runtime_host_access_path(
                        &mut execution_env,
                        "AGENTOS_EXTRA_FS_READ_PATHS",
                        &pyodide_dist_path,
                        true,
                    );
                    add_runtime_host_access_path(
                        &mut execution_env,
                        "AGENTOS_EXTRA_FS_READ_PATHS",
                        &pyodide_cache_path,
                        true,
                    );
                    add_runtime_host_access_path(
                        &mut execution_env,
                        "AGENTOS_EXTRA_FS_WRITE_PATHS",
                        &pyodide_cache_path,
                        false,
                    );
                    let context = python_engine.create_context(CreatePythonContextRequest {
                        vm_id: vm_id.to_owned(),
                        pyodide_dist_path,
                    });
                    let context_id = context.context_id;
                    let execution_result = python_engine
                        .start_execution_with_runtime_async(
                            StartPythonExecutionRequest {
                                vm_id: vm_id.to_owned(),
                                context_id: context_id.clone(),
                                code: resolved.entrypoint.clone(),
                                file_path: python_file_path,
                                env: execution_env,
                                cwd: resolved.host_cwd.clone(),
                                limits: python_execution_limits(&mut vm),
                                guest_runtime: guest_runtime_identity(
                                    &mut vm,
                                    Some(u64::from(kernel_pid)),
                                    Some(u64::from(parent_kernel_pid)),
                                ),
                            },
                            vm.runtime_context.clone(),
                        )
                        .await;
                    python_engine.dispose_context(&context_id);
                    let execution = execution_result.map_err(python_error)?;
                    ActiveExecution::Python(execution)
                }
            };
            let kernel_stdin_writer_fd = if posix_spawn_controls_stdin {
                None
            } else {
                match javascript_child_process_stdin_mode(&request) {
                    "pipe" => Some(install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid)?),
                    "ignore" => {
                        vm.kernel
                            .fd_close(EXECUTION_DRIVER_NAME, kernel_pid, 0)
                            .map_err(kernel_error)?;
                        None
                    }
                    "inherit" => None,
                    _ => Some(install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid)?),
                }
            };
            (
                kernel_pid,
                kernel_handle,
                execution,
                kernel_stdin_writer_fd,
                kernel_stdin_reader_fd,
                posix_spawn_controls_stdin,
            )
        };
        record_execute_phase(
            "child_process_spawn_and_start_execution",
            phase_start.elapsed(),
        );

        let phase_start = Instant::now();
        // Shared-terminal detection: when the child's kernel fd 1 is a PTY (the
        // slave inherited from a TTY shell), record who owns the host-facing
        // master so the child's stdio writes surface through master drains
        // instead of child stdout events (see `tty_master_owner`).
        let child_fd1_is_tty = vm
            .kernel
            .isatty(EXECUTION_DRIVER_NAME, kernel_pid, 1)
            .unwrap_or(false);
        let child_process_group = vm
            .kernel
            .getpgid(EXECUTION_DRIVER_NAME, kernel_pid)
            .map_err(kernel_error)?;
        let process_event_limits = vm.limits.process.clone();
        let shadow_root = normalize_host_path(&vm.cwd);
        let process = vm
            .active_processes
            .get_mut(process_id)
            .ok_or_else(|| missing_process_error(vm_id, process_id))?;
        let inherited_tty_master_owner = if child_fd1_is_tty {
            process
                .tty_master_fd
                .map(|master_fd| (process.kernel_pid, master_fd))
                .or(process.tty_master_owner)
        } else {
            None
        };
        process.child_processes.insert(
            child_process_id.clone(),
            ActiveProcess::new(
                kernel_pid,
                kernel_handle,
                process.runtime_context.clone(),
                process.limits.clone(),
                process_event_capacity,
                resolved.runtime,
                execution,
            )
            .with_event_notify(Arc::clone(&self.process_event_notify))
            .with_process_event_limits(&process_event_limits)
            .with_vm_pending_byte_budgets(
                Arc::clone(&vm_pending_stdin_bytes_budget),
                Arc::clone(&vm_pending_event_bytes_budget),
            )
            .with_detached(request.options.detached)
            .with_guest_cwd(resolved.guest_cwd.clone())
            .with_env(resolved.env.clone())
            .with_shadow_root(shadow_root)
            .with_host_cwd(resolved.host_cwd.clone()),
        );
        {
            let child = process
                .child_processes
                .get_mut(&child_process_id)
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "child process {child_process_id} disappeared during spawn"
                    ))
                })?;
            child.tty_master_owner = inherited_tty_master_owner;
            child.direct_posix_stdin = direct_posix_stdin;
            child.kernel_stdin_reader_fd = kernel_stdin_reader_fd;
            if let Some(kernel_stdin_writer_fd) = kernel_stdin_writer_fd {
                child.kernel_stdin_writer_fd = Some(kernel_stdin_writer_fd);
            }
            prepared_host_net_fds.install(child);
        }
        record_execute_phase("child_process_register", phase_start.elapsed());
        record_execute_phase("child_process_spawn_total", total_start.elapsed());
        Ok(json!({
            "childId": child_process_id,
            "pid": kernel_pid,
            "pgid": child_process_group,
            "directPosixStdin": direct_posix_stdin,
            "command": resolved.command,
            "args": resolved.process_args,
        }))
    }

    pub(crate) fn child_process_sync_max_buffer(
        process: &ActiveProcess,
        requested: Option<usize>,
    ) -> Result<usize, SidecarError> {
        let (limit, setting) = match process.runtime {
            GuestRuntimeKind::JavaScript => (
                process.limits.js_runtime.captured_output_limit_bytes,
                "limits.jsRuntime.capturedOutputLimitBytes",
            ),
            GuestRuntimeKind::Python => (
                process.limits.python.output_buffer_max_bytes,
                "limits.python.outputBufferMaxBytes",
            ),
            GuestRuntimeKind::WebAssembly => (
                process.limits.wasm.captured_output_limit_bytes,
                "limits.wasm.capturedOutputLimitBytes",
            ),
        };
        let requested = requested.unwrap_or(1024 * 1024);
        if requested > limit {
            return Err(SidecarError::Execution(format!(
                "ERR_AGENTOS_CHILD_PROCESS_BUFFER_LIMIT: child process maxBuffer {requested} exceeds {setting} ({limit}); raise {setting} for larger captured output"
            )));
        }
        Ok(requested)
    }

    pub(super) async fn begin_javascript_child_process_sync(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: JavascriptChildProcessSpawnRequest,
        max_buffer: Option<usize>,
        completion: PendingChildProcessSyncCompletion,
    ) -> Result<(), SidecarError> {
        let max_buffer = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let process = vm
                .active_processes
                .get(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            Self::child_process_sync_max_buffer(process, max_buffer)?
        };
        let sync_input = javascript_child_process_sync_input_bytes(request.options.input.as_ref())?;
        let deadline = request
            .options
            .timeout
            .map(|timeout_ms| Instant::now() + Duration::from_millis(timeout_ms));
        let timeout_signal = request
            .options
            .kill_signal
            .clone()
            .unwrap_or_else(|| String::from("SIGTERM"));
        let spawned = self
            .spawn_descendant_javascript_child_process(vm_id, process_id, &[], request)
            .await?;
        let child_process_id = spawned
            .get("childId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "child_process.spawn_sync response is missing childId",
                ))
            })?
            .to_owned();
        let pid = spawned
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "child_process.spawn_sync response is missing a valid pid",
                ))
            })?;

        if let Some(input) = sync_input.as_deref() {
            self.write_javascript_child_process_stdin(vm_id, process_id, &child_process_id, input)?;
        }
        self.close_javascript_child_process_stdin(vm_id, process_id, &child_process_id)?;

        let (runtime, notify) = {
            let mut vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let process = vm
                .active_processes
                .get_mut(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            process.pending_child_process_sync.insert(
                child_process_id,
                PendingChildProcessSync {
                    pid,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    max_buffer,
                    deadline,
                    timeout_signal,
                    kill_sent: false,
                    timed_out: false,
                    max_buffer_exceeded: false,
                    completion,
                },
            );
            (
                process.runtime_context.clone(),
                Arc::clone(&process.process_event_notify),
            )
        };
        if let Some(deadline) = deadline {
            let delay = deadline.saturating_duration_since(Instant::now());
            runtime
                .spawn(agentos_runtime::TaskClass::Timer, async move {
                    tokio::time::sleep(delay).await;
                    notify.notify_one();
                })
                .map_err(SidecarError::from)?;
        }
        Ok(())
    }

    pub(crate) async fn defer_javascript_child_process_sync(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: JavascriptChildProcessSpawnRequest,
        max_buffer: Option<usize>,
    ) -> Result<JavascriptSyncRpcServiceResponse, SidecarError> {
        let (respond_to, receiver) = tokio::sync::oneshot::channel();
        self.begin_javascript_child_process_sync(
            vm_id,
            process_id,
            request,
            max_buffer,
            PendingChildProcessSyncCompletion::Javascript(respond_to),
        )
        .await?;
        Ok(JavascriptSyncRpcServiceResponse::Deferred {
            receiver,
            timeout: None,
            task_class: agentos_runtime::TaskClass::Vm,
        })
    }

    /// Replace a running guest process image for execve(2) without creating a
    /// child. Resolution is deliberately performed against an empty inherited
    /// environment: execve's supplied envp replaces the old environment rather
    /// than being overlaid on it. The existing PID, process tree, cwd, stdio,
    /// and non-CLOEXEC kernel descriptors remain attached to `ActiveProcess`.
    pub(crate) fn exec_javascript_process_image(
        &mut self,
        vm_id: &str,
        root_process_id: &str,
        process_path: &[&str],
        request: JavascriptChildProcessSpawnRequest,
    ) -> Result<(), SidecarError> {
        let handle = self
            .vms
            .handle(vm_id)
            .ok_or_else(|| missing_vm_error(vm_id))?;
        let vms = OwnedChildProcessVm {
            vm_id: vm_id.to_owned(),
            handle,
        };
        #[cfg(test)]
        let fail_after_commit = std::mem::take(&mut self.fail_next_exec_start_after_commit);
        #[cfg(not(test))]
        let fail_after_commit = false;
        Self::exec_javascript_process_image_owned(
            &vms,
            &self.cache_root,
            fail_after_commit,
            vm_id,
            root_process_id,
            process_path,
            request,
        )
    }

    fn exec_javascript_process_image_owned(
        vms: &OwnedChildProcessVm,
        cache_root: &Path,
        fail_after_commit: bool,
        vm_id: &str,
        root_process_id: &str,
        process_path: &[&str],
        mut request: JavascriptChildProcessSpawnRequest,
    ) -> Result<(), SidecarError> {
        #[cfg(not(test))]
        let _ = fail_after_commit;
        if request.options.executable_fd.is_some() {
            return Err(SidecarError::InvalidState(String::from(
                "EINVAL: executableFd is only valid for process.exec_fd_image_commit",
            )));
        }
        if request.options.shell || request.options.detached {
            return Err(SidecarError::InvalidState(String::from(
                "EINVAL: execve does not accept shell or detached process options",
            )));
        }

        let (
            guest_cwd,
            host_cwd,
            kernel_pid,
            parent_kernel_pid,
            current_runtime,
            direct_posix_stdin,
        ) = {
            let vm = vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let root = vm
                .active_processes
                .get(root_process_id)
                .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
            let process = Self::active_process_by_path(root, process_path).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "unknown process path {} during execve",
                    Self::child_process_path_label(root_process_id, process_path)
                ))
            })?;
            let parent_kernel_pid = vm
                .kernel
                .list_processes()
                .get(&process.kernel_pid)
                .map(|entry| entry.ppid)
                .unwrap_or_default();
            (
                process.guest_cwd.clone(),
                process.host_cwd.clone(),
                process.kernel_pid,
                parent_kernel_pid,
                process.runtime.clone(),
                process.direct_posix_stdin,
            )
        };

        if request.command.is_empty() {
            return Err(SidecarError::InvalidState(String::from(
                "ENOENT: execve path is empty",
            )));
        }
        // execve resolves a relative pathname from cwd; it never searches
        // PATH. Making the command explicitly path-like keeps the shared child
        // resolver from taking its spawnp/PATH branch for a bare relative name.
        request.command = if request.command.starts_with('/') {
            normalize_path(&request.command)
        } else {
            normalize_path(&format!("{guest_cwd}/{}", request.command))
        };
        let literal_exec_path = request.command.clone();
        request.command = {
            let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            vm.kernel
                .validate_executable_path(&literal_exec_path, &guest_cwd)
                .map_err(kernel_error)?
        };
        request.options.cwd = None;
        request.options.shell = false;
        request.options.detached = false;

        let mut resolved = {
            let vm = vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            Self::resolve_javascript_child_process_execution_with_mode(
                &vm,
                &BTreeMap::new(),
                &guest_cwd,
                &host_cwd,
                &request,
                true,
                None,
            )?
        };
        apply_child_process_argv0(&mut resolved, request.options.argv0.as_deref());
        if resolved.binding_command {
            return Err(SidecarError::InvalidState(format!(
                "ENOEXEC: exec format error: {}",
                request.command
            )));
        }
        if request.options.local_replacement
            && current_runtime == GuestRuntimeKind::WebAssembly
            && resolved.runtime == GuestRuntimeKind::WebAssembly
        {
            let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            // The runner has already compiled the replacement before it asks
            // the sidecar to commit. Recursively validate the original image
            // and every `#!` interpreter in the kernel so scripts retain
            // Linux pathname, mode, format, and recursion errors without
            // requiring the script itself to contain raw WASM bytes.
            vm.kernel
                .validate_wasm_exec_image(&request.command, &guest_cwd)
                .map_err(kernel_error)?;
        } else {
            let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            validate_exact_exec_image_format(&mut vm, &request.command, &resolved.runtime)?;
        }
        {
            let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            stage_agentos_package_command(&mut vm, &mut resolved)?;
        }
        // Keep guest-visible envp separate from executor bootstrap variables
        // added during resolution. Both local and separate-runtime exec paths
        // must publish and inherit exactly the supplied environment.
        let replacement_guest_env = request.options.env.clone();

        if request.options.local_replacement {
            if current_runtime != GuestRuntimeKind::WebAssembly
                || resolved.runtime != GuestRuntimeKind::WebAssembly
            {
                return Err(SidecarError::InvalidState(format!(
                    "ENOEXEC: in-place exec only supports WebAssembly images: {}",
                    literal_exec_path
                )));
            }
            let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            // execve's envp is a complete replacement. `resolved.env` also
            // contains host/runtime control variables injected while locating
            // the module; those must never leak into the process's Linux-visible
            // environment or become inherited by its future children.
            let retained_internal_fds = Self::active_process_by_path(
                vm.active_processes
                    .get(root_process_id)
                    .ok_or_else(|| missing_process_error(vm_id, root_process_id))?,
                process_path,
            )
            .and_then(|process| process.kernel_stdin_writer_fd)
            .into_iter()
            .collect::<Vec<_>>();
            vm.kernel
                .exec_process_retaining_internal_fds(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    WASM_COMMAND,
                    resolved.process_args.clone(),
                    replacement_guest_env.clone(),
                    resolved.guest_cwd.clone(),
                    &retained_internal_fds,
                    &request.options.cloexec_fds,
                    Some(&literal_exec_path),
                )
                .map_err(kernel_error)?;

            let root = vm
                .active_processes
                .get_mut(root_process_id)
                .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
            let process =
                Self::active_process_by_path_mut(root, process_path).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "process disappeared during execve: {}",
                        Self::child_process_path_label(root_process_id, process_path)
                    ))
                })?;
            process.guest_cwd = resolved.guest_cwd;
            process.host_cwd = resolved.host_cwd;
            process.env = replacement_guest_env;
            process.exit_signal = None;
            process.exit_core_dumped = false;
            process.clear_deferred_kernel_wait_rpc();
            process.module_resolution_cache = Default::default();
            discard_replaced_image_pending_events(process);

            // POSIX exec resets caught dispositions to default, preserves
            // ignored dispositions, and preserves the signal mask/pending set.
            reset_caught_signal_dispositions_after_exec(
                &mut vm.signal_states,
                root_process_id,
                process_path,
            );
            return Ok(());
        }

        let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        let mut vm = &mut *vm;
        let execution_engines = vm.execution_engines.clone();
        let mut execution_env = resolved.env.clone();
        execution_env.insert(
            String::from(EXECUTION_SANDBOX_ROOT_ENV),
            normalize_host_path(&vm.cwd).to_string_lossy().into_owned(),
        );
        let replacement = match resolved.runtime {
            GuestRuntimeKind::JavaScript => {
                let mut javascript_engine =
                    execution_engines.javascript("prepare JavaScript exec replacement")?;
                execution_env.extend(sanitize_javascript_child_process_internal_bootstrap_env(
                    &request.options.internal_bootstrap_env,
                ));
                if !process_path.is_empty() {
                    execution_env.remove("AGENTOS_EAGER_STDIN_HANDLE");
                }
                execution_env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
                if direct_posix_stdin {
                    execution_env.insert(
                        String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
                        String::from("1"),
                    );
                } else {
                    execution_env.remove("AGENTOS_FORWARD_KERNEL_STDIN_RPC");
                }
                let launch_entrypoint = resolve_agentos_package_javascript_launch_entrypoint(
                    &mut vm,
                    &mut execution_env,
                )
                .unwrap_or_else(|| resolved.entrypoint.clone());
                let inline_code = load_javascript_entrypoint_source(
                    &mut vm,
                    &resolved.host_cwd,
                    &launch_entrypoint,
                    &execution_env,
                );
                prepare_javascript_shadow(&mut vm, &resolved, &execution_env)?;
                let built_reader = build_module_reader(&mut vm, &resolved);
                let guest_reader = built_reader.clone().map(|reader| {
                    Box::new(crate::plugins::host_dir::SessionModuleReader::new(reader))
                        as Box<dyn GuestModuleReader>
                });
                let module_reader =
                    built_reader.map(|reader| Box::new(reader) as Box<dyn ModuleFsReader + Send>);
                let context = javascript_engine.create_context(CreateJavascriptContextRequest {
                    vm_id: vm_id.to_owned(),
                    bootstrap_module: None,
                    compile_cache_root: Some(cache_root.join("node-compile-cache")),
                });
                let context_id = context.context_id;
                let replacement_result = javascript_engine
                    .prepare_execution_with_module_reader_and_runtime(
                        StartJavascriptExecutionRequest {
                            guest_runtime: guest_runtime_identity(
                                &mut vm,
                                Some(u64::from(kernel_pid)),
                                Some(u64::from(parent_kernel_pid)),
                            ),
                            vm_id: vm_id.to_owned(),
                            context_id: context_id.clone(),
                            argv: std::iter::once(launch_entrypoint)
                                .chain(resolved.execution_args.clone())
                                .collect(),
                            argv0: request.options.argv0.clone(),
                            env: execution_env,
                            cwd: resolved.host_cwd.clone(),
                            limits: javascript_execution_limits(&mut vm),
                            inline_code,
                            wasm_module_bytes: None,
                        },
                        module_reader,
                        guest_reader,
                        vm.runtime_context.clone(),
                    );
                javascript_engine.dispose_context(&context_id);
                ActiveExecution::Javascript(replacement_result.map_err(javascript_error)?)
            }
            GuestRuntimeKind::WebAssembly => {
                let mut wasm_engine =
                    execution_engines.wasm("prepare WebAssembly exec replacement")?;
                execution_env.extend(sanitize_javascript_child_process_internal_bootstrap_env(
                    &request.options.internal_bootstrap_env,
                ));
                execution_env.insert(String::from(WASM_STDIO_SYNC_RPC_ENV), String::from("1"));
                execution_env.insert(String::from(WASM_EXEC_COMMIT_RPC_ENV), String::from("1"));
                let context = wasm_engine.create_context(CreateWasmContextRequest {
                    vm_id: vm_id.to_owned(),
                    module_path: Some(resolved.entrypoint.clone()),
                });
                let context_id = context.context_id;
                let replacement_result = wasm_engine.prepare_execution(StartWasmExecutionRequest {
                    vm_id: vm_id.to_owned(),
                    context_id: context_id.clone(),
                    argv: resolved.process_args.clone(),
                    env: execution_env,
                    cwd: resolved.host_cwd.clone(),
                    permission_tier: execution_wasm_permission_tier(
                        resolved
                            .wasm_permission_tier
                            .unwrap_or(WasmPermissionTier::Full),
                    ),
                    limits: wasm_execution_limits(&mut vm),
                    guest_runtime: guest_runtime_identity(
                        &mut vm,
                        Some(u64::from(kernel_pid)),
                        Some(u64::from(parent_kernel_pid)),
                    ),
                });
                wasm_engine.dispose_context(&context_id);
                ActiveExecution::Wasm(Box::new(replacement_result.map_err(wasm_error)?))
            }
            GuestRuntimeKind::Python => {
                let mut python_engine =
                    execution_engines.python("prepare Python exec replacement")?;
                let python_file_path = if execution_env.contains_key("AGENTOS_PYTHON_ARGV") {
                    execution_env.get("AGENTOS_PYTHON_FILE").map(PathBuf::from)
                } else {
                    python_file_entrypoint(&resolved.entrypoint)
                };
                let pyodide_dist_path = python_engine
                    .bundled_pyodide_dist_path_for_vm(vm_id)
                    .map_err(python_error)?;
                let pyodide_cache_path = pyodide_dist_path
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or(pyodide_dist_path.as_path())
                    .join("pyodide-package-cache");
                add_runtime_guest_path_mapping(
                    &mut execution_env,
                    PYTHON_PYODIDE_GUEST_ROOT,
                    &pyodide_dist_path,
                );
                add_runtime_guest_path_mapping(
                    &mut execution_env,
                    PYTHON_PYODIDE_CACHE_GUEST_ROOT,
                    &pyodide_cache_path,
                );
                add_runtime_host_access_path(
                    &mut execution_env,
                    "AGENTOS_EXTRA_FS_READ_PATHS",
                    &pyodide_dist_path,
                    true,
                );
                add_runtime_host_access_path(
                    &mut execution_env,
                    "AGENTOS_EXTRA_FS_READ_PATHS",
                    &pyodide_cache_path,
                    true,
                );
                add_runtime_host_access_path(
                    &mut execution_env,
                    "AGENTOS_EXTRA_FS_WRITE_PATHS",
                    &pyodide_cache_path,
                    false,
                );
                let context = python_engine.create_context(CreatePythonContextRequest {
                    vm_id: vm_id.to_owned(),
                    pyodide_dist_path,
                });
                let context_id = context.context_id;
                let replacement_result =
                    python_engine.prepare_execution(StartPythonExecutionRequest {
                        vm_id: vm_id.to_owned(),
                        context_id: context_id.clone(),
                        code: resolved.entrypoint.clone(),
                        file_path: python_file_path,
                        env: execution_env,
                        cwd: resolved.host_cwd.clone(),
                        limits: python_execution_limits(&mut vm),
                        guest_runtime: guest_runtime_identity(
                            &mut vm,
                            Some(u64::from(kernel_pid)),
                            Some(u64::from(parent_kernel_pid)),
                        ),
                    });
                python_engine.dispose_context(&context_id);
                ActiveExecution::Python(replacement_result.map_err(python_error)?)
            }
        };

        // Hard production invariant: a cross-runtime exec image must still
        // own its deferred execute payload here. This check makes it
        // impossible to accidentally regress to the old start-before-commit
        // path without failing execve before kernel state is mutated.
        if !replacement.is_prepared_for_start() {
            return Err(SidecarError::InvalidState(String::from(
                "EIO: cross-runtime execve replacement started before kernel commit",
            )));
        }

        let kernel_command = match resolved.runtime {
            GuestRuntimeKind::JavaScript => JAVASCRIPT_COMMAND,
            GuestRuntimeKind::WebAssembly => WASM_COMMAND,
            GuestRuntimeKind::Python => PYTHON_COMMAND,
        };
        let retained_internal_fds = Self::active_process_by_path(
            vm.active_processes
                .get(root_process_id)
                .ok_or_else(|| missing_process_error(vm_id, root_process_id))?,
            process_path,
        )
        .and_then(|process| process.kernel_stdin_writer_fd)
        .into_iter()
        .collect::<Vec<_>>();
        if let Err(error) = vm.kernel.exec_process_retaining_internal_fds(
            EXECUTION_DRIVER_NAME,
            kernel_pid,
            kernel_command,
            resolved.process_args.clone(),
            replacement_guest_env.clone(),
            resolved.guest_cwd.clone(),
            &retained_internal_fds,
            &request.options.cloexec_fds,
            Some(&literal_exec_path),
        ) {
            let mut replacement = replacement;
            if let Err(terminate_error) = replacement.terminate() {
                tracing::warn!(
                    vm_id,
                    kernel_pid,
                    error = %terminate_error,
                    "failed to terminate prepared replacement after execve kernel commit was rejected"
                );
            }
            return Err(kernel_error(error));
        }

        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let root = vm
            .active_processes
            .get_mut(root_process_id)
            .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
        let process = Self::active_process_by_path_mut(root, process_path).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "process disappeared during execve: {}",
                Self::child_process_path_label(root_process_id, process_path)
            ))
        })?;
        let mut old_execution = std::mem::replace(&mut process.execution, replacement);
        process.runtime = resolved.runtime;
        process.guest_cwd = resolved.guest_cwd;
        process.host_cwd = resolved.host_cwd;
        process.env = replacement_guest_env;
        process.exit_signal = None;
        process.exit_core_dumped = false;
        process.clear_deferred_kernel_wait_rpc();
        process.module_resolution_cache = Default::default();
        discard_replaced_image_pending_events(process);
        rebind_process_runtime_event_targets(process, &kernel_readiness);

        // POSIX exec resets caught dispositions to default but preserves
        // dispositions explicitly set to ignore.
        let signal_key = reset_caught_signal_dispositions_after_exec(
            &mut vm.signal_states,
            root_process_id,
            process_path,
        );
        // The replacement isolate was registered and fully loaded before the
        // kernel commit, but no guest code was enqueued. Only start it now,
        // after both kernel-visible process state and sidecar-owned descriptors
        // and event targets point at the replacement image.
        #[cfg(test)]
        let replacement_start_error = if fail_after_commit {
            Some(SidecarError::Execution(String::from(
                "injected post-commit execve start failure",
            )))
        } else {
            process.execution.start_prepared().err()
        };
        #[cfg(not(test))]
        let replacement_start_error = process.execution.start_prepared().err();
        if let Some(error) = replacement_start_error.as_ref() {
            let message = format!("execve replacement runtime failed to start: {error}\n");
            if let Err(queue_error) = process
                .queue_pending_execution_event(ActiveExecutionEvent::Stderr(message.into_bytes()))
                .and_then(|_| {
                    process.queue_pending_execution_event(ActiveExecutionEvent::Exited(127))
                })
            {
                tracing::error!(
                    vm_id,
                    process_id = %signal_key,
                    error = %queue_error,
                    "failed to queue fatal post-commit execve start failure"
                );
            }
            process.kernel_handle.finish(127);
            if let Err(terminate_error) = process.execution.terminate() {
                tracing::error!(
                    vm_id,
                    process_id = %signal_key,
                    error = %terminate_error,
                    "failed to terminate replacement runtime after post-commit execve start failure"
                );
            }
        }
        // The old image is blocked in the exec RPC. Terminating it after the
        // atomic state swap ensures no success response can resume old code.
        if let Err(error) = old_execution.terminate() {
            tracing::warn!(
                vm_id,
                process_id = %signal_key,
                error = %error,
                "execve committed but the replaced runtime image reported a termination error"
            );
        }
        if let Some(error) = replacement_start_error {
            // The kernel execve commit is irrevocable. Linux does not return an
            // errno into the old image when a post-commit loader/start failure
            // occurs; the new process image dies. Log the typed host failure,
            // leave the queued exit for normal cleanup, and report committed
            // success to the service loop so it never replies to old code.
            tracing::error!(
                vm_id,
                process_id = %signal_key,
                error = %error,
                "execve replacement failed after commit; terminating the replacement process"
            );
        }
        Ok(())
    }

    /// Commit metadata for an fexecve image that the trusted WASM runner has
    /// already read and compiled from its live private descriptor. This route
    /// deliberately performs no pathname resolution or reopen: the descriptor
    /// may name an unlinked file, and the runner owns that open-file identity.
    pub(crate) fn commit_wasm_fd_process_image(
        &mut self,
        vm_id: &str,
        root_process_id: &str,
        process_path: &[&str],
        request: JavascriptChildProcessSpawnRequest,
    ) -> Result<(), SidecarError> {
        let handle = self
            .vms
            .handle(vm_id)
            .ok_or_else(|| missing_vm_error(vm_id))?;
        Self::commit_wasm_fd_process_image_owned(
            &OwnedChildProcessVm {
                vm_id: vm_id.to_owned(),
                handle,
            },
            vm_id,
            root_process_id,
            process_path,
            request,
        )
    }

    fn commit_wasm_fd_process_image_owned(
        vms: &OwnedChildProcessVm,
        vm_id: &str,
        root_process_id: &str,
        process_path: &[&str],
        request: JavascriptChildProcessSpawnRequest,
    ) -> Result<(), SidecarError> {
        if !request.options.local_replacement || request.options.executable_fd.is_none() {
            return Err(SidecarError::InvalidState(String::from(
                "EINVAL: fd-image exec commit requires localReplacement and executableFd",
            )));
        }
        if request.options.shell || request.options.detached || request.options.cwd.is_some() {
            return Err(SidecarError::InvalidState(String::from(
                "EINVAL: fexecve does not accept shell, detached, or cwd options",
            )));
        }

        let (kernel_pid, retained_internal_fds) = {
            let vm = vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let root = vm
                .active_processes
                .get(root_process_id)
                .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
            let process = Self::active_process_by_path(root, process_path).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "unknown process path {} during fexecve commit",
                    Self::child_process_path_label(root_process_id, process_path)
                ))
            })?;
            if process.runtime != GuestRuntimeKind::WebAssembly {
                return Err(SidecarError::InvalidState(String::from(
                    "ENOEXEC: fd-image exec commit requires a WebAssembly process",
                )));
            }
            (
                process.kernel_pid,
                process
                    .kernel_stdin_writer_fd
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
        };

        let mut argv = Vec::with_capacity(request.args.len().saturating_add(1));
        argv.push(
            request
                .options
                .argv0
                .clone()
                .unwrap_or_else(|| request.command.clone()),
        );
        argv.extend(request.args);
        let replacement_guest_env = request.options.env;

        let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        vm.kernel
            .exec_process_retaining_internal_fds(
                EXECUTION_DRIVER_NAME,
                kernel_pid,
                WASM_COMMAND,
                argv,
                replacement_guest_env.clone(),
                String::new(),
                &retained_internal_fds,
                &request.options.cloexec_fds,
                None,
            )
            .map_err(kernel_error)?;

        let root = vm
            .active_processes
            .get_mut(root_process_id)
            .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
        let process = Self::active_process_by_path_mut(root, process_path).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "process disappeared during fexecve commit: {}",
                Self::child_process_path_label(root_process_id, process_path)
            ))
        })?;
        process.env = replacement_guest_env;
        process.exit_signal = None;
        process.exit_core_dumped = false;
        process.clear_deferred_kernel_wait_rpc();
        process.module_resolution_cache = Default::default();
        discard_replaced_image_pending_events(process);
        reset_caught_signal_dispositions_after_exec(
            &mut vm.signal_states,
            root_process_id,
            process_path,
        );
        Ok(())
    }

    pub(crate) fn spawn_descendant_javascript_child_process(
        &self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        request: JavascriptChildProcessSpawnRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, SidecarError>> + 'static>> {
        let vm_id = vm_id.to_owned();
        let process_id = process_id.to_owned();
        let current_process_path = current_process_path
            .iter()
            .map(|segment| (*segment).to_owned())
            .collect::<Vec<_>>();
        let Some(handle) = self.vms.handle(&vm_id) else {
            return Box::pin(async move { Err(missing_vm_error(&vm_id)) });
        };
        let process_event_capacity = self.config.runtime.protocol.max_process_events;
        let sidecar_requests = SharedSidecarRequestClient::clone(&self.sidecar_requests);
        let process_event_notify = Arc::clone(&self.process_event_notify);
        let cache_root = PathBuf::clone(&self.cache_root);
        Self::build_owned_descendant_javascript_child_process_spawn(
            handle,
            vm_id,
            process_id,
            current_process_path,
            request,
            process_event_capacity,
            sidecar_requests,
            process_event_notify,
            cache_root,
        )
    }

    pub(crate) fn build_owned_descendant_javascript_child_process_spawn(
        handle: VmHandle,
        vm_id: String,
        process_id: String,
        current_process_path: Vec<String>,
        request: JavascriptChildProcessSpawnRequest,
        process_event_capacity: usize,
        sidecar_requests: SharedSidecarRequestClient,
        process_event_notify: Arc<tokio::sync::Notify>,
        cache_root: PathBuf,
    ) -> Pin<Box<dyn Future<Output = Result<Value, SidecarError>> + 'static>> {
        let vms = OwnedChildProcessVm {
            vm_id: vm_id.clone(),
            handle,
        };
        Box::pin(async move {
            let vm_id = vm_id.as_str();
            let process_id = process_id.as_str();
            let current_process_path = current_process_path
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let current_process_path = current_process_path.as_slice();
            let mut request = request;
            let spawn_attributes = javascript_spawn_attributes(&request.options)?;
            let requested_pgid = spawn_attributes.process_group;
            let current_process_label =
                NativeSidecar::<B>::child_process_path_label(process_id, current_process_path);
            let parent_sync_roots = {
                let vm = vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                let root = vm
                    .active_processes
                    .get(process_id)
                    .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                let parent = NativeSidecar::<B>::active_process_by_path(root, current_process_path)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                        "unknown child process path {current_process_label} during nested spawn"
                    ))
                    })?;
                (parent.host_write_dirty_recursive()
                    || !parent.clean_host_writes_are_observable_recursive())
                .then(|| {
                    (
                        parent.host_cwd.clone(),
                        parent.guest_cwd.clone(),
                        parent.runtime != GuestRuntimeKind::JavaScript,
                    )
                })
            };
            if let Some((host_cwd, guest_cwd, sync_root_shadow)) = parent_sync_roots {
                let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                sync_process_host_roots_to_kernel(
                    &mut vm,
                    &host_cwd,
                    &guest_cwd,
                    sync_root_shadow,
                )?;
            }
            let prepared_host_net_fds = {
                let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                let mut vm = &mut *vm;
                let current_network_counts = vm_spawn_host_net_resource_counts(&mut vm);
                let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
                let root = active_processes
                    .get_mut(process_id)
                    .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                let parent =
                    NativeSidecar::<B>::active_process_by_path_mut(root, current_process_path)
                        .ok_or_else(|| {
                            SidecarError::InvalidState(format!(
                                "unknown child process path {} during host-network fd inheritance",
                                NativeSidecar::<B>::child_process_path_label(
                                    process_id,
                                    current_process_path
                                )
                            ))
                        })?;
                prepare_spawn_host_net_fds(
                    kernel,
                    parent,
                    current_network_counts,
                    &request.options.spawn_host_net_fds,
                    &request.options.spawn_fd_mappings,
                    &request.options.spawn_file_actions,
                )?
            };
            let prepared_spawn_actions = if !prepared_host_net_fds.kernel_actions.is_empty() {
                let (parent_pid, parent_cwd) = {
                    let vm = vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                    let root = vm
                        .active_processes
                        .get(process_id)
                        .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                    let parent =
                        NativeSidecar::<B>::active_process_by_path(root, current_process_path)
                            .ok_or_else(|| {
                                SidecarError::InvalidState(format!(
                                    "unknown child process path {} during spawn file actions",
                                    NativeSidecar::<B>::child_process_path_label(
                                        process_id,
                                        current_process_path
                                    )
                                ))
                            })?;
                    let initial_cwd = request
                        .options
                        .cwd
                        .as_deref()
                        .map(|cwd| {
                            if cwd.starts_with('/') {
                                normalize_path(cwd)
                            } else {
                                normalize_path(&format!("{}/{cwd}", parent.guest_cwd))
                            }
                        })
                        .unwrap_or_else(|| parent.guest_cwd.clone());
                    (parent.kernel_pid, initial_cwd)
                };
                let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                Some(preapply_posix_spawn_file_actions(
                    &mut vm.kernel,
                    parent_pid,
                    &parent_cwd,
                    requested_pgid,
                    &request.options.spawn_fd_mappings,
                    &prepared_host_net_fds.kernel_actions,
                )?)
            } else {
                None
            };
            if let Some(prepared) = prepared_spawn_actions.as_ref() {
                request.options.cwd = Some(prepared.cwd.clone());
            }
            {
                let parent_guest_cwd = {
                    let vm = vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                    let root = vm
                        .active_processes
                        .get(process_id)
                        .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                    NativeSidecar::<B>::active_process_by_path(root, current_process_path)
                        .ok_or_else(|| {
                            SidecarError::InvalidState(format!(
                                "unknown child process path {} during program resolution",
                                NativeSidecar::<B>::child_process_path_label(
                                    process_id,
                                    current_process_path
                                )
                            ))
                        })?
                        .guest_cwd
                        .clone()
                };
                let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                resolve_posix_spawn_program(&mut vm, &parent_guest_cwd, &mut request)?;
            }
            let total_start = Instant::now();
            let phase_start = Instant::now();
            let (parent_env, parent_guest_cwd, parent_host_cwd, parent_kernel_pid) = {
                let vm = vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                let root = vm
                    .active_processes
                    .get(process_id)
                    .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                let parent = NativeSidecar::<B>::active_process_by_path(root, current_process_path)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                        "unknown child process path {current_process_label} during nested spawn"
                    ))
                    })?;
                (
                    parent.env.clone(),
                    parent.guest_cwd.clone(),
                    parent.host_cwd.clone(),
                    parent.kernel_pid,
                )
            };
            let mut resolved = if !request.options.spawn_exact_path
                && request.options.spawn_search_path.is_none()
            {
                resolve_owned_javascript_child_process_with_shebang::<B>(
                    &vms,
                    vm_id,
                    &parent_env,
                    &parent_guest_cwd,
                    &parent_host_cwd,
                    &mut request,
                )?
            } else {
                let vm = vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                NativeSidecar::<B>::resolve_javascript_child_process_execution_with_mode(
                    &vm,
                    &parent_env,
                    &parent_guest_cwd,
                    &parent_host_cwd,
                    &request,
                    request.options.spawn_exact_path,
                    request.options.spawn_search_path.as_deref(),
                )?
            };
            apply_child_process_argv0(&mut resolved, request.options.argv0.as_deref());
            {
                let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                stage_agentos_package_command(&mut vm, &mut resolved)?;
            }
            tracing::debug!(
                vm_id,
                process_id,
                parent = %current_process_label,
                command = %resolved.command,
                runtime = ?resolved.runtime,
                entrypoint = %resolved.entrypoint,
                execution_args = ?resolved.execution_args,
                parent_guest_cwd = %parent_guest_cwd,
                requested_cwd = ?request.options.cwd,
                guest_cwd = %resolved.guest_cwd,
                host_cwd = %resolved.host_cwd.display(),
                "resolved nested JavaScript child process"
            );
            let resolved = resolved;
            if prepared_host_net_fds.inherited_fd_count() != 0
                && (resolved.runtime != GuestRuntimeKind::WebAssembly || resolved.binding_command)
            {
                return Err(SidecarError::InvalidState(String::from(
                    "ENOTSUP: inherited host-network fds require a WebAssembly child runtime",
                )));
            }
            record_execute_phase("child_process_resolve_execution", phase_start.elapsed());
            let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let execution_engines = vm.execution_engines.clone();
            enforce_resolved_wasm_execute_dac(&mut vm, parent_kernel_pid, &resolved)?;
            let vm_pending_stdin_bytes_budget = Arc::clone(&vm.pending_stdin_bytes_budget);
            let vm_pending_event_bytes_budget = Arc::clone(&vm.pending_event_bytes_budget);
            let phase_start = Instant::now();
            let child_process_id = {
                let root = vm
                    .active_processes
                    .get_mut(process_id)
                    .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                let parent =
                    NativeSidecar::<B>::active_process_by_path_mut(root, current_process_path)
                        .ok_or_else(|| {
                            SidecarError::InvalidState(format!(
                        "unknown child process path {current_process_label} during nested spawn"
                    ))
                        })?;
                parent.allocate_child_process_id()
            };
            let mut child_path = current_process_path.to_vec();
            child_path.push(child_process_id.as_str());
            let mut pending_kernel_handle: Option<KernelProcessHandle>;
            let spawned = if resolved.binding_command {
                let binding_resolution = resolve_binding_command(
                    &mut vm,
                    &resolved.command,
                    &resolved.execution_args,
                    Some(&resolved.guest_cwd),
                )?
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "binding command no longer resolves: {}",
                        resolved.command
                    ))
                })?;
                let kernel_handle = vm
                    .kernel
                    .create_virtual_process_with_process_group(
                        EXECUTION_DRIVER_NAME,
                        BINDING_DRIVER_NAME,
                        &resolved.command,
                        resolved.process_args.clone(),
                        VirtualProcessOptions {
                            parent_pid: Some(parent_kernel_pid),
                            env: resolved.env.clone(),
                            cwd: Some(resolved.guest_cwd.clone()),
                        },
                        requested_pgid,
                    )
                    .map_err(kernel_error)?;
                let kernel_pid = kernel_handle.pid();
                if let Some(prepared) = prepared_spawn_actions {
                    install_preapplied_posix_spawn_file_actions(
                        &mut vm.kernel,
                        &kernel_handle,
                        prepared,
                    )?;
                } else {
                    apply_posix_spawn_file_actions_or_rollback(
                        &mut vm.kernel,
                        &kernel_handle,
                        &resolved.guest_cwd,
                        &request.options.spawn_fd_mappings,
                        &prepared_host_net_fds.kernel_actions,
                    )?;
                }
                apply_spawn_session_or_rollback(
                    &mut vm.kernel,
                    &kernel_handle,
                    spawn_attributes.new_session || request.options.detached,
                )?;
                pending_kernel_handle = Some(kernel_handle.clone());
                let binding_execution = BindingExecution::with_event_notify(
                    Arc::clone(&process_event_notify),
                    process_event_capacity,
                )
                .with_vm_pending_event_bytes_budget(Arc::clone(&vm_pending_event_bytes_budget));
                let cancelled = binding_execution.cancelled.clone();
                let pending_events = binding_execution.pending_events.clone();
                let event_overflow_reason = binding_execution.event_overflow_reason.clone();
                let pending_event_bytes = binding_execution.pending_event_bytes.clone();
                let pending_event_count_limit = binding_execution.pending_event_count_limit.clone();
                let pending_event_bytes_limit = binding_execution.pending_event_bytes_limit.clone();
                let binding_vm_pending_event_bytes_budget =
                    binding_execution.vm_pending_event_bytes_budget.clone();
                let event_notify = binding_execution.event_notify.clone();
                spawn_binding_process_events(BindingProcessEventRequest {
                    runtime_context: vm.runtime_context.clone(),
                    sidecar_requests: sidecar_requests.clone(),
                    connection_id: vm.connection_id.clone(),
                    session_id: vm.session_id.clone(),
                    vm_id: vm_id.to_owned(),
                    binding_resolution,
                    cancelled,
                    pending_events,
                    event_overflow_reason,
                    pending_event_bytes,
                    pending_event_count_limit,
                    pending_event_bytes_limit,
                    vm_pending_event_bytes_budget: binding_vm_pending_event_bytes_budget,
                    event_notify,
                });
                (
                    kernel_pid,
                    kernel_handle,
                    Box::pin(async move { Ok(ActiveExecution::Binding(binding_execution)) })
                        as OwnedChildExecutionStart,
                    None,
                    0,
                    false,
                )
            } else {
                let kernel_command = match resolved.runtime {
                    GuestRuntimeKind::JavaScript => JAVASCRIPT_COMMAND,
                    GuestRuntimeKind::WebAssembly => WASM_COMMAND,
                    GuestRuntimeKind::Python => PYTHON_COMMAND,
                };
                let kernel_handle = vm
                    .kernel
                    .spawn_process_with_process_group(
                        kernel_command,
                        resolved.process_args.clone(),
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            parent_pid: Some(parent_kernel_pid),
                            env: resolved.env.clone(),
                            cwd: Some(resolved.guest_cwd.clone()),
                        },
                        requested_pgid,
                    )
                    .map_err(kernel_error)?;
                let kernel_pid = kernel_handle.pid();
                let applied_spawn_actions = if let Some(prepared) = prepared_spawn_actions {
                    install_preapplied_posix_spawn_file_actions(
                        &mut vm.kernel,
                        &kernel_handle,
                        prepared,
                    )?
                } else {
                    apply_posix_spawn_file_actions_or_rollback(
                        &mut vm.kernel,
                        &kernel_handle,
                        &resolved.guest_cwd,
                        &request.options.spawn_fd_mappings,
                        &prepared_host_net_fds.kernel_actions,
                    )?
                };
                let posix_spawn_controls_stdin = !request.options.spawn_file_actions.is_empty()
                    && (applied_spawn_actions
                        .fd_mappings
                        .iter()
                        .any(|mapping| mapping[0] == 0)
                        || applied_spawn_actions.closed_guest_fds.contains(&0)
                        || prepared_host_net_fds
                            .descriptions
                            .iter()
                            .any(|description| description.guest_fds.contains(&0)));
                if matches!(
                    resolved.runtime,
                    GuestRuntimeKind::JavaScript | GuestRuntimeKind::Python
                ) {
                    materialize_direct_runtime_stdio_mappings(
                        &mut vm.kernel,
                        kernel_pid,
                        &applied_spawn_actions,
                    )?;
                }
                let kernel_stdin_reader_fd = if resolved.runtime != GuestRuntimeKind::WebAssembly {
                    canonicalize_host_runtime_posix_stdin(
                        &mut vm.kernel,
                        kernel_pid,
                        &applied_spawn_actions,
                    )?
                } else {
                    0
                };
                apply_spawn_session_or_rollback(
                    &mut vm.kernel,
                    &kernel_handle,
                    spawn_attributes.new_session || request.options.detached,
                )?;
                pending_kernel_handle = Some(kernel_handle.clone());
                let mut execution_env = resolved.env.clone();
                if resolved.runtime == GuestRuntimeKind::JavaScript
                    && (posix_spawn_controls_stdin
                        || javascript_child_process_stdin_mode(&request) != "pipe")
                {
                    execution_env.insert(
                        String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
                        String::from("1"),
                    );
                }
                if resolved.runtime == GuestRuntimeKind::WebAssembly {
                    execution_env.insert(
                        String::from("AGENTOS_WASM_INHERITED_FD_MAPPINGS"),
                        serde_json::to_string(&applied_spawn_actions.fd_mappings).map_err(
                            |error| {
                                SidecarError::InvalidState(format!(
                                    "failed to serialize inherited WASM fd mappings: {error}"
                                ))
                            },
                        )?,
                    );
                    execution_env.insert(
                        String::from("AGENTOS_WASM_CLOSED_INHERITED_FDS"),
                        serde_json::to_string(&applied_spawn_actions.closed_guest_fds).map_err(
                            |error| {
                                SidecarError::InvalidState(format!(
                                    "failed to serialize closed inherited WASM fds: {error}"
                                ))
                            },
                        )?,
                    );
                    execution_env.insert(
                        String::from("AGENTOS_WASM_INHERITED_HOSTNET_FDS"),
                        serde_json::to_string(&prepared_host_net_fds.bootstrap_json()).map_err(
                            |error| {
                                SidecarError::InvalidState(format!(
                                    "failed to serialize inherited WASM host-network fds: {error}"
                                ))
                            },
                        )?,
                    );
                }
                execution_env.insert(
                    String::from(EXECUTION_SANDBOX_ROOT_ENV),
                    normalize_host_path(&vm.cwd).to_string_lossy().into_owned(),
                );
                let execution_start: OwnedChildExecutionStart = match resolved.runtime {
                    GuestRuntimeKind::JavaScript => {
                        let mut javascript_engine = execution_engines
                            .javascript("start nested JavaScript child process")?;
                        execution_env.extend(
                            sanitize_javascript_child_process_internal_bootstrap_env(
                                &request.options.internal_bootstrap_env,
                            ),
                        );
                        execution_env.remove("AGENTOS_EAGER_STDIN_HANDLE");
                        execution_env
                            .insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
                        if posix_spawn_controls_stdin {
                            execution_env.insert(
                                String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
                                String::from("1"),
                            );
                        } else {
                            execution_env.remove("AGENTOS_FORWARD_KERNEL_STDIN_RPC");
                        }
                        let launch_entrypoint =
                            resolve_agentos_package_javascript_launch_entrypoint(
                                &mut vm,
                                &mut execution_env,
                            )
                            .unwrap_or_else(|| resolved.entrypoint.clone());
                        let inline_code = load_javascript_entrypoint_source(
                            &mut vm,
                            &resolved.host_cwd,
                            &launch_entrypoint,
                            &execution_env,
                        );
                        prepare_javascript_shadow(&mut vm, &resolved, &execution_env)?;

                        let built_reader = build_module_reader(&mut vm, &resolved);
                        let guest_reader = built_reader.clone().map(|reader| {
                            Box::new(crate::plugins::host_dir::SessionModuleReader::new(reader))
                                as Box<dyn GuestModuleReader>
                        });
                        let module_reader = built_reader
                            .map(|reader| Box::new(reader) as Box<dyn ModuleFsReader + Send>);
                        let context =
                            javascript_engine.create_context(CreateJavascriptContextRequest {
                                vm_id: vm_id.to_owned(),
                                bootstrap_module: None,
                                compile_cache_root: Some(cache_root.join("node-compile-cache")),
                            });
                        let context_id = context.context_id;
                        let execution_result = javascript_engine
                            .start_execution_with_module_reader_and_runtime(
                                StartJavascriptExecutionRequest {
                                    guest_runtime: guest_runtime_identity(
                                        &mut vm,
                                        Some(u64::from(kernel_pid)),
                                        Some(u64::from(parent_kernel_pid)),
                                    ),
                                    vm_id: vm_id.to_owned(),
                                    context_id: context_id.clone(),
                                    argv: std::iter::once(launch_entrypoint)
                                        .chain(resolved.execution_args.clone())
                                        .collect(),
                                    argv0: request.options.argv0.clone(),
                                    env: execution_env,
                                    cwd: resolved.host_cwd.clone(),
                                    limits: javascript_execution_limits(&mut vm),
                                    inline_code,
                                    wasm_module_bytes: None,
                                },
                                module_reader,
                                guest_reader,
                                vm.runtime_context.clone(),
                            );
                        javascript_engine.dispose_context(&context_id);
                        let execution = execution_result.map_err(javascript_error)?;
                        Box::pin(async move { Ok(ActiveExecution::Javascript(execution)) })
                    }
                    GuestRuntimeKind::WebAssembly => {
                        execution_env.extend(
                            sanitize_javascript_child_process_internal_bootstrap_env(
                                &request.options.internal_bootstrap_env,
                            ),
                        );
                        execution_env
                            .insert(String::from(WASM_STDIO_SYNC_RPC_ENV), String::from("1"));
                        execution_env
                            .insert(String::from(WASM_EXEC_COMMIT_RPC_ENV), String::from("1"));
                        let wasm_limits = wasm_execution_limits(&mut vm);
                        let wasm_guest_runtime = guest_runtime_identity(
                            &mut vm,
                            Some(u64::from(kernel_pid)),
                            Some(u64::from(parent_kernel_pid)),
                        );
                        let start_request = StartWasmExecutionRequest {
                            vm_id: vm_id.to_owned(),
                            context_id: String::new(),
                            argv: resolved.process_args.clone(),
                            env: execution_env,
                            cwd: resolved.host_cwd.clone(),
                            permission_tier: execution_wasm_permission_tier(
                                resolved
                                    .wasm_permission_tier
                                    .unwrap_or(WasmPermissionTier::Full),
                            ),
                            limits: wasm_limits,
                            guest_runtime: wasm_guest_runtime,
                        };
                        let module_path = resolved.entrypoint.clone();
                        let runtime_context = vm.runtime_context.clone();
                        let execution_engines = execution_engines.clone();
                        Box::pin(async move {
                            let mut wasm_engine =
                                execution_engines.wasm("start nested WebAssembly child process")?;
                            let context = wasm_engine.create_context(CreateWasmContextRequest {
                                vm_id: start_request.vm_id.clone(),
                                module_path: Some(module_path),
                            });
                            let context_id = context.context_id;
                            let execution_result = wasm_engine
                                .start_execution_with_runtime_async(
                                    StartWasmExecutionRequest {
                                        context_id: context_id.clone(),
                                        ..start_request
                                    },
                                    runtime_context,
                                )
                                .await;
                            wasm_engine.dispose_context(&context_id);
                            let execution = execution_result.map_err(wasm_error)?;
                            Ok(ActiveExecution::Wasm(Box::new(execution)))
                        })
                    }
                    GuestRuntimeKind::Python => {
                        // Nested `python` child_process: set up the Pyodide context the
                        // same way the top-level execute path does, so a guest shell or
                        // node parent can spawn `python` exactly like `node`.
                        let python_file_path = if execution_env.contains_key("AGENTOS_PYTHON_ARGV")
                        {
                            execution_env.get("AGENTOS_PYTHON_FILE").map(PathBuf::from)
                        } else {
                            python_file_entrypoint(&resolved.entrypoint)
                        };
                        let python_limits = python_execution_limits(&mut vm);
                        let python_guest_runtime = guest_runtime_identity(
                            &mut vm,
                            Some(u64::from(kernel_pid)),
                            Some(u64::from(parent_kernel_pid)),
                        );
                        let runtime_context = vm.runtime_context.clone();
                        let execution_engines = execution_engines.clone();
                        let start_vm_id = vm_id.to_owned();
                        let code = resolved.entrypoint.clone();
                        let cwd = resolved.host_cwd.clone();
                        Box::pin(async move {
                            let mut python_engine =
                                execution_engines.python("start nested Python child process")?;
                            let pyodide_dist_path = python_engine
                                .bundled_pyodide_dist_path_for_vm_async(
                                    &start_vm_id,
                                    &runtime_context,
                                )
                                .await
                                .map_err(python_error)?;
                            let pyodide_cache_path = pyodide_dist_path
                                .parent()
                                .and_then(Path::parent)
                                .unwrap_or(pyodide_dist_path.as_path())
                                .join("pyodide-package-cache");
                            add_runtime_guest_path_mapping(
                                &mut execution_env,
                                PYTHON_PYODIDE_GUEST_ROOT,
                                &pyodide_dist_path,
                            );
                            add_runtime_guest_path_mapping(
                                &mut execution_env,
                                PYTHON_PYODIDE_CACHE_GUEST_ROOT,
                                &pyodide_cache_path,
                            );
                            add_runtime_host_access_path(
                                &mut execution_env,
                                "AGENTOS_EXTRA_FS_READ_PATHS",
                                &pyodide_dist_path,
                                true,
                            );
                            add_runtime_host_access_path(
                                &mut execution_env,
                                "AGENTOS_EXTRA_FS_READ_PATHS",
                                &pyodide_cache_path,
                                true,
                            );
                            add_runtime_host_access_path(
                                &mut execution_env,
                                "AGENTOS_EXTRA_FS_WRITE_PATHS",
                                &pyodide_cache_path,
                                false,
                            );
                            let context =
                                python_engine.create_context(CreatePythonContextRequest {
                                    vm_id: start_vm_id.clone(),
                                    pyodide_dist_path,
                                });
                            let context_id = context.context_id;
                            let execution_result = python_engine
                                .start_execution_with_runtime_async(
                                    StartPythonExecutionRequest {
                                        vm_id: start_vm_id,
                                        context_id: context_id.clone(),
                                        code,
                                        file_path: python_file_path,
                                        env: execution_env,
                                        cwd,
                                        limits: python_limits,
                                        guest_runtime: python_guest_runtime,
                                    },
                                    runtime_context,
                                )
                                .await;
                            python_engine.dispose_context(&context_id);
                            let execution = execution_result.map_err(python_error)?;
                            Ok(ActiveExecution::Python(execution))
                        })
                    }
                };
                let kernel_stdin_writer_fd = if posix_spawn_controls_stdin {
                    None
                } else {
                    match javascript_child_process_stdin_mode(&request) {
                        "pipe" => Some(install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid)?),
                        "ignore" => {
                            vm.kernel
                                .fd_close(EXECUTION_DRIVER_NAME, kernel_pid, 0)
                                .map_err(kernel_error)?;
                            None
                        }
                        "inherit" => None,
                        _ => Some(install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid)?),
                    }
                };
                (
                    kernel_pid,
                    kernel_handle,
                    execution_start,
                    kernel_stdin_writer_fd,
                    kernel_stdin_reader_fd,
                    posix_spawn_controls_stdin,
                )
            };
            let (
                kernel_pid,
                kernel_handle,
                execution_start,
                kernel_stdin_writer_fd,
                kernel_stdin_reader_fd,
                direct_posix_stdin,
            ) = spawned;
            let mut pending_spawn =
                PendingOwnedChildSpawn::new(vms.handle.clone(), kernel_handle.clone());
            pending_kernel_handle.take();
            drop(vm);
            let execution = execution_start.await?;
            pending_spawn.set_execution(execution);
            let mut vm = vms.get_mut(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let mut execution = pending_spawn.take_execution();
            record_execute_phase(
                "child_process_spawn_and_start_execution",
                phase_start.elapsed(),
            );

            let phase_start = Instant::now();
            let child_fd1_is_tty = vm
                .kernel
                .isatty(EXECUTION_DRIVER_NAME, kernel_pid, 1)
                .unwrap_or(false);
            let child_process_group = match vm.kernel.getpgid(EXECUTION_DRIVER_NAME, kernel_pid) {
                Ok(process_group) => process_group,
                Err(error) => {
                    if let Some(process) = pending_kernel_handle.take() {
                        rollback_unregistered_spawn_child(
                            &mut vm.kernel,
                            &process,
                            Some(&mut execution),
                            "nested child_process.spawn",
                        );
                    }
                    return Err(kernel_error(error));
                }
            };
            let process_event_limits = vm.limits.process.clone();
            let shadow_root = normalize_host_path(&vm.cwd);
            let root = match vm.active_processes.get_mut(process_id) {
                Some(root) => root,
                None => {
                    let error = missing_process_error(vm_id, process_id);
                    if let Some(child) = pending_kernel_handle.take() {
                        rollback_unregistered_spawn_child(
                            &mut vm.kernel,
                            &child,
                            Some(&mut execution),
                            "nested child_process.spawn",
                        );
                    }
                    return Err(error);
                }
            };
            let parent =
                match NativeSidecar::<B>::active_process_by_path_mut(root, current_process_path) {
                    Some(parent) => parent,
                    None => {
                        let error = SidecarError::InvalidState(format!(
                    "unknown child process path {current_process_label} during nested spawn"
                ));
                        if let Some(child) = pending_kernel_handle.take() {
                            rollback_unregistered_spawn_child(
                                &mut vm.kernel,
                                &child,
                                Some(&mut execution),
                                "nested child_process.spawn",
                            );
                        }
                        return Err(error);
                    }
                };
            let inherited_tty_master_owner = if child_fd1_is_tty {
                parent
                    .tty_master_fd
                    .map(|master_fd| (parent.kernel_pid, master_fd))
                    .or(parent.tty_master_owner)
            } else {
                None
            };
            pending_kernel_handle.take();
            parent.child_processes.insert(
                child_process_id.clone(),
                ActiveProcess::new(
                    kernel_pid,
                    kernel_handle,
                    parent.runtime_context.clone(),
                    parent.limits.clone(),
                    process_event_capacity,
                    resolved.runtime,
                    execution,
                )
                .with_event_notify(Arc::clone(&process_event_notify))
                .with_process_event_limits(&process_event_limits)
                .with_vm_pending_byte_budgets(
                    Arc::clone(&vm_pending_stdin_bytes_budget),
                    Arc::clone(&vm_pending_event_bytes_budget),
                )
                .with_detached(request.options.detached)
                .with_guest_cwd(resolved.guest_cwd.clone())
                .with_env(resolved.env.clone())
                .with_shadow_root(shadow_root)
                .with_host_cwd(resolved.host_cwd.clone()),
            );
            pending_spawn.disarm();
            {
                let child = parent
                    .child_processes
                    .get_mut(&child_process_id)
                    .expect("inserted nested child exists during spawn registration");
                child.tty_master_owner = inherited_tty_master_owner;
                child.direct_posix_stdin = direct_posix_stdin;
                child.kernel_stdin_reader_fd = kernel_stdin_reader_fd;
                if let Some(kernel_stdin_writer_fd) = kernel_stdin_writer_fd {
                    child.kernel_stdin_writer_fd = Some(kernel_stdin_writer_fd);
                }
                prepared_host_net_fds.install(child);
            }
            record_execute_phase("child_process_register", phase_start.elapsed());
            record_execute_phase("child_process_spawn_total", total_start.elapsed());
            Ok(json!({
                "childId": child_process_id,
                "pid": kernel_pid,
                "pgid": child_process_group,
                "directPosixStdin": direct_posix_stdin,
                "command": resolved.command,
                "args": resolved.process_args,
            }))
        })
    }

    fn prepare_owned_descendant_javascript_child_process_sync(
        &self,
        vm: VmHandle,
        vm_id: String,
        process_id: String,
        current_process_path: Vec<String>,
        request: JavascriptChildProcessSpawnRequest,
        max_buffer: Option<usize>,
    ) -> Pin<
        Box<dyn Future<Output = Result<JavascriptSyncRpcServiceResponse, SidecarError>> + 'static>,
    > {
        let sync_input_value = request.options.input.clone();
        let timeout_ms = request.options.timeout;
        let kill_signal = request.options.kill_signal.clone();
        let path = current_process_path
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let spawn =
            self.spawn_descendant_javascript_child_process(&vm_id, &process_id, &path, request);

        Box::pin(async move {
            let (max_buffer, runtime, notify) =
                vm.try_command("prepare owned JavaScript spawnSync", |vm| {
                    let root = vm
                        .active_processes
                        .get(&process_id)
                        .ok_or_else(|| missing_process_error(&vm_id, &process_id))?;
                    let path = current_process_path
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>();
                    let parent = Self::active_process_by_path(root, &path).ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "unknown process path {} during owned spawnSync",
                            Self::child_process_path_label(&process_id, &path)
                        ))
                    })?;
                    Ok((
                        Self::child_process_sync_max_buffer(parent, max_buffer)?,
                        parent.runtime_context.clone(),
                        Arc::clone(&parent.process_event_notify),
                    ))
                })?;
            let sync_input = javascript_child_process_sync_input_bytes(sync_input_value.as_ref())?;
            let deadline =
                timeout_ms.map(|timeout_ms| Instant::now() + Duration::from_millis(timeout_ms));
            let timeout_signal = kill_signal.unwrap_or_else(|| String::from("SIGTERM"));
            let spawned = spawn.await?;
            let child_process_id = spawned
                .get("childId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "owned child_process.spawn_sync response is missing childId",
                    ))
                })?
                .to_owned();
            let pid = spawned
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "owned child_process.spawn_sync response is missing a valid pid",
                    ))
                })?;
            let mut pending_child = PendingOwnedRegisteredChild::new(
                vm.clone(),
                process_id.clone(),
                current_process_path.clone(),
                child_process_id.clone(),
            );

            let response = vm
                .try_command("initialize owned JavaScript spawnSync child", |vm| {
                    let VmState {
                        kernel,
                        active_processes,
                        ..
                    } = vm;
                    let root = active_processes
                        .get_mut(&process_id)
                        .ok_or_else(|| missing_process_error(&vm_id, &process_id))?;
                    let path = current_process_path
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>();
                    let parent =
                        Self::active_process_by_path_mut(root, &path).ok_or_else(|| {
                            SidecarError::InvalidState(format!(
                                "unknown process path {} during owned spawnSync publish",
                                Self::child_process_path_label(&process_id, &path)
                            ))
                        })?;
                    let child = parent
                        .child_processes
                        .get_mut(&child_process_id)
                        .ok_or_else(|| {
                            javascript_child_process_gone_error(
                                &process_id,
                                &[child_process_id.as_str()],
                            )
                        })?;
                    if let Some(input) = sync_input.as_deref() {
                        if let Err(error) = child.execution.write_stdin(input) {
                            if !is_broken_pipe_error(&error) {
                                return Err(error);
                            }
                        }
                        write_kernel_process_stdin(kernel, child, input)?;
                    }
                    child.execution.close_stdin()?;
                    close_kernel_process_stdin(kernel, child)?;
                    let (respond_to, receiver) = tokio::sync::oneshot::channel();
                    parent.pending_child_process_sync.insert(
                        child_process_id.clone(),
                        PendingChildProcessSync {
                            pid,
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                            max_buffer,
                            deadline,
                            timeout_signal,
                            kill_sent: false,
                            timed_out: false,
                            max_buffer_exceeded: false,
                            completion: PendingChildProcessSyncCompletion::Javascript(respond_to),
                        },
                    );
                    Ok(receiver)
                })
                .and_then(|receiver| {
                    if let Some(deadline) = deadline {
                        let delay = deadline.saturating_duration_since(Instant::now());
                        runtime
                            .spawn(agentos_runtime::TaskClass::Timer, async move {
                                tokio::time::sleep(delay).await;
                                notify.notify_one();
                            })
                            .map_err(SidecarError::from)?;
                    }
                    Ok(JavascriptSyncRpcServiceResponse::Deferred {
                        receiver,
                        timeout: None,
                        task_class: agentos_runtime::TaskClass::Vm,
                    })
                });
            if response.is_ok() {
                pending_child.disarm();
            }
            response
        })
    }

    /// Prepare the path-aware service for a claimed JavaScript process RPC.
    /// Root and descendant targets share this dispatcher; an empty path means
    /// the root process. Preparation only parses or clones bounded inputs.
    /// Every process mutation happens after the owned future is polled, so the
    /// protocol engine never performs guest work or retains `NativeSidecar`
    /// while the operation is pending.
    pub(crate) fn prepare_owned_javascript_process_event_service(
        &mut self,
        target: OwnedJavascriptEventService,
    ) -> Pin<Box<dyn Future<Output = Result<(), SidecarError>> + 'static>> {
        let bridge = self.bridge.clone();
        match target.request.method.as_str() {
            "child_process.spawn" => {
                let payload = target
                    .vm
                    .try_read("prepare owned child_process.spawn", |vm| {
                        parse_javascript_child_process_spawn_request(vm, &target.request.args)
                            .map(|parsed| parsed.0)
                    });
                let spawn = match payload {
                    Ok(Ok(payload)) => {
                        let path = target
                            .child_path
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>();
                        Some(self.spawn_descendant_javascript_child_process(
                            &target.vm_id,
                            &target.process_id,
                            &path,
                            payload,
                        ))
                    }
                    Ok(Err(error)) | Err(error) => {
                        return settle_owned_javascript_process_event_target::<B>(
                            target,
                            Err(error),
                        )
                    }
                };
                let vm = target.vm.clone();
                let process_id = target.process_id.clone();
                let child_path = target.child_path.clone();
                let request = target.request.clone();
                return Box::pin(async move {
                    let response = spawn
                        .expect("owned spawn future prepared")
                        .await
                        .map(Into::into);
                    let result = settle_owned_fetch_sync_rpc::<B>(
                        &vm,
                        &process_id,
                        &child_path,
                        request,
                        response,
                    )
                    .await;
                    drop(target);
                    result
                });
            }
            "child_process.spawn_sync" => {
                let payload = target
                    .vm
                    .try_read("prepare owned child_process.spawn_sync", |vm| {
                        parse_javascript_child_process_spawn_request(vm, &target.request.args)
                    });
                let (payload, max_buffer) = match payload {
                    Ok(Ok(payload)) => payload,
                    Ok(Err(error)) | Err(error) => {
                        return settle_owned_javascript_process_event_target::<B>(
                            target,
                            Err(error),
                        )
                    }
                };
                let service = self.prepare_owned_descendant_javascript_child_process_sync(
                    target.vm.clone(),
                    target.vm_id.clone(),
                    target.process_id.clone(),
                    target.child_path.clone(),
                    payload,
                    max_buffer,
                );
                let vm = target.vm.clone();
                let process_id = target.process_id.clone();
                let child_path = target.child_path.clone();
                let request = target.request.clone();
                return Box::pin(async move {
                    let response = service.await;
                    let result = settle_owned_fetch_sync_rpc::<B>(
                        &vm,
                        &process_id,
                        &child_path,
                        request,
                        response,
                    )
                    .await;
                    drop(target);
                    result
                });
            }
            _ => {}
        }

        let special = matches!(
            target.request.method.as_str(),
            "child_process.poll"
                | "child_process.write_stdin"
                | "child_process.close_stdin"
                | "child_process.kill"
                | "process.exec_fd_image_commit"
                | "process.exec"
                | "process.signal_state"
                | "process.kill"
        );
        if !special {
            return Box::pin(async move {
                let result = service_owned_javascript_sync_rpc_request(
                    &bridge,
                    &target.vm_id,
                    &target.vm,
                    &target.process_id,
                    &target.child_path,
                    target.request.clone(),
                )
                .await;
                drop(target);
                result
            });
        }

        let cache_root = self.cache_root.clone();
        let process_event_notify = Arc::clone(&self.process_event_notify);
        #[cfg(test)]
        let fail_after_commit = target.request.method == "process.exec"
            && std::mem::take(&mut self.fail_next_exec_start_after_commit);
        #[cfg(not(test))]
        let fail_after_commit = false;
        Box::pin(async move {
            let path = target
                .child_path
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let owned_vm = OwnedChildProcessVm {
                vm_id: target.vm_id.clone(),
                handle: target.vm.clone(),
            };
            let mut omit_exec_response = false;
            let response: Result<JavascriptSyncRpcServiceResponse, SidecarError> = match target
                .request
                .method
                .as_str()
            {
                "child_process.poll" => {
                    let child_process_id = javascript_sync_rpc_arg_str(
                        &target.request.args,
                        0,
                        "child_process.poll child id",
                    );
                    match child_process_id {
                        Ok(child_process_id) => {
                            let wait_ms = javascript_sync_rpc_arg_u64_optional(
                                &target.request.args,
                                1,
                                "child_process.poll wait ms",
                            )
                            .map(|wait| wait.unwrap_or_default());
                            match wait_ms {
                                Ok(wait_ms) => {
                                    Self::poll_owned_descendant_javascript_child_process(
                                        &bridge,
                                        &target.vm,
                                        &target.vm_id,
                                        &target.process_id,
                                        &target.child_path,
                                        child_process_id,
                                        wait_ms,
                                        Arc::clone(&process_event_notify),
                                    )
                                    .await
                                    .map(Into::into)
                                }
                                Err(error) => Err(error),
                            }
                        }
                        Err(error) => Err(error),
                    }
                }
                "child_process.write_stdin" => javascript_sync_rpc_arg_str(
                    &target.request.args,
                    0,
                    "child_process.write_stdin child id",
                )
                .and_then(|child_process_id| {
                    javascript_sync_rpc_bytes_arg(
                        &target.request.args,
                        1,
                        "child_process.write_stdin chunk",
                    )
                    .and_then(|chunk| {
                        Self::write_descendant_javascript_child_process_stdin_owned(
                            &target.vm,
                            &target.process_id,
                            &target.child_path,
                            child_process_id,
                            &chunk,
                        )
                    })
                })
                .map(|()| Value::Null.into()),
                "child_process.close_stdin" => javascript_sync_rpc_arg_str(
                    &target.request.args,
                    0,
                    "child_process.close_stdin child id",
                )
                .and_then(|child_process_id| {
                    Self::close_descendant_javascript_child_process_stdin_owned(
                        &target.vm,
                        &target.process_id,
                        &target.child_path,
                        child_process_id,
                    )
                })
                .map(|()| Value::Null.into()),
                "child_process.kill" => javascript_sync_rpc_arg_str(
                    &target.request.args,
                    0,
                    "child_process.kill child id",
                )
                .and_then(|child_process_id| {
                    let signal = javascript_sync_rpc_arg_str(
                        &target.request.args,
                        1,
                        "child_process.kill signal",
                    )?;
                    Self::kill_descendant_javascript_child_process_owned(
                        &bridge,
                        &target.vm,
                        &target.vm_id,
                        &target.process_id,
                        &target.child_path,
                        child_process_id,
                        signal,
                        &process_event_notify,
                    )
                })
                .map(|()| Value::Null.into()),
                "process.exec_fd_image_commit" => target
                    .vm
                    .try_read("parse owned fd image commit", |vm| {
                        parse_javascript_child_process_spawn_request(vm, &target.request.args)
                            .map(|parsed| parsed.0)
                    })
                    .and_then(|payload| payload)
                    .and_then(|payload| {
                        Self::commit_wasm_fd_process_image_owned(
                            &owned_vm,
                            &target.vm_id,
                            &target.process_id,
                            &path,
                            payload,
                        )
                    })
                    .map(|()| json!({ "committed": true }).into()),
                "process.exec" => {
                    let parsed = target.vm.try_read("parse owned process exec", |vm| {
                        parse_javascript_child_process_spawn_request(vm, &target.request.args)
                            .map(|parsed| parsed.0)
                    });
                    match parsed.and_then(|payload| payload) {
                        Ok(payload) => {
                            let local_replacement = payload.options.local_replacement;
                            match Self::exec_javascript_process_image_owned(
                                &owned_vm,
                                &cache_root,
                                fail_after_commit,
                                &target.vm_id,
                                &target.process_id,
                                &path,
                                payload,
                            ) {
                                Ok(()) => {
                                    omit_exec_response = !local_replacement;
                                    Ok(if local_replacement {
                                        json!({ "committed": true }).into()
                                    } else {
                                        Value::Null.into()
                                    })
                                }
                                Err(error) => Err(error),
                            }
                        }
                        Err(error) => Err(error),
                    }
                }
                "process.signal_state" => parse_process_signal_state_request(&target.request.args)
                    .map_err(|error| SidecarError::InvalidState(error.to_string()))
                    .and_then(|(signal, registration)| {
                        target
                            .vm
                            .try_command("apply owned process signal state", |vm| {
                                let signal_key =
                                    Self::child_process_signal_key(&target.process_id, &path)
                                        .to_owned();
                                apply_process_signal_state_update(
                                    &mut vm.signal_states,
                                    &signal_key,
                                    signal,
                                    registration,
                                );
                                Ok(())
                            })
                    })
                    .map(|()| Value::Null.into()),
                "process.kill" => Self::handle_owned_process_kill_rpc(
                    &bridge,
                    &target.vm,
                    &target.vm_id,
                    &target.process_id,
                    &target.child_path,
                    &target.request,
                )
                .map(Into::into),
                _ => unreachable!("special JavaScript process RPC was classified above"),
            };

            // A successful non-local exec destroys the requesting image. It is
            // the one path where replying would target code that no longer
            // exists, so completion is intentionally response-free.
            if omit_exec_response && response.is_ok() {
                drop(target);
                return Ok(());
            }
            let result = settle_owned_fetch_sync_rpc::<B>(
                &target.vm,
                &target.process_id,
                &target.child_path,
                target.request.clone(),
                response,
            )
            .await;
            drop(target);
            result
        })
    }

    async fn poll_owned_descendant_javascript_child_process(
        bridge: &SharedBridge<B>,
        vm: &VmHandle,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[String],
        child_process_id: &str,
        wait_ms: u64,
        process_event_notify: Arc<tokio::sync::Notify>,
    ) -> Result<Value, SidecarError> {
        // Polling is readiness-driven by the process supervisor. The legacy
        // wait argument remains wire-compatible but must never park this
        // request or retain VM state.
        let _ = wait_ms;
        let mut child_path = current_process_path.to_vec();
        child_path.push(child_process_id.to_owned());
        loop {
            let polled = vm.try_command("poll owned descendant process", |state| {
                let path = current_process_path
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                let root = state
                    .active_processes
                    .get_mut(process_id)
                    .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                let parent = Self::active_process_by_path_mut(root, &path).ok_or_else(|| {
                    javascript_child_process_gone_error(
                        process_id,
                        &child_path.iter().map(String::as_str).collect::<Vec<_>>(),
                    )
                })?;
                let child = parent
                    .child_processes
                    .get_mut(child_process_id)
                    .ok_or_else(|| {
                        javascript_child_process_gone_error(
                            process_id,
                            &child_path.iter().map(String::as_str).collect::<Vec<_>>(),
                        )
                    })?;
                if let Some(event) = child.lease_pending_execution_event() {
                    return Ok(Some(event));
                }
                match child.try_poll_execution_event() {
                    Ok(event) => Ok(event),
                    Err(SidecarError::Execution(message))
                        if (child.runtime == GuestRuntimeKind::JavaScript
                            && closed_javascript_event_channel(&message))
                            || (child.runtime == GuestRuntimeKind::Python
                                && closed_python_event_channel(&message))
                            || (child.runtime == GuestRuntimeKind::WebAssembly
                                && closed_wasm_event_channel(&message)) =>
                    {
                        Ok(None)
                    }
                    Err(error) => Err(error),
                }
            })?;
            let Some(PolledExecutionEvent { event, reservation }) = polled else {
                return Ok(Value::Null);
            };
            match event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    return Ok(json!({
                        "type": "stdout",
                        "data": javascript_sync_rpc_bytes_value(&chunk),
                    }));
                }
                ActiveExecutionEvent::Stderr(chunk)
                    if chunk.as_slice() == SYNTHETIC_V8_TERMINATION_STDERR =>
                {
                    drop(reservation);
                    continue;
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    return Ok(json!({
                        "type": "stderr",
                        "data": javascript_sync_rpc_bytes_value(&chunk),
                    }));
                }
                ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
                    if request.method == "process.signal_state" {
                        let response = parse_process_signal_state_request(&request.args)
                            .map_err(|error| SidecarError::InvalidState(error.to_string()))
                            .and_then(|(signal, registration)| {
                                vm.try_command("apply nested owned process signal state", |state| {
                                    let path =
                                        child_path.iter().map(String::as_str).collect::<Vec<_>>();
                                    let signal_key =
                                        Self::child_process_signal_key(process_id, &path)
                                            .to_owned();
                                    apply_process_signal_state_update(
                                        &mut state.signal_states,
                                        &signal_key,
                                        signal,
                                        registration,
                                    );
                                    Ok(())
                                })
                            })
                            .map(|()| Value::Null.into());
                        settle_owned_fetch_sync_rpc::<B>(
                            vm,
                            process_id,
                            &child_path,
                            request,
                            response,
                        )
                        .await?;
                        drop(reservation);
                        continue;
                    }
                    if matches!(
                        request.method.as_str(),
                        "child_process.spawn"
                            | "child_process.spawn_sync"
                            | "child_process.poll"
                            | "child_process.write_stdin"
                            | "child_process.close_stdin"
                            | "child_process.kill"
                            | "process.exec_fd_image_commit"
                            | "process.exec"
                            | "process.signal_state"
                            | "process.kill"
                    ) {
                        vm.try_command("requeue nested special process RPC", |state| {
                            let path = current_process_path
                                .iter()
                                .map(String::as_str)
                                .collect::<Vec<_>>();
                            let root = state
                                .active_processes
                                .get_mut(process_id)
                                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                            let parent =
                                Self::active_process_by_path_mut(root, &path).ok_or_else(|| {
                                    javascript_child_process_gone_error(
                                        process_id,
                                        &child_path.iter().map(String::as_str).collect::<Vec<_>>(),
                                    )
                                })?;
                            let child = parent
                                .child_processes
                                .get_mut(child_process_id)
                                .ok_or_else(|| {
                                    javascript_child_process_gone_error(
                                        process_id,
                                        &child_path.iter().map(String::as_str).collect::<Vec<_>>(),
                                    )
                                })?;
                            child.queue_pending_polled_execution_event(PolledExecutionEvent {
                                event: ActiveExecutionEvent::JavascriptSyncRpcRequest(request),
                                reservation,
                            })
                        })?;
                        process_event_notify.notify_one();
                        return Ok(Value::Null);
                    }
                    service_owned_javascript_sync_rpc_request(
                        bridge,
                        vm_id,
                        vm,
                        process_id,
                        &child_path,
                        request,
                    )
                    .await?;
                    drop(reservation);
                }
                ActiveExecutionEvent::Exited(exit_code) => {
                    let (signal_name, core_dumped) =
                        vm.try_command("finish owned descendant process exit", |state| {
                            let path = current_process_path
                                .iter()
                                .map(String::as_str)
                                .collect::<Vec<_>>();
                            let parent = state
                                .active_processes
                                .get_mut(process_id)
                                .and_then(|root| Self::active_process_by_path_mut(root, &path));
                            let Some(parent) = parent else {
                                return Ok((None, false));
                            };
                            let Some(mut child) = parent.child_processes.remove(child_process_id)
                            else {
                                return Ok((None, false));
                            };
                            let signal_name = child
                                .exit_signal
                                .take()
                                .and_then(canonical_signal_name)
                                .map(str::to_owned);
                            let core_dumped = child.exit_core_dumped;
                            let label = Self::child_process_path_label(
                                process_id,
                                &child_path.iter().map(String::as_str).collect::<Vec<_>>(),
                            );
                            let detached = Self::adopt_detached_child_processes(&label, &mut child);
                            if child.host_write_dirty_recursive()
                                || !child.clean_host_writes_are_observable_recursive()
                            {
                                sync_process_host_writes_to_kernel(state, &child)?;
                            }
                            release_inherited_child_raw_mode(&mut state.kernel, &child)?;
                            terminate_child_process_tree(
                                &mut state.kernel,
                                &mut child,
                                &state.kernel_socket_readiness,
                                &state.unix_address_registry,
                            );
                            child.kernel_handle.finish(exit_code);
                            let _ = state.kernel.wait_and_reap(child.kernel_pid);
                            state.signal_states.remove(child_process_id);
                            for (detached_id, detached_child) in detached {
                                state.detached_child_processes.insert(detached_id.clone());
                                state.active_processes.insert(detached_id, detached_child);
                            }
                            Ok((signal_name, core_dumped))
                        })?;
                    let mut payload = Map::new();
                    payload.insert(String::from("type"), Value::String(String::from("exit")));
                    payload.insert(String::from("exitCode"), Value::from(exit_code));
                    payload.insert(String::from("coreDumped"), Value::from(core_dumped));
                    if let Some(signal_name) = signal_name {
                        payload.insert(String::from("signal"), Value::String(signal_name));
                    }
                    drop(reservation);
                    return Ok(Value::Object(payload));
                }
                other => {
                    vm.try_command("requeue supervisor-owned child event", |state| {
                        let path = current_process_path
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>();
                        let root = state
                            .active_processes
                            .get_mut(process_id)
                            .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                        let parent =
                            Self::active_process_by_path_mut(root, &path).ok_or_else(|| {
                                javascript_child_process_gone_error(
                                    process_id,
                                    &child_path.iter().map(String::as_str).collect::<Vec<_>>(),
                                )
                            })?;
                        let child = parent
                            .child_processes
                            .get_mut(child_process_id)
                            .ok_or_else(|| {
                                javascript_child_process_gone_error(
                                    process_id,
                                    &child_path.iter().map(String::as_str).collect::<Vec<_>>(),
                                )
                            })?;
                        child.queue_pending_polled_execution_event(PolledExecutionEvent {
                            event: other,
                            reservation,
                        })
                    })?;
                    process_event_notify.notify_one();
                    return Ok(Value::Null);
                }
            }
        }
    }

    fn write_descendant_javascript_child_process_stdin_owned(
        vm: &VmHandle,
        process_id: &str,
        current_process_path: &[String],
        child_process_id: &str,
        chunk: &[u8],
    ) -> Result<(), SidecarError> {
        vm.try_command("write owned descendant stdin", |state| {
            let path = current_process_path
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let mut child_path = path.clone();
            child_path.push(child_process_id);
            let VmState {
                kernel,
                active_processes,
                ..
            } = state;
            let root = active_processes
                .get_mut(process_id)
                .ok_or_else(|| javascript_child_process_gone_error(process_id, &child_path))?;
            let parent = Self::active_process_by_path_mut(root, &path)
                .ok_or_else(|| javascript_child_process_gone_error(process_id, &child_path))?;
            let child = parent
                .child_processes
                .get_mut(child_process_id)
                .ok_or_else(|| javascript_child_process_gone_error(process_id, &child_path))?;
            if let Err(error) = child.execution.write_stdin(chunk) {
                if is_broken_pipe_error(&error) {
                    return Ok(());
                }
                return Err(error);
            }
            write_kernel_process_stdin(kernel, child, chunk)
        })
    }

    fn close_descendant_javascript_child_process_stdin_owned(
        vm: &VmHandle,
        process_id: &str,
        current_process_path: &[String],
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        vm.try_command("close owned descendant stdin", |state| {
            let path = current_process_path
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let mut child_path = path.clone();
            child_path.push(child_process_id);
            let VmState {
                kernel,
                active_processes,
                ..
            } = state;
            let root = active_processes
                .get_mut(process_id)
                .ok_or_else(|| javascript_child_process_gone_error(process_id, &child_path))?;
            let parent = Self::active_process_by_path_mut(root, &path)
                .ok_or_else(|| javascript_child_process_gone_error(process_id, &child_path))?;
            let child = parent
                .child_processes
                .get_mut(child_process_id)
                .ok_or_else(|| javascript_child_process_gone_error(process_id, &child_path))?;
            child.execution.close_stdin()?;
            close_kernel_process_stdin(kernel, child)
        })
    }

    fn kill_descendant_javascript_child_process_owned(
        bridge: &SharedBridge<B>,
        vm: &VmHandle,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[String],
        child_process_id: &str,
        signal_name: &str,
        process_event_notify: &Arc<tokio::sync::Notify>,
    ) -> Result<(), SidecarError> {
        let signal = parse_signal(signal_name)?;
        let audit = vm.try_command("kill owned descendant process", |state| {
            let path = current_process_path
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let mut signal_path = path.clone();
            signal_path.push(child_process_id);
            let signal_key = Self::child_process_signal_key(process_id, &signal_path).to_owned();
            let registration = state
                .signal_states
                .get(&signal_key)
                .and_then(|handlers| handlers.get(&(signal as u32)))
                .cloned();
            let VmState {
                kernel,
                active_processes,
                ..
            } = state;
            let Some(root) = active_processes.get_mut(process_id) else {
                return Ok(None);
            };
            let Some(parent) = Self::active_process_by_path_mut(root, &path) else {
                return Ok(None);
            };
            let source_pid = parent.kernel_pid;
            let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                return Ok(None);
            };
            terminate_tracked_child_process_for_signal(
                kernel,
                child,
                signal,
                registration.as_ref(),
            )?;
            Ok(Some((source_pid, child.kernel_pid)))
        })?;
        if let Some((source_pid, target_pid)) = audit {
            let child_label = if current_process_path.is_empty() {
                child_process_id.to_owned()
            } else {
                format!("{}/{}", current_process_path.join("/"), child_process_id)
            };
            emit_security_audit_event(
                bridge,
                vm_id,
                "security.process.kill",
                audit_fields([
                    (String::from("source"), String::from("guest_child_process")),
                    (String::from("source_pid"), source_pid.to_string()),
                    (String::from("target_pid"), target_pid.to_string()),
                    (String::from("process_id"), process_id.to_owned()),
                    (String::from("child_process_id"), child_label),
                    (String::from("signal"), signal_name.to_owned()),
                ]),
            );
            process_event_notify.notify_one();
        }
        Ok(())
    }

    fn handle_owned_process_kill_rpc(
        bridge: &SharedBridge<B>,
        vm: &VmHandle,
        vm_id: &str,
        process_id: &str,
        source_path: &[String],
        request: &JavascriptSyncRpcRequest,
    ) -> Result<Value, SidecarError> {
        let target_pid = javascript_sync_rpc_arg_i32(&request.args, 0, "process.kill target pid")?;
        let signal_name = javascript_sync_rpc_arg_str(&request.args, 1, "process.kill signal")?;
        let signal = parse_signal(signal_name)?;
        if signal == 0 {
            vm.try_command("probe owned process pid", |state| {
                state
                    .kernel
                    .signal_process(EXECUTION_DRIVER_NAME, target_pid, 0)
                    .map_err(kernel_error)
            })?;
            return Ok(Value::Null);
        }
        let source_pid = vm.try_read("locate owned process.kill source", |state| {
            let root = state.active_processes.get(process_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "ESRCH: unknown process {process_id} during process.kill"
                ))
            })?;
            let path = source_path.iter().map(String::as_str).collect::<Vec<_>>();
            Self::active_process_by_path(root, &path)
                .map(|source| source.kernel_pid)
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "ESRCH: unknown source process {}",
                        Self::child_process_path_label(process_id, &path)
                    ))
                })
        })??;
        if target_pid < 0 {
            let pgid = target_pid.unsigned_abs();
            let members = vm.try_read("list owned process group", |state| {
                state
                    .kernel
                    .list_processes()
                    .into_iter()
                    .filter(|(_, info)| info.pgid == pgid && info.status != ProcessStatus::Exited)
                    .map(|(pid, _)| pid)
                    .collect::<Vec<_>>()
            })?;
            if members.is_empty() {
                return Err(SidecarError::InvalidState(format!(
                    "ESRCH: no such process group {pgid}"
                )));
            }
            for member in members.iter().copied().filter(|pid| *pid != source_pid) {
                match signal_vm_kernel_pid_owned(bridge, vm, vm_id, member, signal_name) {
                    Ok(()) => {}
                    Err(error) if error.to_string().contains("ESRCH") => {}
                    Err(error) => return Err(error),
                }
            }
            if members.contains(&source_pid) {
                if !matches!(
                    canonical_signal_name(signal),
                    Some("SIGWINCH" | "SIGCHLD" | "SIGCONT" | "SIGURG")
                ) {
                    vm.try_command("apply owned process-group self signal", |state| {
                        let VmState {
                            kernel,
                            active_processes,
                            ..
                        } = state;
                        let root = active_processes.get_mut(process_id).ok_or_else(|| {
                            SidecarError::InvalidState(format!(
                                "ESRCH: unknown process {process_id} during process.kill"
                            ))
                        })?;
                        let path = source_path.iter().map(String::as_str).collect::<Vec<_>>();
                        let source =
                            Self::active_process_by_path_mut(root, &path).ok_or_else(|| {
                                SidecarError::InvalidState(String::from(
                                    "ESRCH: process.kill source disappeared",
                                ))
                            })?;
                        apply_active_process_default_signal(kernel, source, signal)
                    })?;
                }
                return Ok(json!({ "self": true, "action": "default" }));
            }
            return Ok(Value::Null);
        }
        let target_kernel_pid = u32::try_from(target_pid).map_err(|_| {
            SidecarError::InvalidState(format!("EINVAL: invalid process pid {target_pid}"))
        })?;
        let located = vm.try_read("classify owned process.kill target", |state| {
            state.active_processes.iter().find_map(|(root_id, root)| {
                Self::active_process_path_by_kernel_pid(root, target_kernel_pid).map(|path| {
                    let signal_key = path.last().map(String::as_str).unwrap_or(root_id);
                    let action = state
                        .signal_states
                        .get(signal_key)
                        .and_then(|handlers| handlers.get(&(signal as u32)))
                        .map(|registration| registration.action.clone());
                    (root_id.clone(), path, action)
                })
            })
        })?;
        signal_vm_kernel_pid_owned(bridge, vm, vm_id, target_kernel_pid, signal_name)?;
        let Some((target_root_id, target_path, registration)) = located else {
            return Ok(Value::Null);
        };
        if source_pid == target_kernel_pid {
            return Ok(json!({ "self": true, "action": "default" }));
        }
        let action = match registration {
            Some(SignalDispositionAction::Ignore) => "ignore",
            Some(SignalDispositionAction::User) => "user",
            Some(SignalDispositionAction::Default) | None
                if matches!(
                    canonical_signal_name(signal),
                    Some("SIGWINCH" | "SIGCHLD" | "SIGURG")
                ) =>
            {
                "ignore"
            }
            Some(SignalDispositionAction::Default) | None => "default",
        };
        let path = target_path.iter().map(String::as_str).collect::<Vec<_>>();
        let target_path_label = Self::child_process_path_label(&target_root_id, &path);
        Ok(json!({
            "self": false,
            "action": action,
            "signal": signal_name,
            "number": signal,
            "targetProcessPath": target_path_label,
        }))
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn spawn_descendant_javascript_child_process_for_test(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        request: JavascriptChildProcessSpawnRequest,
    ) -> Result<Value, SidecarError> {
        self.spawn_descendant_javascript_child_process(
            vm_id,
            process_id,
            current_process_path,
            request,
        )
        .await
    }

    async fn defer_descendant_javascript_child_process_sync(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        request: JavascriptChildProcessSpawnRequest,
        max_buffer: Option<usize>,
    ) -> Result<JavascriptSyncRpcServiceResponse, SidecarError> {
        let max_buffer = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let root = vm
                .active_processes
                .get(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            let parent =
                Self::active_process_by_path(root, current_process_path).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "unknown child process path during nested spawnSync",
                    ))
                })?;
            Self::child_process_sync_max_buffer(parent, max_buffer)?
        };
        let sync_input = javascript_child_process_sync_input_bytes(request.options.input.as_ref())?;
        let deadline = request
            .options
            .timeout
            .map(|timeout_ms| Instant::now() + Duration::from_millis(timeout_ms));
        let timeout_signal = request
            .options
            .kill_signal
            .clone()
            .unwrap_or_else(|| String::from("SIGTERM"));
        let spawned = self
            .spawn_descendant_javascript_child_process(
                vm_id,
                process_id,
                current_process_path,
                request,
            )
            .await?;
        let child_process_id = spawned
            .get("childId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "child_process.spawn_sync response is missing childId",
                ))
            })?
            .to_owned();
        let pid = spawned
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "child_process.spawn_sync response is missing a valid pid",
                ))
            })?;

        if let Some(input) = sync_input.as_deref() {
            self.write_descendant_javascript_child_process_stdin(
                vm_id,
                process_id,
                current_process_path,
                &child_process_id,
                input,
            )?;
        }
        self.close_descendant_javascript_child_process_stdin(
            vm_id,
            process_id,
            current_process_path,
            &child_process_id,
        )?;

        let (respond_to, receiver) = tokio::sync::oneshot::channel();
        let (runtime, notify) = {
            let mut vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let root = vm
                .active_processes
                .get_mut(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            let parent =
                Self::active_process_by_path_mut(root, current_process_path).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "unknown child process path during nested spawnSync",
                    ))
                })?;
            parent.pending_child_process_sync.insert(
                child_process_id,
                PendingChildProcessSync {
                    pid,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    max_buffer,
                    deadline,
                    timeout_signal,
                    kill_sent: false,
                    timed_out: false,
                    max_buffer_exceeded: false,
                    completion: PendingChildProcessSyncCompletion::Javascript(respond_to),
                },
            );
            (
                parent.runtime_context.clone(),
                Arc::clone(&parent.process_event_notify),
            )
        };
        if let Some(deadline) = deadline {
            let delay = deadline.saturating_duration_since(Instant::now());
            runtime
                .spawn(agentos_runtime::TaskClass::Timer, async move {
                    tokio::time::sleep(delay).await;
                    notify.notify_one();
                })
                .map_err(SidecarError::from)?;
        }
        Ok(JavascriptSyncRpcServiceResponse::Deferred {
            receiver,
            timeout: None,
            task_class: agentos_runtime::TaskClass::Vm,
        })
    }

    async fn handle_descendant_javascript_child_process_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        request: &JavascriptSyncRpcRequest,
    ) -> Result<JavascriptSyncRpcServiceResponse, SidecarError> {
        match request.method.as_str() {
            "child_process.spawn" => {
                let payload = {
                    let Some(vm) = self.vms.get(vm_id) else {
                        return Ok(Value::Null.into());
                    };
                    parse_javascript_child_process_spawn_request(&vm, &request.args)?.0
                };
                self.spawn_descendant_javascript_child_process(
                    vm_id,
                    process_id,
                    current_process_path,
                    payload,
                )
                .await
                .map(Into::into)
            }
            "child_process.spawn_sync" => {
                let (payload, max_buffer) = {
                    let Some(vm) = self.vms.get(vm_id) else {
                        return Ok(Value::Null.into());
                    };
                    parse_javascript_child_process_spawn_request(&vm, &request.args)?
                };
                self.defer_descendant_javascript_child_process_sync(
                    vm_id,
                    process_id,
                    current_process_path,
                    payload,
                    max_buffer,
                )
                .await
            }
            "child_process.poll" => {
                let child_process_id =
                    javascript_sync_rpc_arg_str(&request.args, 0, "child_process.poll child id")?;
                let wait_ms = javascript_sync_rpc_arg_u64_optional(
                    &request.args,
                    1,
                    "child_process.poll wait ms",
                )?
                .unwrap_or_default();
                Box::pin(self.poll_descendant_javascript_child_process(
                    vm_id,
                    process_id,
                    current_process_path,
                    child_process_id,
                    wait_ms,
                    false,
                    None,
                    None,
                    None,
                    None,
                    None,
                ))
                .await
                .map(Into::into)
            }
            "child_process.write_stdin" => {
                let child_process_id = javascript_sync_rpc_arg_str(
                    &request.args,
                    0,
                    "child_process.write_stdin child id",
                )?;
                let chunk = javascript_sync_rpc_bytes_arg(
                    &request.args,
                    1,
                    "child_process.write_stdin chunk",
                )?;
                self.write_descendant_javascript_child_process_stdin(
                    vm_id,
                    process_id,
                    current_process_path,
                    child_process_id,
                    &chunk,
                )
                .map(|()| Value::Null.into())
            }
            "child_process.close_stdin" => {
                let child_process_id = javascript_sync_rpc_arg_str(
                    &request.args,
                    0,
                    "child_process.close_stdin child id",
                )?;
                self.close_descendant_javascript_child_process_stdin(
                    vm_id,
                    process_id,
                    current_process_path,
                    child_process_id,
                )
                .map(|()| Value::Null.into())
            }
            "child_process.kill" => {
                let child_process_id =
                    javascript_sync_rpc_arg_str(&request.args, 0, "child_process.kill child id")?;
                let signal =
                    javascript_sync_rpc_arg_str(&request.args, 1, "child_process.kill signal")?;
                self.kill_descendant_javascript_child_process(
                    vm_id,
                    process_id,
                    current_process_path,
                    child_process_id,
                    signal,
                )
                .map(|()| Value::Null.into())
            }
            _ => Err(SidecarError::InvalidState(format!(
                "unsupported nested child process RPC method {}",
                request.method
            ))),
        }
    }

    /// Deferred servicing for a child's blocking kernel read, poll, or stdio
    /// write inside the child-event pump. Each operation is probed without
    /// blocking the sidecar actor; an unavailable write is parked by reply
    /// token and retried after the parent consumes pipe data. The pump loop
    /// re-checks the parked RPC every iteration. Returns false when the RPC
    /// must use the normal inline path (local JS stdin or shared TTY output).
    fn service_child_kernel_wait_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        request: &JavascriptSyncRpcRequest,
    ) -> Result<bool, SidecarError> {
        let event_notify = Arc::clone(&self.process_event_notify);
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Ok(true);
        };
        let vm = &mut *vm;
        let operation_deadline_ms = vm.limits.reactor.operation_deadline_ms;
        let runtime = vm.runtime_context.clone();
        let kernel = &mut vm.kernel;
        let Some(root) = vm.active_processes.get_mut(process_id) else {
            return Ok(true);
        };
        let Some(parent) = Self::active_process_by_path_mut(root, current_process_path) else {
            return Ok(true);
        };
        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
            return Ok(true);
        };
        if request.method == "__kernel_stdio_write" || request.method == "process.fd_write" {
            if request.method == "__kernel_stdio_write" && child.tty_master_owner.is_some() {
                return Ok(false);
            }
            let now = Instant::now();
            let (deadline, arm_deadline_wake) = match &child.deferred_kernel_wait_rpc {
                Some((parked, parked_deadline)) if parked.id == request.id => (
                    parked_deadline.unwrap_or(now + Duration::from_millis(operation_deadline_ms)),
                    false,
                ),
                _ => (now + Duration::from_millis(operation_deadline_ms), true),
            };
            let response = if request.method == "__kernel_stdio_write" {
                service_javascript_kernel_stdio_write_sync_rpc(kernel, child, request)
            } else {
                service_javascript_kernel_fd_write_sync_rpc(kernel, child, request)
            };
            match response {
                Ok(response) => {
                    child.clear_deferred_kernel_wait_rpc();
                    child
                        .execution
                        .respond_javascript_sync_rpc_response(request.id, response.into())
                        .or_else(ignore_stale_javascript_sync_rpc_response)?;
                }
                Err(error)
                    if javascript_sync_rpc_error_code(&error) == "EAGAIN" && now >= deadline =>
                {
                    child.clear_deferred_kernel_wait_rpc();
                    child
                        .execution
                        .respond_javascript_sync_rpc_error(
                            request.id,
                            "ETIMEDOUT",
                            format!(
                                "pipe write exceeded limits.reactor.operationDeadlineMs ({operation_deadline_ms} ms); raise that limit for slower readers"
                            ),
                        )
                        .or_else(ignore_stale_javascript_sync_rpc_response)?;
                }
                Err(error) if javascript_sync_rpc_error_code(&error) == "EAGAIN" => {
                    if arm_deadline_wake {
                        let delay = deadline.saturating_duration_since(now);
                        child.clear_deferred_kernel_wait_rpc();
                        child.deferred_kernel_wait_rpc = Some((request.clone(), Some(deadline)));
                        let timer = runtime.spawn(agentos_runtime::TaskClass::Timer, async move {
                            tokio::time::sleep(delay).await;
                            event_notify.notify_one();
                        });
                        match timer {
                            Ok(timer) => {
                                child.deferred_child_write_timer = Some(timer);
                            }
                            Err(agentos_runtime::TaskSpawnError::ResourceLimit(limit)) => {
                                child.clear_deferred_kernel_wait_rpc();
                                child
                                    .execution
                                    .respond_javascript_sync_rpc_error(
                                        request.id,
                                        "ERR_AGENTOS_RESOURCE_LIMIT",
                                        crate::state::guest_limit_message(&limit),
                                    )
                                    .or_else(ignore_stale_javascript_sync_rpc_response)?;
                            }
                            Err(
                                error @ agentos_runtime::TaskSpawnError::AdmissionClosed { .. },
                            ) => {
                                child.clear_deferred_kernel_wait_rpc();
                                child
                                    .execution
                                    .respond_javascript_sync_rpc_error(
                                        request.id,
                                        "ERR_AGENTOS_TASK_ADMISSION_CLOSED",
                                        error.to_string(),
                                    )
                                    .or_else(ignore_stale_javascript_sync_rpc_response)?;
                            }
                        }
                    } else {
                        child.deferred_kernel_wait_rpc = Some((request.clone(), Some(deadline)));
                    }
                }
                Err(error) => {
                    child.clear_deferred_kernel_wait_rpc();
                    tracing::warn!(
                        method = %request.method,
                        error = %error,
                        "child JavaScript sync RPC failed"
                    );
                    child
                        .execution
                        .respond_javascript_sync_rpc_error(
                            request.id,
                            javascript_sync_rpc_error_code(&error),
                            javascript_sync_rpc_error_message(&error),
                        )
                        .or_else(ignore_stale_javascript_sync_rpc_response)?;
                }
            }
            return Ok(true);
        }
        if request.method == "__kernel_stdin_read"
            && matches!(child.execution, ActiveExecution::Javascript(_))
            && child.tty_master_fd.is_none()
            && !child.direct_posix_stdin
            && child.kernel_stdin_writer_fd.is_some()
        {
            return Ok(false);
        }
        if request.method == "process.fd_read" {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_read fd")?;
            let stat = kernel
                .fd_stat(EXECUTION_DRIVER_NAME, child.kernel_pid, fd)
                .map_err(kernel_error)?;
            if matches!(
                stat.filetype,
                agentos_kernel::fd_table::FILETYPE_REGULAR_FILE
                    | agentos_kernel::fd_table::FILETYPE_DIRECTORY
                    | agentos_kernel::fd_table::FILETYPE_SYMBOLIC_LINK
            ) {
                // Ordinary VFS descriptors are immediately serviced by the
                // normal RPC handler. Parking them behind poll readiness can
                // deadlock a nested command before it produces pipeline data.
                return Ok(false);
            }
        }
        // The child draining its stdin pipe is what frees capacity for queued
        // host bytes and eventually delivers a deferred close/EOF.
        flush_pending_kernel_stdin(kernel, child)?;
        let now = Instant::now();
        let requested_timeout_ms = match request.method.as_str() {
            "__kernel_stdin_read" => parse_kernel_stdin_read_args(request)?.1,
            "__kernel_poll" => {
                let timeout_ms = parse_kernel_poll_args(request)?.1;
                (timeout_ms >= 0).then_some(timeout_ms as u64)
            }
            "process.fd_read" => Some(
                javascript_sync_rpc_arg_u64_optional(&request.args, 2, "fd_read timeout ms")?
                    .unwrap_or(DEFAULT_KERNEL_STDIN_READ_TIMEOUT_MS),
            ),
            _ => return Ok(false),
        };
        let deadline = match &child.deferred_kernel_wait_rpc {
            Some((parked, parked_deadline)) if parked.id == request.id => *parked_deadline,
            _ => requested_timeout_ms.map(|timeout_ms| now + Duration::from_millis(timeout_ms)),
        };
        let kernel_pid = child.kernel_pid;
        let mut fd_read = None;
        let probe = match request.method.as_str() {
            "__kernel_stdin_read" => {
                let (max_bytes, _) = parse_kernel_stdin_read_args(request)?;
                kernel_stdin_read_response(
                    kernel,
                    kernel_pid,
                    child.kernel_stdin_reader_fd,
                    max_bytes,
                    Duration::ZERO,
                )
                .map(|value| (value, true))
            }
            "__kernel_poll" => {
                let (fd_requests, _) = parse_kernel_poll_args(request)?;
                kernel_poll_response(kernel, kernel_pid, &fd_requests, 0).map(|value| (value, true))
            }
            "process.fd_read" => {
                let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_read fd")?;
                let length = usize::try_from(javascript_sync_rpc_arg_u64(
                    &request.args,
                    1,
                    "fd_read length",
                )?)
                .map_err(|_| SidecarError::InvalidState("fd_read length is too large".into()))?;
                fd_read = Some((fd, length));
                kernel
                    .poll_fds(
                        EXECUTION_DRIVER_NAME,
                        kernel_pid,
                        vec![PollFd::new(fd, POLLIN)],
                        0,
                    )
                    .map(|result| (Value::Null, result.ready_count > 0))
                    .map_err(kernel_error)
            }
            _ => unreachable!("unsupported deferred kernel wait method"),
        };
        let (probe, fd_read_ready) = match probe {
            Ok(probe) => probe,
            Err(error) => {
                child.clear_deferred_kernel_wait_rpc();
                child
                    .execution
                    .respond_javascript_sync_rpc_error(
                        request.id,
                        javascript_sync_rpc_error_code(&error),
                        javascript_sync_rpc_error_message(&error),
                    )
                    .or_else(ignore_stale_javascript_sync_rpc_response)?;
                return Ok(true);
            }
        };
        let ready = match request.method.as_str() {
            "__kernel_stdin_read" => !probe.is_null(),
            "__kernel_poll" => probe.get("readyCount").and_then(Value::as_u64).unwrap_or(0) > 0,
            "process.fd_read" => fd_read_ready,
            _ => unreachable!("unsupported deferred kernel wait method"),
        };
        if ready
            || requested_timeout_ms == Some(0)
            || deadline.is_some_and(|deadline| now >= deadline)
        {
            child.clear_deferred_kernel_wait_rpc();
            if let Some((fd, length)) = fd_read {
                // Claim before the destructive read. A stale reply token must
                // not consume bytes intended for a later read on this fd.
                let claimed = child
                    .execution
                    .claim_javascript_sync_rpc_response(request.id)?;
                if !claimed {
                    return Ok(true);
                }
                let read_result = kernel.fd_read_with_timeout_result(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    fd,
                    length,
                    Some(Duration::ZERO),
                );
                match read_result {
                    Ok(Some(bytes)) => child
                        .execution
                        .respond_claimed_javascript_sync_rpc_success(
                            request.id,
                            javascript_sync_rpc_bytes_value(&bytes),
                        )?,
                    Ok(None) => child
                        .execution
                        .respond_claimed_javascript_sync_rpc_success(
                            request.id,
                            javascript_sync_rpc_bytes_value(&[]),
                        )?,
                    Err(error) => {
                        let error = kernel_error(error);
                        child.execution.respond_claimed_javascript_sync_rpc_error(
                            request.id,
                            javascript_sync_rpc_error_code(&error),
                            error.to_string(),
                        )?;
                    }
                }
                return Ok(true);
            }
            child
                .execution
                .respond_javascript_sync_rpc_response(request.id, probe.into())
                .or_else(ignore_stale_javascript_sync_rpc_response)?;
            return Ok(true);
        }
        child.deferred_kernel_wait_rpc = Some((request.clone(), deadline));
        Ok(true)
    }

    /// Service `__kernel_stdio_write` for a process writing to the SHARED
    /// terminal (`tty_master_owner` set): write through the process's own PTY
    /// slave (line discipline applies), then drain the master and surface the
    /// drained bytes as the OWNER's ordered output stream — the single
    /// host-facing path. No child stdout event is queued, so nothing gets
    /// relayed (and re-rendered) by the parent shell.
    pub(crate) fn service_shared_tty_stdio_write(
        &mut self,
        vm_id: &str,
        writer_kernel_pid: u32,
        owner: (u32, u32),
        request: &JavascriptSyncRpcRequest,
    ) -> Result<Value, SidecarError> {
        let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_stdio_write fd")?;
        let chunk = javascript_sync_rpc_bytes_arg(&request.args, 1, "__kernel_stdio_write chunk")?;
        if fd != 1 && fd != 2 {
            return Err(SidecarError::InvalidState(format!(
                "__kernel_stdio_write only supports fd 1/2, got {fd}"
            )));
        }
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Ok(json!(chunk.len()));
        };
        let written = if fd == 1 {
            vm.kernel
                .write_process_stdout(EXECUTION_DRIVER_NAME, writer_kernel_pid, &chunk)
                .map_err(kernel_error)?
        } else {
            vm.kernel
                .write_process_stderr(EXECUTION_DRIVER_NAME, writer_kernel_pid, &chunk)
                .map_err(kernel_error)?
        };
        let (owner_pid, master_fd) = owner;
        let mut drained: Vec<u8> = Vec::new();
        loop {
            match vm.kernel.fd_read_with_timeout_result(
                EXECUTION_DRIVER_NAME,
                owner_pid,
                master_fd,
                MAX_PTY_BUFFER_BYTES,
                Some(Duration::ZERO),
            ) {
                Ok(Some(bytes)) if !bytes.is_empty() => drained.extend(bytes),
                Ok(_) => break,
                Err(error) if error.code() == "EAGAIN" => break,
                Err(error) => return Err(kernel_error(error)),
            }
        }
        if !drained.is_empty() {
            if let Some(owner_process) = vm
                .active_processes
                .values_mut()
                .find(|process| process.kernel_pid == owner_pid)
            {
                owner_process
                    .queue_pending_execution_event(ActiveExecutionEvent::Stdout(drained))?;
            }
        }
        Ok(json!(written))
    }

    /// Re-check a child's parked kernel-wait RPC (see
    /// `service_child_kernel_wait_rpc`); called once per pump-loop iteration.
    fn recheck_child_deferred_kernel_wait_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let parked = {
            let Some(mut vm) = self.vms.get_mut(vm_id) else {
                return Ok(());
            };
            let Some(root) = vm.active_processes.get_mut(process_id) else {
                return Ok(());
            };
            let Some(parent) = Self::active_process_by_path_mut(root, current_process_path) else {
                return Ok(());
            };
            let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                return Ok(());
            };
            child
                .deferred_kernel_wait_rpc
                .as_ref()
                .map(|(request, _)| request.clone())
        };
        if let Some(request) = parked {
            let _ = self.service_child_kernel_wait_rpc(
                vm_id,
                process_id,
                current_process_path,
                child_process_id,
                &request,
            )?;
        }
        Ok(())
    }

    fn check_descendant_public_process_event_capacity(
        &self,
        vm_id: &str,
        public_process_id: &str,
        event: ActiveExecutionEvent,
    ) -> Result<(), SidecarError> {
        let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        self.check_pending_process_event_capacity(&ProcessEventEnvelope {
            connection_id: vm.connection_id.clone(),
            session_id: vm.session_id.clone(),
            vm_id: vm_id.to_owned(),
            child_path: Vec::new(),
            process_id: public_process_id.to_owned(),
            event,
        })
    }

    fn requeue_descendant_claimed_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        event: ActiveExecutionEvent,
        reservation: Option<PendingExecutionEventReservation>,
    ) -> Result<(), SidecarError> {
        let mut vm = self
            .vms
            .get_mut(vm_id)
            .ok_or_else(|| missing_vm_error(vm_id))?;
        let parent = Self::descendant_parent_process_mut(&mut vm, process_id, current_process_path)
            .ok_or_else(|| javascript_child_process_gone_error(process_id, current_process_path))?;
        let child = parent
            .child_processes
            .get_mut(child_process_id)
            .ok_or_else(|| {
                let mut child_path = current_process_path.to_vec();
                child_path.push(child_process_id);
                javascript_child_process_gone_error(process_id, &child_path)
            })?;
        child.requeue_pending_execution_event(PolledExecutionEvent { event, reservation })
    }

    fn poll_descendant_javascript_child_process_nowait(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        preserve_pull_owned_events: bool,
        owned_javascript_services: &mut Vec<OwnedJavascriptEventService>,
        owned_python_services: &mut Vec<OwnedPythonEventService>,
        owned_python_socket_completions: &mut Vec<OwnedPythonSocketCompletionService>,
        public_process_id: Option<&str>,
    ) -> Result<ClaimedDescendantBridgeEvent, SidecarError> {
        let mut claimed_reservation = None;
        let mut future = Box::pin(self.poll_descendant_javascript_child_process(
            vm_id,
            process_id,
            current_process_path,
            child_process_id,
            0,
            preserve_pull_owned_events,
            Some(owned_javascript_services),
            Some(owned_python_services),
            Some(owned_python_socket_completions),
            Some(&mut claimed_reservation),
            public_process_id,
        ));
        let mut context = Context::from_waker(Waker::noop());
        let poll = future.as_mut().poll(&mut context);
        drop(future);
        match poll {
            Poll::Ready(result) => result.map(|event| ClaimedDescendantBridgeEvent {
                event,
                reservation: claimed_reservation,
            }),
            Poll::Pending => Err(SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_CHILD_EVENT_TURN_SUSPENDED: bounded child event claim unexpectedly suspended",
            ))),
        }
    }

    async fn poll_descendant_javascript_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        wait_ms: u64,
        preserve_pull_owned_events: bool,
        mut owned_javascript_services: Option<&mut Vec<OwnedJavascriptEventService>>,
        mut owned_python_services: Option<&mut Vec<OwnedPythonEventService>>,
        mut owned_python_socket_completions: Option<&mut Vec<OwnedPythonSocketCompletionService>>,
        mut claimed_bridge_reservation: Option<&mut Option<PendingExecutionEventReservation>>,
        public_process_id: Option<&str>,
    ) -> Result<Value, SidecarError> {
        let mut child_path = current_process_path.to_vec();
        child_path.push(child_process_id);
        let child_gone_error = || javascript_child_process_gone_error(process_id, &child_path);
        // `wait_ms` remains on the compatibility RPC surface for WASM/native
        // callers, but the sidecar never parks while servicing it. Runtime
        // event producers wake the shared process pump instead.
        let _ = wait_ms;

        loop {
            self.drain_queued_descendant_javascript_child_process_events(
                vm_id,
                process_id,
                &child_path,
            )?;
            self.recheck_child_deferred_kernel_wait_rpc(
                vm_id,
                process_id,
                current_process_path,
                child_process_id,
            )?;
            enum ChildPollResult {
                Event(Box<Option<PolledExecutionEvent>>),
                RecoverRuntimeExit,
                Timeout,
            }
            let poll_result = {
                let Some(mut vm) = self.vms.get_mut(vm_id) else {
                    return Ok(Value::Null);
                };
                let Some(parent) =
                    Self::descendant_parent_process_mut(&mut vm, process_id, current_process_path)
                else {
                    return Err(child_gone_error());
                };
                let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                    return Err(child_gone_error());
                };
                if let Some(event) = child.lease_pending_execution_event() {
                    ChildPollResult::Event(Box::new(Some(event)))
                } else {
                    match child.try_poll_execution_event() {
                        Ok(Some(event)) => ChildPollResult::Event(Box::new(Some(event))),
                        Ok(None) => ChildPollResult::Timeout,
                        Err(SidecarError::Execution(message))
                            if (child.runtime == GuestRuntimeKind::JavaScript
                                && closed_javascript_event_channel(&message))
                                || (child.runtime == GuestRuntimeKind::Python
                                    && closed_python_event_channel(&message))
                                || (child.runtime == GuestRuntimeKind::WebAssembly
                                    && closed_wasm_event_channel(&message)) =>
                        {
                            ChildPollResult::RecoverRuntimeExit
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            let event = match poll_result {
                ChildPollResult::Event(event) => *event,
                ChildPollResult::Timeout => return Ok(Value::Null),
                ChildPollResult::RecoverRuntimeExit => self
                    .recover_descendant_runtime_child_process_event(
                        vm_id,
                        process_id,
                        current_process_path,
                        child_process_id,
                    )?
                    .map(PolledExecutionEvent::unreserved),
            };

            let Some(event) = event else {
                return Ok(Value::Null);
            };

            let PolledExecutionEvent {
                event,
                mut reservation,
            } = event;
            let synthetic_signal_termination = matches!(
                &event,
                ActiveExecutionEvent::Stderr(chunk)
                    if chunk.as_slice() == SYNTHETIC_V8_TERMINATION_STDERR
            ) && self.vms.get(vm_id).is_some_and(|vm| {
                vm.active_processes
                    .get(process_id)
                    .and_then(|root| Self::active_process_by_path(root, current_process_path))
                    .and_then(|parent| parent.child_processes.get(child_process_id))
                    .is_some_and(|child| child.exit_signal.is_some())
            });
            if synthetic_signal_termination {
                // The following exit event carries the authoritative signal status.
                drop(reservation);
                continue;
            }
            if preserve_pull_owned_events {
                let Some(mut vm) = self.vms.get_mut(vm_id) else {
                    return Ok(Value::Null);
                };
                let Some(parent) =
                    Self::descendant_parent_process_mut(&mut vm, process_id, current_process_path)
                else {
                    return Ok(Value::Null);
                };
                let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                    return Ok(Value::Null);
                };
                if let ActiveExecutionEvent::Exited(exit_code) = &event {
                    child.kernel_handle.finish(*exit_code);
                }
                child.queue_pending_polled_execution_event(PolledExecutionEvent {
                    event,
                    reservation,
                })?;
                return Ok(Value::Null);
            }
            match event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    if let Some(public_process_id) = public_process_id {
                        if let Err(error) = self.check_descendant_public_process_event_capacity(
                            vm_id,
                            public_process_id,
                            ActiveExecutionEvent::Stdout(chunk.clone()),
                        ) {
                            tracing::debug!(
                                %error,
                                vm_id,
                                process_id = public_process_id,
                                "detached descendant stdout remains leased at producer capacity"
                            );
                            self.requeue_descendant_claimed_event(
                                vm_id,
                                process_id,
                                current_process_path,
                                child_process_id,
                                ActiveExecutionEvent::Stdout(chunk),
                                reservation,
                            )?;
                            return Ok(Value::Null);
                        }
                    }
                    if let Some(slot) = claimed_bridge_reservation.as_deref_mut() {
                        *slot = reservation.take();
                    }
                    return Ok(json!({
                        "type": "stdout",
                        "data": javascript_sync_rpc_bytes_value(&chunk),
                    }));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    if let Some(public_process_id) = public_process_id {
                        if let Err(error) = self.check_descendant_public_process_event_capacity(
                            vm_id,
                            public_process_id,
                            ActiveExecutionEvent::Stderr(chunk.clone()),
                        ) {
                            tracing::debug!(
                                %error,
                                vm_id,
                                process_id = public_process_id,
                                "detached descendant stderr remains leased at producer capacity"
                            );
                            self.requeue_descendant_claimed_event(
                                vm_id,
                                process_id,
                                current_process_path,
                                child_process_id,
                                ActiveExecutionEvent::Stderr(chunk),
                                reservation,
                            )?;
                            return Ok(Value::Null);
                        }
                    }
                    if let Some(slot) = claimed_bridge_reservation.as_deref_mut() {
                        *slot = reservation.take();
                    }
                    return Ok(json!({
                        "type": "stderr",
                        "data": javascript_sync_rpc_bytes_value(&chunk),
                    }));
                }
                ActiveExecutionEvent::Exited(mut exit_code) => {
                    let cleanup_start = Instant::now();
                    let had_trailing_events = {
                        let Some(mut vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(parent) = Self::descendant_parent_process_mut(
                            &mut vm,
                            process_id,
                            current_process_path,
                        ) else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        loop {
                            let next = poll_child_execution_after_exit(child)?;
                            let Some(next) = next else {
                                break;
                            };
                            if matches!(next.event(), ActiveExecutionEvent::Exited(_)) {
                                continue;
                            }
                            child.queue_pending_polled_execution_event(next)?;
                        }
                        if !child.pending_execution_events.is_empty() {
                            // Preserve Node ordering: output and signal-state
                            // events already queued for the child must be
                            // observed before its terminal exit. Requeueing the
                            // exit at the front spins forever on the same exit
                            // while a trailing event remains behind it.
                            child.queue_pending_polled_execution_event(PolledExecutionEvent {
                                event: ActiveExecutionEvent::Exited(exit_code),
                                reservation: reservation.take(),
                            })?;
                            true
                        } else {
                            false
                        }
                    };
                    if had_trailing_events {
                        continue;
                    }

                    if let Some(public_process_id) = public_process_id {
                        if let Err(error) = self.check_descendant_public_process_event_capacity(
                            vm_id,
                            public_process_id,
                            ActiveExecutionEvent::Exited(exit_code),
                        ) {
                            tracing::debug!(
                                %error,
                                vm_id,
                                process_id = public_process_id,
                                "detached descendant exit remains leased before cleanup"
                            );
                            self.requeue_descendant_claimed_event(
                                vm_id,
                                process_id,
                                current_process_path,
                                child_process_id,
                                ActiveExecutionEvent::Exited(exit_code),
                                reservation,
                            )?;
                            return Ok(Value::Null);
                        }
                    }

                    // The native wait status is authoritative for whether the
                    // runner exited normally or was terminated by a signal.
                    // Never infer a signal from 128+N: a program can
                    // legitimately call exit(137).
                    {
                        let Some(mut vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(parent) = Self::descendant_parent_process_mut(
                            &mut vm,
                            process_id,
                            current_process_path,
                        ) else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        let runtime_pid = child.execution.child_pid();
                        if runtime_pid != 0 && !child.execution.uses_shared_v8_runtime() {
                            if let RuntimeChildStatusObservation::Exited(status) =
                                runtime_child_exit_status(runtime_pid)?
                            {
                                exit_code = status.status;
                                child.exit_signal = status.signal;
                                child.exit_core_dumped = status.core_dumped;
                            }
                        }
                    }

                    // Detached descendant exits cross two ownership domains:
                    // the child tree and the public process-event queue. Make
                    // the public handoff durable before removing or tearing
                    // down the child. If admission changed after the earlier
                    // capacity probe, put the exact claimed exit (including
                    // its retained-byte reservation) back on the child.
                    let public_event_queued = if let Some(public_process_id) = public_process_id {
                        let Some((connection_id, session_id)) = self
                            .vms
                            .get(vm_id)
                            .map(|vm| (vm.connection_id.clone(), vm.session_id.clone()))
                        else {
                            return Ok(Value::Null);
                        };
                        let envelope = ProcessEventEnvelope {
                            connection_id,
                            session_id,
                            vm_id: vm_id.to_owned(),
                            child_path: Vec::new(),
                            process_id: public_process_id.to_owned(),
                            event: ActiveExecutionEvent::Exited(exit_code),
                        };
                        if let Err((error, envelope)) =
                            self.try_queue_pending_process_event(envelope)
                        {
                            self.requeue_descendant_claimed_event(
                                vm_id,
                                process_id,
                                current_process_path,
                                child_process_id,
                                envelope.event,
                                reservation.take(),
                            )?;
                            return Err(error);
                        }
                        drop(reservation.take());
                        true
                    } else {
                        false
                    };

                    let parent_signal_key =
                        Self::child_process_signal_key(process_id, current_process_path);
                    let Some(mut vm) = self.vms.get_mut(vm_id) else {
                        return Ok(Value::Null);
                    };
                    let (signal_name, core_dumped) = {
                        let Some(parent) = Self::descendant_parent_process_mut(
                            &mut vm,
                            process_id,
                            current_process_path,
                        ) else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        let actual_signal = child.exit_signal.take();
                        child.pending_self_signal_exit = None;
                        (
                            actual_signal
                                .and_then(canonical_signal_name)
                                .map(str::to_owned),
                            child.exit_core_dumped,
                        )
                    };
                    let (
                        parent_runtime_pid,
                        parent_v8_signal_session,
                        parent_is_wasm,
                        should_signal_parent,
                    ) = {
                        let Some(parent) = Self::descendant_parent_process(
                            &mut vm,
                            process_id,
                            current_process_path,
                        ) else {
                            return Ok(Value::Null);
                        };
                        (
                            parent.execution.child_pid(),
                            parent.execution.javascript_v8_session_handle().filter(|_| {
                                matches!(
                                    &parent.execution,
                                    ActiveExecution::Javascript(execution)
                                        if execution.uses_shared_v8_runtime()
                                )
                            }),
                            matches!(&parent.execution, ActiveExecution::Wasm(_)),
                            vm.signal_states
                                .get(parent_signal_key)
                                .and_then(|handlers| handlers.get(&(libc::SIGCHLD as u32)))
                                .is_some_and(|registration| {
                                    registration.action != SignalDispositionAction::Default
                                }),
                        )
                    };
                    let Some(parent) = Self::descendant_parent_process_mut(
                        &mut vm,
                        process_id,
                        current_process_path,
                    ) else {
                        return Ok(Value::Null);
                    };
                    let Some(mut child) = parent.child_processes.remove(child_process_id) else {
                        return Ok(Value::Null);
                    };
                    let child_process_label =
                        Self::child_process_path_label(process_id, &child_path);
                    let detached_children =
                        Self::adopt_detached_child_processes(&child_process_label, &mut child);
                    // A WASM child writes directly to the kernel VFS. Importing
                    // its unchanged host shadow here would overwrite those
                    // writes with the pre-spawn snapshot (for example, undoing
                    // an append to an existing file). Host-backed runtimes still
                    // need their dirty or otherwise non-observable writes
                    // reconciled before teardown, matching root-process exit.
                    if child.host_write_dirty_recursive()
                        || !child.clean_host_writes_are_observable_recursive()
                    {
                        sync_process_host_writes_to_kernel(&mut vm, &child)?;
                    }
                    release_inherited_child_raw_mode(&mut vm.kernel, &child)?;
                    let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                    let unix_address_registry = Arc::clone(&vm.unix_address_registry);
                    terminate_child_process_tree(
                        &mut vm.kernel,
                        &mut child,
                        &kernel_readiness,
                        &unix_address_registry,
                    );
                    child.kernel_handle.finish(exit_code);
                    let _ = vm.kernel.wait_and_reap(child.kernel_pid);
                    vm.signal_states.remove(child_process_id);
                    for (detached_process_id, detached_child) in detached_children {
                        vm.detached_child_processes
                            .insert(detached_process_id.clone());
                        vm.active_processes
                            .insert(detached_process_id, detached_child);
                    }
                    if should_signal_parent {
                        if parent_is_wasm {
                            let Some(parent) = Self::descendant_parent_process_mut(
                                &mut vm,
                                process_id,
                                current_process_path,
                            ) else {
                                return Ok(Value::Null);
                            };
                            parent.queue_pending_wasm_signal(libc::SIGCHLD)?;
                        } else if let Some(session) = parent_v8_signal_session {
                            dispatch_v8_session_signal(session, libc::SIGCHLD);
                        } else {
                            signal_runtime_process(parent_runtime_pid, libc::SIGCHLD)?;
                        }
                    }
                    let mut payload = Map::new();
                    payload.insert(String::from("type"), Value::String(String::from("exit")));
                    payload.insert(String::from("exitCode"), Value::from(exit_code));
                    payload.insert(String::from("coreDumped"), Value::from(core_dumped));
                    if public_event_queued {
                        payload.insert(String::from("publicEventQueued"), Value::Bool(true));
                    }
                    if let Some(signal_name) = signal_name {
                        payload.insert(String::from("signal"), Value::String(signal_name));
                    }
                    if let Some(slot) = claimed_bridge_reservation.as_deref_mut() {
                        *slot = reservation.take();
                    }
                    record_execute_phase("child_process_exit_cleanup", cleanup_start.elapsed());
                    return Ok(Value::Object(payload));
                }
                ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
                    let mut current_child_path = current_process_path.to_vec();
                    current_child_path.push(child_process_id);
                    if let Some(services) = owned_javascript_services.as_deref_mut() {
                        let Some((connection_id, session_id, vm)) =
                            self.vms.get(vm_id).and_then(|state| {
                                self.vms.handle(vm_id).map(|handle| {
                                    (
                                        state.connection_id.clone(),
                                        state.session_id.clone(),
                                        handle,
                                    )
                                })
                            })
                        else {
                            return Ok(Value::Null);
                        };
                        services.push(OwnedJavascriptEventService::new(
                            OwnershipScope::vm(connection_id, session_id, vm_id),
                            vm_id.to_owned(),
                            process_id.to_owned(),
                            current_child_path
                                .iter()
                                .map(|segment| (*segment).to_owned())
                                .collect(),
                            vm,
                            request,
                            reservation.take(),
                        ));
                        return Ok(Value::Null);
                    }
                    drop(reservation);
                    let kernel_wait_request = {
                        let Some(vm) = self.vms.get(vm_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(root) = vm.active_processes.get(process_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(parent) = Self::active_process_by_path(root, current_process_path)
                        else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        deferred_kernel_wait_request_for_process(&request, &vm.kernel, child)?
                    };
                    if let Some(kernel_wait_request) = kernel_wait_request {
                        if self.service_child_kernel_wait_rpc(
                            vm_id,
                            process_id,
                            current_process_path,
                            child_process_id,
                            &kernel_wait_request,
                        )? {
                            if javascript_sync_rpc_may_make_fd_writable(&kernel_wait_request) {
                                let Some(mut vm) = self.vms.get_mut(vm_id) else {
                                    return Ok(Value::Null);
                                };
                                Self::wake_ready_deferred_fd_writes(&mut vm)?;
                            }
                            let parked = self.vms.get(vm_id).is_some_and(|vm| {
                                vm.active_processes
                                    .get(process_id)
                                    .and_then(|root| {
                                        Self::active_process_by_path(root, current_process_path)
                                    })
                                    .and_then(|parent| parent.child_processes.get(child_process_id))
                                    .and_then(|child| child.deferred_kernel_wait_rpc.as_ref())
                                    .is_some_and(|(parked, _)| parked.id == kernel_wait_request.id)
                            });
                            if parked {
                                // The execution keeps exposing the unresolved
                                // sync request until it receives a reply. Yield
                                // the sidecar actor so capacity/deadline wakes
                                // can drive the next bounded recheck.
                                return Ok(Value::Null);
                            }
                            // An immediate response may have made a following
                            // execution event available in the same turn.
                            continue;
                        }
                    }
                    if request.method == "__kernel_stdio_write" {
                        let shared_tty = {
                            let Some(mut vm) = self.vms.get_mut(vm_id) else {
                                return Ok(Value::Null);
                            };
                            let Some(root) = vm.active_processes.get_mut(process_id) else {
                                return Ok(Value::Null);
                            };
                            let Some(parent) =
                                Self::active_process_by_path_mut(root, current_process_path)
                            else {
                                return Ok(Value::Null);
                            };
                            parent
                                .child_processes
                                .get(child_process_id)
                                .and_then(|child| {
                                    child
                                        .tty_master_owner
                                        .map(|owner| (child.kernel_pid, owner))
                                })
                        };
                        if let Some((child_kernel_pid, owner)) = shared_tty {
                            let response = self.service_shared_tty_stdio_write(
                                vm_id,
                                child_kernel_pid,
                                owner,
                                &request,
                            );
                            let Some(mut vm) = self.vms.get_mut(vm_id) else {
                                return Ok(Value::Null);
                            };
                            let Some(root) = vm.active_processes.get_mut(process_id) else {
                                return Ok(Value::Null);
                            };
                            let Some(parent) =
                                Self::active_process_by_path_mut(root, current_process_path)
                            else {
                                return Ok(Value::Null);
                            };
                            let Some(child) = parent.child_processes.get_mut(child_process_id)
                            else {
                                return Ok(Value::Null);
                            };
                            match response {
                                Ok(result) => child
                                    .execution
                                    .respond_javascript_sync_rpc_response(request.id, result.into())
                                    .or_else(ignore_stale_javascript_sync_rpc_response)?,
                                Err(error) => child
                                    .execution
                                    .respond_javascript_sync_rpc_error(
                                        request.id,
                                        javascript_sync_rpc_error_code(&error),
                                        javascript_sync_rpc_error_message(&error),
                                    )
                                    .or_else(ignore_stale_javascript_sync_rpc_response)?,
                            }
                            continue;
                        }
                    }
                    let response = if request.method == "process.exec_fd_image_commit" {
                        let payload = {
                            let Some(vm) = self.vms.get(vm_id) else {
                                return Ok(Value::Null);
                            };
                            parse_javascript_child_process_spawn_request(&vm, &request.args)?.0
                        };
                        self.commit_wasm_fd_process_image(
                            vm_id,
                            process_id,
                            &current_child_path,
                            payload,
                        )?;
                        Ok(json!({ "committed": true }).into())
                    } else if request.method == "process.exec" {
                        let payload = {
                            let Some(vm) = self.vms.get(vm_id) else {
                                return Ok(Value::Null);
                            };
                            parse_javascript_child_process_spawn_request(&vm, &request.args)?.0
                        };
                        let local_replacement = payload.options.local_replacement;
                        match self.exec_javascript_process_image(
                            vm_id,
                            process_id,
                            &current_child_path,
                            payload,
                        ) {
                            Ok(()) if local_replacement => Ok(json!({ "committed": true }).into()),
                            // Separate-runtime exec destroys the old image, so
                            // no response may resume its blocked RPC call.
                            Ok(()) => return Ok(Value::Null),
                            Err(error) => Err(error),
                        }
                    } else if request.method == "process.signal_state" {
                        let (signal, registration) =
                            parse_process_signal_state_request(&request.args)
                                .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
                        let Some(mut vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        let signal_key =
                            Self::child_process_signal_key(process_id, &current_child_path)
                                .to_owned();
                        apply_process_signal_state_update(
                            &mut vm.signal_states,
                            &signal_key,
                            signal,
                            registration,
                        );
                        Ok(Value::Null.into())
                    } else if request.method == "process.kill" {
                        self.handle_descendant_process_kill_rpc(
                            vm_id,
                            process_id,
                            current_process_path,
                            child_process_id,
                            &request,
                        )
                        .map(Into::into)
                    } else if request.method.starts_with("child_process.") {
                        self.handle_descendant_javascript_child_process_rpc(
                            vm_id,
                            process_id,
                            &current_child_path,
                            &request,
                        )
                        .await
                    } else {
                        let Some(mut vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        let mut vm = &mut *vm;
                        let socket_paths = build_javascript_socket_path_context(&mut vm)?;
                        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                        let capabilities = vm.capabilities.clone();
                        let Some(root) = vm.active_processes.get_mut(process_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(parent) =
                            Self::active_process_by_path_mut(root, current_process_path)
                        else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        service_javascript_sync_rpc(JavascriptSyncRpcServiceRequest {
                            bridge: &self.bridge,
                            vm_id,
                            dns: &vm.dns,
                            socket_paths: &socket_paths,
                            kernel: &mut vm.kernel,
                            kernel_readiness,
                            process: child,
                            sync_request: &request,
                            capabilities,
                        })
                        .await
                    };

                    let response = match response {
                        Ok(JavascriptSyncRpcServiceResponse::Deferred {
                            receiver,
                            timeout,
                            task_class,
                        }) => {
                            let Some(vm) = self.vms.get(vm_id) else {
                                return Ok(Value::Null);
                            };
                            let runtime = vm.runtime_context.clone();
                            let connection_id = vm.connection_id.clone();
                            let session_id = vm.session_id.clone();
                            let sender = self.process_event_sender.clone();
                            let event_notify = Arc::clone(&self.process_event_notify);
                            let envelope_vm_id = vm_id.to_owned();
                            let envelope_process_id = process_id.to_owned();
                            let envelope_child_path = current_child_path
                                .iter()
                                .map(|segment| (*segment).to_owned())
                                .collect::<Vec<_>>();
                            let request_id = request.id;
                            let method = request.method.clone();
                            runtime
                                .spawn(task_class, async move {
                                    let receive = async {
                                        receiver.await.unwrap_or_else(|_| {
                                            Err(crate::state::DeferredRpcError {
                                                code: String::from(
                                                    "ERR_AGENTOS_DEFERRED_RPC_RESPONSE_CHANNEL_CLOSED",
                                                ),
                                                message: format!(
                                                    "deferred sync RPC response channel closed for {method}"
                                                ),
                                            })
                                        })
                                    };
                                    let result = match timeout {
                                        Some(timeout) => {
                                            match tokio::time::timeout(timeout, receive).await {
                                                Ok(result) => result,
                                                Err(_) => Err(crate::state::DeferredRpcError {
                                                    code: String::from(
                                                        "ERR_AGENTOS_DEFERRED_RPC_TIMEOUT",
                                                    ),
                                                    message: format!(
                                                        "deferred sync RPC {method} timed out after {} ms",
                                                        timeout.as_millis()
                                                    ),
                                                }),
                                            }
                                        }
                                        None => receive.await,
                                    };
                                    if sender
                                        .send(ProcessEventEnvelope {
                                            connection_id,
                                            session_id,
                                            vm_id: envelope_vm_id,
                                            child_path: envelope_child_path,
                                            process_id: envelope_process_id,
                                            event: ActiveExecutionEvent::JavascriptSyncRpcCompletion(
                                                crate::state::JavascriptSyncRpcCompletion {
                                                    request_id,
                                                    result,
                                                },
                                            ),
                                        })
                                        .await
                                        .is_err()
                                    {
                                        eprintln!(
                                            "ERR_AGENTOS_PROCESS_EVENT_CHANNEL_CLOSED: nested deferred sync RPC completion could not be delivered"
                                        );
                                    } else {
                                        event_notify.notify_one();
                                    }
                                })
                                .map_err(SidecarError::from)?;
                            continue;
                        }
                        other => other,
                    };

                    if response.is_ok() && javascript_sync_rpc_may_make_fd_readable(&request) {
                        let Some(mut vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        Self::wake_ready_deferred_fd_reads(&mut vm)?;
                    }
                    if response.is_ok() && javascript_sync_rpc_may_make_fd_writable(&request) {
                        let Some(mut vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        Self::wake_ready_deferred_fd_writes(&mut vm)?;
                    }

                    let Some(mut vm) = self.vms.get_mut(vm_id) else {
                        return Ok(Value::Null);
                    };
                    let Some(parent) = Self::descendant_parent_process_mut(
                        &mut vm,
                        process_id,
                        current_process_path,
                    ) else {
                        return Ok(Value::Null);
                    };
                    let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                        return Ok(Value::Null);
                    };
                    let parent_signal_event = response
                        .as_ref()
                        .ok()
                        .and_then(JavascriptSyncRpcServiceResponse::as_json)
                        .and_then(|result| {
                        let target_path_label =
                            Self::child_process_path_label(process_id, current_process_path);
                        if request.method != "process.kill"
                            || result.get("action").and_then(Value::as_str) != Some("user")
                            || result.get("targetProcessPath").and_then(Value::as_str)
                                != Some(target_path_label.as_str())
                        {
                            return None;
                        }
                        Some(json!({
                            "type": "signal",
                            "signal": result.get("signal").and_then(Value::as_str).unwrap_or_default(),
                            "number": result.get("number").and_then(Value::as_i64).unwrap_or_default(),
                        }))
                    });
                    match response {
                        Ok(result) => child
                            .execution
                            .respond_javascript_sync_rpc_response(request.id, result)
                            .or_else(ignore_stale_javascript_sync_rpc_response)?,
                        Err(error) => child
                            .execution
                            .respond_javascript_sync_rpc_error(
                                request.id,
                                javascript_sync_rpc_error_code(&error),
                                javascript_sync_rpc_error_message(&error),
                            )
                            .or_else(ignore_stale_javascript_sync_rpc_response)?,
                    }
                    if preserve_pull_owned_events {
                        // The WASM parent owns stream delivery, but the global
                        // process pump may service one child RPC per turn so a
                        // blocked foreground child cannot starve a background
                        // sibling that supplies its readiness transition.
                        return Ok(Value::Null);
                    }
                    if let Some(event) = parent_signal_event {
                        return Ok(event);
                    }
                }
                ActiveExecutionEvent::PythonVfsRpcRequest(request) => {
                    let Some(services) = owned_python_services.as_deref_mut() else {
                        return Err(SidecarError::InvalidState(String::from(
                            "ERR_AGENTOS_PYTHON_EVENT_OWNERSHIP: nested Python VFS RPC must be claimed by the owned process-event supervisor",
                        )));
                    };
                    let current_child_path = child_path
                        .iter()
                        .map(|segment| (*segment).to_owned())
                        .collect::<Vec<_>>();
                    let Some((connection_id, session_id, vm)) =
                        self.vms.get(vm_id).and_then(|state| {
                            self.vms.handle(vm_id).map(|handle| {
                                (
                                    state.connection_id.clone(),
                                    state.session_id.clone(),
                                    handle,
                                )
                            })
                        })
                    else {
                        return Ok(Value::Null);
                    };
                    let responder =
                        vm.try_read("claim attached Python process-event service", |state| {
                            let root = state.active_processes.get(process_id)?;
                            let path = current_child_path
                                .iter()
                                .map(String::as_str)
                                .collect::<Vec<_>>();
                            Self::active_process_by_path(root, &path)
                                .map(|process| process.execution.python_vfs_rpc_responder())
                        })?;
                    let Some(responder) = responder else {
                        return Ok(Value::Null);
                    };
                    services.push(OwnedPythonEventService::new(
                        OwnershipScope::vm(connection_id, session_id, vm_id),
                        vm_id.to_owned(),
                        process_id.to_owned(),
                        current_child_path,
                        vm,
                        responder?,
                        *request,
                        reservation.take(),
                    ));
                    return Ok(Value::Null);
                }
                ActiveExecutionEvent::PythonSocketConnectCompletion(completion) => {
                    let Some(services) = owned_python_socket_completions.as_deref_mut() else {
                        return Err(SidecarError::InvalidState(String::from(
                            "ERR_AGENTOS_PYTHON_SOCKET_COMPLETION_OWNERSHIP: nested completion must be claimed by the owned process-event supervisor",
                        )));
                    };
                    let current_child_path = child_path
                        .iter()
                        .map(|segment| (*segment).to_owned())
                        .collect::<Vec<_>>();
                    let Some((connection_id, session_id, vm)) =
                        self.vms.get(vm_id).and_then(|state| {
                            self.vms.handle(vm_id).map(|handle| {
                                (
                                    state.connection_id.clone(),
                                    state.session_id.clone(),
                                    handle,
                                )
                            })
                        })
                    else {
                        return Ok(Value::Null);
                    };
                    let responder =
                        vm.try_read("claim attached Python socket completion", |state| {
                            let root = state.active_processes.get(process_id)?;
                            let path = current_child_path
                                .iter()
                                .map(String::as_str)
                                .collect::<Vec<_>>();
                            Self::active_process_by_path(root, &path)
                                .map(|process| process.execution.python_vfs_rpc_responder())
                        })?;
                    let Some(responder) = responder else {
                        return Ok(Value::Null);
                    };
                    services.push(OwnedPythonSocketCompletionService::new(
                        OwnershipScope::vm(connection_id, session_id, vm_id),
                        vm_id.to_owned(),
                        process_id.to_owned(),
                        current_child_path,
                        vm,
                        responder?,
                        *completion,
                        reservation.take(),
                    ));
                    return Ok(Value::Null);
                }
                ActiveExecutionEvent::JavascriptSyncRpcCompletion(completion) => {
                    drop(reservation);
                    let Some(mut vm) = self.vms.get_mut(vm_id) else {
                        return Ok(Value::Null);
                    };
                    let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                    let Some(parent) = Self::descendant_parent_process_mut(
                        &mut vm,
                        process_id,
                        current_process_path,
                    ) else {
                        return Ok(Value::Null);
                    };
                    let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                        return Ok(Value::Null);
                    };
                    settle_javascript_sync_rpc_completion(
                        child,
                        &kernel_readiness,
                        completion.request_id,
                        completion.result,
                    )?;
                }
                ActiveExecutionEvent::SignalState {
                    signal,
                    registration,
                } => {
                    drop(reservation);
                    let Some(mut vm) = self.vms.get_mut(vm_id) else {
                        return Ok(Value::Null);
                    };
                    let signal_key =
                        Self::child_process_signal_key(process_id, &child_path).to_owned();
                    apply_process_signal_state_update(
                        &mut vm.signal_states,
                        &signal_key,
                        signal,
                        registration.clone(),
                    );
                    return Ok(json!({
                        "type": "signal_state",
                        "signal": signal,
                        "registration": registration,
                    }));
                }
            }
        }
    }

    fn recover_descendant_runtime_child_process_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        let (
            parent_kernel_pid,
            child_kernel_pid,
            child_runtime_pid,
            child_runtime,
            child_shared_runtime,
        ) = {
            let mut child_path = current_process_path.to_vec();
            child_path.push(child_process_id);
            let Some(mut vm) = self.vms.get_mut(vm_id) else {
                return Ok(None);
            };
            let Some(parent) =
                Self::descendant_parent_process_mut(&mut vm, process_id, current_process_path)
            else {
                return Err(javascript_child_process_gone_error(process_id, &child_path));
            };
            let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                return Err(javascript_child_process_gone_error(process_id, &child_path));
            };
            (
                parent.kernel_pid,
                child.kernel_pid,
                child.execution.child_pid(),
                child.runtime.clone(),
                child.execution.uses_shared_v8_runtime(),
            )
        };
        if child_runtime != GuestRuntimeKind::JavaScript
            && child_runtime != GuestRuntimeKind::Python
            && child_runtime != GuestRuntimeKind::WebAssembly
        {
            return Ok(None);
        }
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Ok(None);
        };
        if let Some(process_info) = vm.kernel.list_processes().get(&child_kernel_pid) {
            if process_info.status == ProcessStatus::Exited {
                return Ok(Some(ActiveExecutionEvent::Exited(
                    process_info.exit_code.unwrap_or(0),
                )));
            }
        }
        if let Some(wait_result) = vm
            .kernel
            .waitpid_with_options(
                EXECUTION_DRIVER_NAME,
                parent_kernel_pid,
                child_kernel_pid as i32,
                WaitPidFlags::WNOHANG,
            )
            .map_err(kernel_error)?
        {
            return Ok(Some(ActiveExecutionEvent::Exited(wait_result.status)));
        }

        if !child_shared_runtime && child_runtime_pid != 0 {
            match runtime_child_exit_status(child_runtime_pid)? {
                RuntimeChildStatusObservation::Exited(status) => {
                    let Some(root) = vm.active_processes.get_mut(process_id) else {
                        return Ok(None);
                    };
                    let Some(parent) = Self::active_process_by_path_mut(root, current_process_path)
                    else {
                        return Ok(None);
                    };
                    let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                        return Ok(None);
                    };
                    child.exit_signal = status.signal;
                    child.exit_core_dumped = status.core_dumped;
                    return Ok(Some(ActiveExecutionEvent::Exited(status.status)));
                }
                RuntimeChildStatusObservation::Running => {}
                RuntimeChildStatusObservation::NotWaitable => {
                    return Err(SidecarError::Execution(format!(
                        "ECHILD: guest runtime process {child_runtime_pid} exited without an observable wait status"
                    )));
                }
            }
        }
        Ok(None)
    }

    fn write_descendant_javascript_child_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        chunk: &[u8],
    ) -> Result<(), SidecarError> {
        let mut child_path = current_process_path.to_vec();
        child_path.push(child_process_id);
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let vm = &mut *vm;
        let Some(root) = vm.active_processes.get_mut(process_id) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let Some(parent) = Self::active_process_by_path_mut(root, current_process_path) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        if let Err(error) = child.execution.write_stdin(chunk) {
            if is_broken_pipe_error(&error) {
                return Ok(());
            }
            return Err(error);
        }
        write_kernel_process_stdin(&mut vm.kernel, child, chunk)
    }

    fn close_descendant_javascript_child_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let mut child_path = current_process_path.to_vec();
        child_path.push(child_process_id);
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let vm = &mut *vm;
        let Some(root) = vm.active_processes.get_mut(process_id) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let Some(parent) = Self::active_process_by_path_mut(root, current_process_path) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
            return missing_javascript_child_cleanup_result(
                parent.next_child_process_id,
                child_process_id,
                "stdin close",
            );
        };
        child.execution.close_stdin()?;
        close_kernel_process_stdin(&mut vm.kernel, child)
    }

    fn kill_descendant_javascript_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        signal: &str,
    ) -> Result<(), SidecarError> {
        let signal_name = signal.to_owned();
        let signal = parse_signal(signal)?;
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let vm = &mut *vm;
        let registration = vm
            .signal_states
            .get(child_process_id)
            .and_then(|handlers| handlers.get(&(signal as u32)))
            .cloned();
        let Some(root) = vm.active_processes.get_mut(process_id) else {
            return Ok(());
        };
        let Some(parent) = Self::active_process_by_path_mut(root, current_process_path) else {
            return Ok(());
        };
        let source_pid = parent.kernel_pid;
        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
            return Ok(());
        };
        terminate_tracked_child_process_for_signal(
            &mut vm.kernel,
            child,
            signal,
            registration.as_ref(),
        )?;
        let child_process_label = if current_process_path.is_empty() {
            child_process_id.to_owned()
        } else {
            format!("{}/{}", current_process_path.join("/"), child_process_id)
        };
        emit_security_audit_event(
            &self.bridge,
            vm_id,
            "security.process.kill",
            audit_fields([
                (String::from("source"), String::from("guest_child_process")),
                (String::from("source_pid"), source_pid.to_string()),
                (String::from("target_pid"), child.kernel_pid.to_string()),
                (String::from("process_id"), process_id.to_owned()),
                (String::from("child_process_id"), child_process_label),
                (String::from("signal"), signal_name),
            ]),
        );
        self.process_event_notify.notify_one();
        Ok(())
    }

    fn handle_descendant_process_kill_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        request: &JavascriptSyncRpcRequest,
    ) -> Result<Value, SidecarError> {
        let target_pid = javascript_sync_rpc_arg_i32(&request.args, 0, "process.kill target pid")?;
        let signal_name = javascript_sync_rpc_arg_str(&request.args, 1, "process.kill signal")?;
        let signal = parse_signal(signal_name)?;

        let mut source_path = current_process_path.to_vec();
        source_path.push(child_process_id);

        if signal != 0 && target_pid < 0 {
            let pgid = target_pid.unsigned_abs();
            let caller_kernel_pid = {
                let Some(vm) = self.vms.get(vm_id) else {
                    return Err(SidecarError::InvalidState(String::from(
                        "ESRCH: unknown VM during process.kill",
                    )));
                };
                let Some(root) = vm.active_processes.get(process_id) else {
                    return Err(SidecarError::InvalidState(format!(
                        "ESRCH: unknown process {process_id} during process.kill",
                    )));
                };
                let Some(source) = Self::active_process_by_path(root, &source_path) else {
                    return Err(SidecarError::InvalidState(format!(
                        "ESRCH: unknown child process {child_process_id} during process.kill",
                    )));
                };
                source.kernel_pid
            };
            let caller_is_member =
                self.signal_vm_process_group(vm_id, caller_kernel_pid, pgid, signal_name)?;
            if !caller_is_member {
                return Ok(Value::Null);
            }
            let Some(mut vm) = self.vms.get_mut(vm_id) else {
                return Ok(Value::Null);
            };
            let vm = &mut *vm;
            let Some(root) = vm.active_processes.get_mut(process_id) else {
                return Ok(Value::Null);
            };
            let Some(source) = Self::active_process_by_path_mut(root, &source_path) else {
                return Ok(Value::Null);
            };
            if !matches!(
                canonical_signal_name(signal),
                Some("SIGWINCH" | "SIGCHLD" | "SIGCONT" | "SIGURG")
            ) {
                apply_active_process_default_signal(&mut vm.kernel, source, signal)?;
            }
            return Ok(json!({
                "self": true,
                "action": "default",
            }));
        }

        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Err(SidecarError::InvalidState(String::from(
                "ESRCH: unknown VM during process.kill",
            )));
        };

        if signal == 0 {
            vm.kernel
                .signal_process(EXECUTION_DRIVER_NAME, target_pid, signal)
                .map_err(kernel_error)?;
            return Ok(Value::Null);
        }

        let target_kernel_pid = u32::try_from(target_pid).map_err(|_| {
            SidecarError::InvalidState(format!("EINVAL: invalid process pid {target_pid}"))
        })?;
        let (source_pid, located_target_path) = {
            let Some(root) = vm.active_processes.get(process_id) else {
                return Err(SidecarError::InvalidState(format!(
                    "ESRCH: unknown process {process_id} during process.kill",
                )));
            };
            let Some(source) = Self::active_process_by_path(root, &source_path) else {
                return Err(SidecarError::InvalidState(format!(
                    "ESRCH: unknown child process {child_process_id} during process.kill",
                )));
            };
            vm.kernel
                .signal_process(EXECUTION_DRIVER_NAME, target_pid, 0)
                .map_err(kernel_error)?;
            (
                source.kernel_pid,
                Self::active_process_path_by_kernel_pid(root, target_kernel_pid),
            )
        };
        drop(vm);
        let Some(target_path) = located_target_path else {
            // The target is alive but not part of this root's process tree.
            // Resolve it VM-wide so cross-tree pids and untracked kernel
            // processes still receive the signal.
            self.signal_vm_kernel_pid(vm_id, target_kernel_pid, signal_name)?;
            return Ok(Value::Null);
        };
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Err(SidecarError::InvalidState(String::from(
                "ESRCH: unknown VM during process.kill",
            )));
        };
        let vm = &mut *vm;

        if source_pid == target_kernel_pid {
            let Some(root) = vm.active_processes.get_mut(process_id) else {
                return Ok(Value::Null);
            };
            let Some(source) = Self::active_process_by_path_mut(root, &source_path) else {
                return Ok(Value::Null);
            };
            if !matches!(
                canonical_signal_name(signal),
                Some("SIGWINCH" | "SIGCHLD" | "SIGCONT" | "SIGURG")
            ) {
                apply_active_process_default_signal(&mut vm.kernel, source, signal)?;
            }
            return Ok(json!({
                "self": true,
                "action": "default",
            }));
        }

        let signal_key = target_path.last().map(String::as_str).unwrap_or(process_id);
        let registration = vm
            .signal_states
            .get(signal_key)
            .and_then(|handlers| handlers.get(&(signal as u32)))
            .cloned();

        let action = match registration
            .as_ref()
            .map(|registration| &registration.action)
        {
            Some(SignalDispositionAction::Ignore) => "ignore",
            Some(SignalDispositionAction::User) => {
                let Some(root) = vm.active_processes.get_mut(process_id) else {
                    return Ok(Value::Null);
                };
                let Some(target) = Self::active_process_by_owned_path_mut(root, &target_path)
                else {
                    return Err(SidecarError::InvalidState(format!(
                        "ESRCH: unknown process pid {target_pid}"
                    )));
                };
                if matches!(&target.execution, ActiveExecution::Wasm(execution) if execution.uses_shared_v8_runtime())
                {
                    target.queue_pending_wasm_signal(signal)?;
                } else if let Some(session) = target.execution.javascript_v8_session_handle().filter(
                    |_| matches!(&target.execution, ActiveExecution::Javascript(execution) if execution.uses_shared_v8_runtime()),
                ) {
                    dispatch_v8_session_signal(session, signal);
                } else if !dispatch_v8_process_signal(target, signal)? {
                    return Err(SidecarError::InvalidState(format!(
                        "unsupported guest signal delivery for pid {target_pid}"
                    )));
                }
                "user"
            }
            Some(SignalDispositionAction::Default) | None
                if matches!(
                    canonical_signal_name(signal),
                    Some("SIGWINCH" | "SIGCHLD" | "SIGURG")
                ) =>
            {
                "ignore"
            }
            Some(SignalDispositionAction::Default) | None => {
                let Some(root) = vm.active_processes.get_mut(process_id) else {
                    return Ok(Value::Null);
                };
                let Some(target) = Self::active_process_by_owned_path_mut(root, &target_path)
                else {
                    return Err(SidecarError::InvalidState(format!(
                        "ESRCH: unknown process pid {target_pid}"
                    )));
                };
                apply_active_process_default_signal(&mut vm.kernel, target, signal)?;
                "default"
            }
        };

        let target_path_label = Self::child_process_path_label(
            process_id,
            &target_path.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        emit_security_audit_event(
            &self.bridge,
            vm_id,
            "security.process.kill",
            audit_fields([
                (String::from("source"), String::from("guest_process")),
                (String::from("source_pid"), source_pid.to_string()),
                (String::from("target_pid"), target_pid.to_string()),
                (String::from("process_id"), process_id.to_owned()),
                (
                    String::from("target_process_path"),
                    target_path_label.clone(),
                ),
                (String::from("signal"), signal_name.to_owned()),
            ]),
        );

        Ok(json!({
            "self": false,
            "action": action,
            "signal": signal_name,
            "number": signal,
            "targetProcessPath": target_path_label,
        }))
    }

    pub(crate) async fn poll_javascript_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
        wait_ms: u64,
    ) -> Result<Value, SidecarError> {
        self.poll_descendant_javascript_child_process(
            vm_id,
            process_id,
            &[],
            child_process_id,
            wait_ms,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .await
    }

    pub(crate) fn write_javascript_child_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
        chunk: &[u8],
    ) -> Result<(), SidecarError> {
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Err(javascript_child_process_gone_error(
                process_id,
                &[child_process_id],
            ));
        };
        let vm = &mut *vm;
        let Some(child) = vm
            .active_processes
            .get_mut(process_id)
            .ok_or_else(|| missing_process_error(vm_id, process_id))?
            .child_processes
            .get_mut(child_process_id)
        else {
            return Err(javascript_child_process_gone_error(
                process_id,
                &[child_process_id],
            ));
        };
        if let Err(error) = child.execution.write_stdin(chunk) {
            if is_broken_pipe_error(&error) {
                return Ok(());
            }
            return Err(error);
        }
        write_kernel_process_stdin(&mut vm.kernel, child, chunk)
    }

    pub(crate) fn close_javascript_child_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Err(javascript_child_process_gone_error(
                process_id,
                &[child_process_id],
            ));
        };
        let vm = &mut *vm;
        let process = vm
            .active_processes
            .get_mut(process_id)
            .ok_or_else(|| missing_process_error(vm_id, process_id))?;
        let Some(child) = process.child_processes.get_mut(child_process_id) else {
            return missing_javascript_child_cleanup_result(
                process.next_child_process_id,
                child_process_id,
                "stdin close",
            );
        };
        child.execution.close_stdin()?;
        close_kernel_process_stdin(&mut vm.kernel, child)
    }

    pub(crate) fn kill_javascript_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
        signal: &str,
    ) -> Result<(), SidecarError> {
        let signal_name = signal.to_owned();
        let signal = parse_signal(signal)?;
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let vm = &mut *vm;
        let registration = vm
            .signal_states
            .get(child_process_id)
            .and_then(|handlers| handlers.get(&(signal as u32)))
            .cloned();
        let process = vm
            .active_processes
            .get_mut(process_id)
            .ok_or_else(|| missing_process_error(vm_id, process_id))?;
        let source_pid = process.kernel_pid;
        let Some(child) = process.child_processes.get_mut(child_process_id) else {
            // Child IDs are monotonically allocated per parent. An allocated
            // ID that is no longer present was already reaped (or rolled back),
            // so cleanup kills are idempotent without hiding never-issued IDs.
            return missing_javascript_child_cleanup_result(
                process.next_child_process_id,
                child_process_id,
                "kill",
            );
        };
        terminate_tracked_child_process_for_signal(
            &mut vm.kernel,
            child,
            signal,
            registration.as_ref(),
        )?;
        emit_security_audit_event(
            &self.bridge,
            vm_id,
            "security.process.kill",
            audit_fields([
                (String::from("source"), String::from("guest_child_process")),
                (String::from("source_pid"), source_pid.to_string()),
                (String::from("target_pid"), child.kernel_pid.to_string()),
                (String::from("process_id"), process_id.to_owned()),
                (
                    String::from("child_process_id"),
                    child_process_id.to_owned(),
                ),
                (String::from("signal"), signal_name),
            ]),
        );
        self.process_event_notify.notify_one();
        Ok(())
    }
}

#[cfg(test)]
mod child_event_claim_tests {
    use super::*;
    use crate::protocol::{RequestFrame, RequestPayload, ResponsePayload};
    use crate::request_operations::OperationCancellation;
    use crate::state::{ConnectionState, SessionState};
    use crate::stdio::LocalBridge;
    use crate::NativeSidecarConfig;

    async fn sidecar_with_test_vm(
        max_process_events: usize,
    ) -> (NativeSidecar<LocalBridge>, String) {
        sidecar_with_test_vm_and_handle_command_limit(max_process_events, None).await
    }

    async fn sidecar_with_test_vm_and_handle_command_limit(
        max_process_events: usize,
        max_handle_commands: Option<u64>,
    ) -> (NativeSidecar<LocalBridge>, String) {
        let config = NativeSidecarConfig::default();
        let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
            .expect("child claim test runtime");
        let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
            LocalBridge::default(),
            config,
            Vec::new(),
            runtime.context(),
        )
        .expect("child claim test sidecar");
        // The process runtime is a singleton and every test must construct it
        // from one identical topology. This limit belongs to the sidecar-owned
        // public event queue, so tighten the fixture after runtime acquisition.
        sidecar.config.runtime.protocol.max_process_events = max_process_events;
        let connection_id = "child-claim-connection";
        let session_id = "child-claim-session";
        sidecar.connections.insert(
            connection_id.to_owned(),
            ConnectionState {
                auth_token: String::new(),
                sessions: BTreeSet::from([session_id.to_owned()]),
            },
        );
        sidecar.sessions.insert(
            session_id.to_owned(),
            SessionState {
                connection_id: connection_id.to_owned(),
                placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                    crate::protocol::SidecarPlacementShared { pool: None },
                ),
                metadata: BTreeMap::new(),
                vm_ids: BTreeSet::new(),
            },
        );
        let mut create = crate::protocol::CreateVmRequest::legacy_test_config(
            crate::protocol::GuestRuntimeKind::JavaScript,
            std::collections::HashMap::new(),
            Default::default(),
            Some(crate::wire::PermissionsPolicy::allow_all()),
        );
        if let Some(max_handle_commands) = max_handle_commands {
            let mut vm_config: agentos_vm_config::CreateVmConfig =
                serde_json::from_str(&create.config).expect("decode child claim VM config");
            let reactor = vm_config
                .limits
                .get_or_insert_default()
                .reactor
                .get_or_insert_default();
            reactor.max_handle_commands = Some(max_handle_commands);
            reactor.per_handle_operation_quantum = Some(max_handle_commands);
            create.config =
                serde_json::to_string(&vm_config).expect("encode child claim VM config");
        }
        let request = RequestFrame::new(
            1,
            OwnershipScope::session(connection_id, session_id),
            RequestPayload::CreateVm(create),
        );
        let RequestPayload::CreateVm(payload) = request.payload.clone() else {
            unreachable!("child claim fixture request is create VM");
        };
        let dispatch = sidecar
            .create_vm(&request, payload)
            .await
            .expect("create child claim VM");
        let ResponsePayload::VmCreated(created) = dispatch.response.payload else {
            panic!("child claim VM creation returned another response");
        };
        (sidecar, created.vm_id)
    }

    fn javascript_process(
        vm_id: &str,
        vm: &mut VmState,
        label: &str,
        source: &str,
        event_notify: Arc<tokio::sync::Notify>,
    ) -> ActiveProcess {
        let kernel_handle = vm
            .kernel
            .create_virtual_process(
                EXECUTION_DRIVER_NAME,
                EXECUTION_DRIVER_NAME,
                JAVASCRIPT_COMMAND,
                vec![String::from(JAVASCRIPT_COMMAND)],
                VirtualProcessOptions {
                    env: vm.guest_env.clone(),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|error| panic!("create {label} kernel process: {error}"));
        let kernel_pid = kernel_handle.pid();
        let execution_engines = vm.execution_engines.clone();
        let runtime_context = vm.runtime_context.clone();
        let limits = javascript_execution_limits(vm);
        let guest_runtime = guest_runtime_identity(vm, Some(u64::from(kernel_pid)), None);
        let cwd = vm.host_cwd.clone();
        let mut javascript_engine = execution_engines
            .javascript("start child event claim test JavaScript")
            .expect("borrow child event claim JavaScript engine");
        let context = javascript_engine.create_context(CreateJavascriptContextRequest {
            vm_id: vm_id.to_owned(),
            bootstrap_module: None,
            compile_cache_root: None,
        });
        let context_id = context.context_id.clone();
        let execution = javascript_engine
            .start_execution_with_module_reader_and_runtime(
                StartJavascriptExecutionRequest {
                    guest_runtime,
                    vm_id: vm_id.to_owned(),
                    context_id,
                    argv: vec![String::from("-e")],
                    argv0: None,
                    env: vm.guest_env.clone(),
                    cwd,
                    limits,
                    inline_code: Some(source.to_owned()),
                    wasm_module_bytes: None,
                },
                None,
                None,
                runtime_context.clone(),
            )
            .unwrap_or_else(|error| panic!("start {label} JavaScript process: {error}"));
        javascript_engine.dispose_context(&context.context_id);
        ActiveProcess::new(
            kernel_pid,
            kernel_handle,
            runtime_context,
            vm.limits.clone(),
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::JavaScript,
            ActiveExecution::Javascript(execution),
        )
        .with_event_notify(event_notify)
    }

    fn binding_process(vm: &mut VmState, label: &str, parent_pid: Option<u32>) -> ActiveProcess {
        let kernel_handle = vm
            .kernel
            .create_virtual_process(
                EXECUTION_DRIVER_NAME,
                EXECUTION_DRIVER_NAME,
                JAVASCRIPT_COMMAND,
                vec![String::from(JAVASCRIPT_COMMAND)],
                VirtualProcessOptions {
                    parent_pid,
                    env: vm.guest_env.clone(),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|error| panic!("create {label} kernel process: {error}"));
        ActiveProcess::new(
            kernel_handle.pid(),
            kernel_handle,
            vm.runtime_context.clone(),
            vm.limits.clone(),
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::JavaScript,
            ActiveExecution::Binding(BindingExecution::default()),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wasm_child_exit_closes_kernel_writer_before_parent_poll() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let root_id = String::from("wasm-pipe-root");
                let child_id = String::from("wasm-pipe-child");
                let (root_pid, read_fd) = {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("WASM pipe VM");
                    let mut root = binding_process(&mut vm, "WASM pipe root", None);
                    root.runtime = GuestRuntimeKind::WebAssembly;
                    let root_pid = root.kernel_pid;
                    let (read_fd, write_fd) = vm
                        .kernel
                        .open_pipe(EXECUTION_DRIVER_NAME, root_pid)
                        .expect("open parent pipe");
                    let mut child =
                        binding_process(&mut vm, "WASM pipe child", Some(root.kernel_pid));
                    child.runtime = GuestRuntimeKind::WebAssembly;
                    let writer = vm
                        .kernel
                        .fd_transfer(EXECUTION_DRIVER_NAME, root_pid, write_fd)
                        .expect("transfer pipe writer to child");
                    vm.kernel
                        .fd_install_transfer_at(
                            EXECUTION_DRIVER_NAME,
                            child.kernel_pid,
                            write_fd,
                            0,
                            &writer,
                        )
                        .expect("install child pipe writer");
                    vm.kernel
                        .fd_close(EXECUTION_DRIVER_NAME, root_pid, write_fd)
                        .expect("close parent pipe writer");
                    child
                        .queue_pending_execution_event(ActiveExecutionEvent::Exited(0))
                        .expect("queue child exit");
                    root.child_processes.insert(child_id.clone(), child);
                    vm.active_processes.insert(root_id.clone(), root);
                    (root_pid, read_fd)
                };

                assert!(
                    !sidecar
                        .pump_child_process_events(&vm_id)
                        .await
                        .expect("pump WASM child exit"),
                    "the proactive pump must leave WASM child delivery pull-owned"
                );
                let mut vm = sidecar.vms.get_mut(&vm_id).expect("WASM pipe VM");
                assert_eq!(
                    vm.kernel
                        .fd_read_with_timeout_result(
                            EXECUTION_DRIVER_NAME,
                            root_pid,
                            read_fd,
                            1,
                            Some(Duration::ZERO),
                        )
                        .expect("read after child exit"),
                    None,
                    "the child exit must close the final pipe writer before child_process.poll"
                );
                let child = vm
                    .active_processes
                    .get(&root_id)
                    .and_then(|root| root.child_processes.get(&child_id))
                    .expect("child remains available to child_process.poll");
                assert!(matches!(
                    child.pending_execution_events.front(),
                    Some(ActiveExecutionEvent::Exited(0))
                ));
            })
            .await;
    }

    fn rpc(id: u64) -> ActiveExecutionEvent {
        ActiveExecutionEvent::JavascriptSyncRpcRequest(JavascriptSyncRpcRequest {
            id,
            method: String::from("fs.stat"),
            args: Vec::new(),
            raw_bytes_args: Default::default(),
        })
    }

    fn owned_rpc_target(
        sidecar: &NativeSidecar<LocalBridge>,
        vm_id: &str,
        process_id: &str,
        child_path: Vec<String>,
        request: JavascriptSyncRpcRequest,
    ) -> OwnedJavascriptEventService {
        OwnedJavascriptEventService::new(
            OwnershipScope::vm("child-claim-connection", "child-claim-session", vm_id),
            vm_id.to_owned(),
            process_id.to_owned(),
            child_path,
            sidecar.vms.handle(vm_id).expect("owned RPC VM handle"),
            request,
            None,
        )
    }

    fn signal_state_rpc(id: u64, signal: u32, action: &str) -> JavascriptSyncRpcRequest {
        JavascriptSyncRpcRequest {
            id,
            method: String::from("process.signal_state"),
            args: vec![
                Value::from(signal),
                Value::from(action),
                Value::from("[]"),
                Value::from(0),
            ],
            raw_bytes_args: Default::default(),
        }
    }

    fn bound_kernel_udp_pair(
        sidecar: &NativeSidecar<LocalBridge>,
        vm_id: &str,
        process_id: &str,
    ) -> (String, String, SocketAddr, Arc<tokio::sync::Notify>) {
        let bridge = sidecar.bridge.clone();
        let mut vm = sidecar.vms.get_mut(vm_id).expect("kernel UDP test VM");
        let socket_paths =
            build_javascript_socket_path_context(&vm).expect("kernel UDP socket path context");
        let dns = vm.dns.clone();
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let capabilities = vm.capabilities.clone();
        let VmState {
            kernel,
            active_processes,
            ..
        } = &mut *vm;
        let process = active_processes
            .get_mut(process_id)
            .expect("kernel UDP test process");

        let mut create = |id| {
            let response =
                service_javascript_dgram_sync_rpc(JavascriptDgramSyncRpcServiceRequest {
                    bridge: &bridge,
                    kernel,
                    vm_id,
                    dns: &dns,
                    socket_paths: &socket_paths,
                    process,
                    kernel_readiness: Arc::clone(&kernel_readiness),
                    sync_request: &JavascriptSyncRpcRequest {
                        id,
                        method: String::from("dgram.createSocket"),
                        args: vec![json!({ "type": "udp4" })],
                        raw_bytes_args: Default::default(),
                    },
                    capabilities: capabilities.clone(),
                })
                .expect("create kernel UDP socket");
            let JavascriptSyncRpcServiceResponse::Json(value) = response else {
                panic!("kernel UDP create returned a deferred response");
            };
            value
                .get("socketId")
                .and_then(Value::as_str)
                .expect("kernel UDP socket id")
                .to_owned()
        };
        let receiver_id = create(100);
        let sender_id = create(101);
        let pid = process.kernel_pid;
        let receiver_addr = process
            .udp_sockets
            .get_mut(&receiver_id)
            .expect("kernel UDP receiver")
            .bind(kernel, pid, Some("127.0.0.1"), 43_120, &socket_paths)
            .expect("bind kernel UDP receiver");
        process
            .udp_sockets
            .get_mut(&sender_id)
            .expect("kernel UDP sender")
            .bind(kernel, pid, Some("127.0.0.1"), 43_121, &socket_paths)
            .expect("bind kernel UDP sender");
        let notify = Arc::clone(
            &process
                .udp_sockets
                .get(&receiver_id)
                .expect("kernel UDP receiver")
                .read_event_notify,
        );
        (receiver_id, sender_id, receiver_addr, notify)
    }

    async fn consume_notify_permit(notify: &Arc<tokio::sync::Notify>) {
        let _ = tokio::time::timeout(Duration::from_millis(5), notify.notified()).await;
    }

    async fn assert_owned_deferred_tcp_connect_settles(
        child_path: Vec<String>,
        listener: Option<std::net::TcpListener>,
        port: u16,
        expect_socket: bool,
    ) {
        let (sidecar, vm_id) =
            sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS).await;
        let process_id = String::from("owned-connect-root");
        {
            let mut vm = sidecar.vms.get_mut(&vm_id).expect("owned connect VM");
            vm.create_loopback_exempt_ports.insert(port);
            let mut root = binding_process(&mut vm, "owned connect root", None);
            if let Some(child_id) = child_path.first() {
                let child =
                    binding_process(&mut vm, "owned connect descendant", Some(root.kernel_pid));
                root.child_processes.insert(child_id.clone(), child);
            }
            vm.active_processes.insert(process_id.clone(), root);
        }

        let request_id = 104;
        let vm = sidecar.vms.handle(&vm_id).expect("owned connect VM handle");
        let request = JavascriptSyncRpcRequest {
            id: request_id,
            method: String::from("net.connect"),
            args: vec![json!({ "host": "127.0.0.1", "port": port })],
            raw_bytes_args: Default::default(),
        };
        let error = service_owned_javascript_sync_rpc_request::<LocalBridge>(
            &sidecar.bridge,
            &vm_id,
            &vm,
            &process_id,
            &child_path,
            request,
        )
        .await
        .expect_err("binding fixture cannot accept a JavaScript RPC response");
        assert!(error.to_string().contains("JavaScript sync RPC"));

        vm.try_read("verify owned deferred connect settlement", |state| {
            let root = state
                .active_processes
                .get(&process_id)
                .expect("owned connect root remains active");
            let path = child_path.iter().map(String::as_str).collect::<Vec<_>>();
            let process = NativeSidecar::<LocalBridge>::active_process_by_path(root, &path)
                .expect("owned connect target remains active");
            assert!(process.pending_javascript_net_connects.is_empty());
            assert_eq!(process.tcp_sockets.len(), usize::from(expect_socket));
        })
        .expect("inspect owned deferred connect settlement");
        drop(listener);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owned_root_deferred_tcp_connect_commits_before_reply() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind owned root connect listener");
        let port = listener
            .local_addr()
            .expect("owned root listener address")
            .port();
        assert_owned_deferred_tcp_connect_settles(Vec::new(), Some(listener), port, true).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owned_descendant_deferred_tcp_connect_commits_before_reply() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind owned descendant connect listener");
        let port = listener
            .local_addr()
            .expect("owned descendant listener address")
            .port();
        assert_owned_deferred_tcp_connect_settles(
            vec![String::from("child")],
            Some(listener),
            port,
            true,
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owned_deferred_tcp_connect_error_clears_pending_state() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("reserve refused connect port");
        let port = listener
            .local_addr()
            .expect("reserved connect address")
            .port();
        drop(listener);
        assert_owned_deferred_tcp_connect_settles(Vec::new(), None, port, false).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owned_kernel_udp_poll_wakes_for_arrival_and_restores_socket() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let process_id = String::from("owned-udp-root");
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("owned UDP VM");
                    let root = binding_process(&mut vm, "owned UDP root", None);
                    vm.active_processes.insert(process_id.clone(), root);
                }
                let (receiver_id, sender_id, receiver_addr, notify) =
                    bound_kernel_udp_pair(&sidecar, &vm_id, &process_id);
                consume_notify_permit(&notify).await;

                let vm = sidecar.vms.handle(&vm_id).expect("owned UDP VM handle");
                let request = JavascriptSyncRpcRequest {
                    id: 102,
                    method: String::from("dgram.poll"),
                    args: vec![Value::from(receiver_id.clone()), Value::from(500)],
                    raw_bytes_args: Default::default(),
                };
                let mut poll = Box::pin(service_owned_javascript_sync_rpc_request::<LocalBridge>(
                    &sidecar.bridge,
                    &vm_id,
                    &vm,
                    &process_id,
                    &[],
                    request,
                ));
                let mut context = Context::from_waker(Waker::noop());
                assert!(matches!(poll.as_mut().poll(&mut context), Poll::Pending));
                assert!(!vm
                    .try_read("verify UDP socket detached while waiting", |vm| vm
                        .active_processes
                        .get(&process_id)
                        .is_some_and(|process| process
                            .udp_sockets
                            .contains_key(&receiver_id)))
                    .expect("verify UDP socket detached while waiting"));
                vm.try_command("send kernel UDP test datagram", |vm| {
                    let VmState {
                        kernel,
                        active_processes,
                        ..
                    } = vm;
                    let process = active_processes
                        .get_mut(&process_id)
                        .expect("kernel UDP sender process");
                    let sender = process
                        .udp_sockets
                        .get(&sender_id)
                        .expect("kernel UDP sender");
                    kernel
                        .socket_send_to_inet_loopback(
                            EXECUTION_DRIVER_NAME,
                            process.kernel_pid,
                            sender.kernel_socket_id.expect("sender kernel socket"),
                            agentos_kernel::socket_table::InetSocketAddress::new(
                                receiver_addr.ip().to_string(),
                                receiver_addr.port(),
                            ),
                            b"owned-wake",
                        )
                        .map(|_| ())
                        .map_err(kernel_error)
                })
                .expect("send kernel UDP test datagram");
                let error = poll
                    .await
                    .expect_err("binding test execution cannot accept JavaScript RPC replies");
                assert!(error.to_string().contains("JavaScript sync RPC"));
                assert!(vm
                    .try_read("verify restored UDP socket", |vm| vm
                        .active_processes
                        .get(&process_id)
                        .is_some_and(|process| process
                            .udp_sockets
                            .contains_key(&receiver_id)))
                    .expect("verify restored UDP socket"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_owned_udp_poll_restores_detached_socket() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let process_id = String::from("owned-udp-cancel-root");
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("owned UDP cancel VM");
                    let root = binding_process(&mut vm, "owned UDP cancel root", None);
                    vm.active_processes.insert(process_id.clone(), root);
                }
                let (receiver_id, _sender_id, _receiver_addr, notify) =
                    bound_kernel_udp_pair(&sidecar, &vm_id, &process_id);
                consume_notify_permit(&notify).await;
                let vm = sidecar
                    .vms
                    .handle(&vm_id)
                    .expect("owned UDP cancel VM handle");
                let request = JavascriptSyncRpcRequest {
                    id: 103,
                    method: String::from("dgram.poll"),
                    args: vec![Value::from(receiver_id.clone()), Value::from(500)],
                    raw_bytes_args: Default::default(),
                };
                let mut poll = Box::pin(service_owned_javascript_dgram_poll::<LocalBridge>(
                    &vm,
                    &process_id,
                    &[],
                    &request,
                ));
                let mut context = Context::from_waker(Waker::noop());
                assert!(matches!(poll.as_mut().poll(&mut context), Poll::Pending));
                assert!(!vm
                    .try_read("verify detached UDP socket", |vm| vm
                        .active_processes
                        .get(&process_id)
                        .is_some_and(|process| process
                            .udp_sockets
                            .contains_key(&receiver_id)))
                    .expect("verify detached UDP socket"));
                drop(poll);
                assert!(vm
                    .try_read("verify cancelled UDP socket restore", |vm| vm
                        .active_processes
                        .get(&process_id)
                        .is_some_and(|process| process
                            .udp_sockets
                            .contains_key(&receiver_id)))
                    .expect("verify cancelled UDP socket restore"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owned_special_dispatch_is_path_aware_for_root_and_descendant() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let root_id = String::from("owned-special-root");
                let child_id = String::from("owned-special-child");
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("owned special VM");
                    let mut root = binding_process(&mut vm, "owned special root", None);
                    let child =
                        binding_process(&mut vm, "owned special child", Some(root.kernel_pid));
                    root.child_processes.insert(child_id.clone(), child);
                    vm.active_processes.insert(root_id.clone(), root);
                }

                let root_target = owned_rpc_target(
                    &sidecar,
                    &vm_id,
                    &root_id,
                    Vec::new(),
                    signal_state_rpc(1, libc::SIGUSR1 as u32, "ignore"),
                );
                let root_service =
                    sidecar.prepare_owned_javascript_process_event_service(root_target);
                let _ = root_service.await;

                let child_target = owned_rpc_target(
                    &sidecar,
                    &vm_id,
                    &root_id,
                    vec![child_id.clone()],
                    signal_state_rpc(2, libc::SIGUSR2 as u32, "user"),
                );
                let child_service =
                    sidecar.prepare_owned_javascript_process_event_service(child_target);
                let _ = child_service.await;

                let vm = sidecar.vms.get(&vm_id).expect("owned special VM");
                assert!(vm
                    .signal_states
                    .get(&root_id)
                    .is_some_and(|handlers| handlers.contains_key(&(libc::SIGUSR1 as u32))));
                assert!(vm
                    .signal_states
                    .get(&child_id)
                    .is_some_and(|handlers| handlers.contains_key(&(libc::SIGUSR2 as u32))));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn all_root_and_descendant_special_methods_are_side_effect_free_until_polled() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let root_id = String::from("owned-special-cancel-root");
                let child_id = String::from("owned-special-cancel-child");
                let root_target_child = String::from("root-target-child");
                let descendant_target_child = String::from("descendant-target-child");
                let (root_target_pid, descendant_target_pid) = {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("special cancel VM");
                    let mut root = binding_process(&mut vm, "special cancel root", None);
                    let root_pid = root.kernel_pid;
                    let root_child =
                        binding_process(&mut vm, "root target child", Some(root.kernel_pid));
                    let mut descendant = binding_process(
                        &mut vm,
                        "special cancel descendant",
                        Some(root.kernel_pid),
                    );
                    let descendant_pid = descendant.kernel_pid;
                    let descendant_child = binding_process(
                        &mut vm,
                        "descendant target child",
                        Some(descendant.kernel_pid),
                    );
                    root.child_processes
                        .insert(root_target_child.clone(), root_child);
                    descendant
                        .child_processes
                        .insert(descendant_target_child.clone(), descendant_child);
                    root.child_processes.insert(child_id.clone(), descendant);
                    vm.active_processes.insert(root_id.clone(), root);
                    (root_pid, descendant_pid)
                };

                for (path, target_child, target_pid) in [
                    (Vec::new(), root_target_child.clone(), root_target_pid),
                    (
                        vec![child_id.clone()],
                        descendant_target_child.clone(),
                        descendant_target_pid,
                    ),
                ] {
                    let requests = vec![
                        JavascriptSyncRpcRequest {
                            id: 100,
                            method: String::from("child_process.spawn"),
                            args: vec![
                                Value::from("/bin/echo"),
                                Value::from("[]"),
                                Value::from("{}"),
                            ],
                            raw_bytes_args: Default::default(),
                        },
                        JavascriptSyncRpcRequest {
                            id: 101,
                            method: String::from("child_process.spawn_sync"),
                            args: vec![
                                Value::from("/bin/echo"),
                                Value::from("[]"),
                                Value::from("{}"),
                            ],
                            raw_bytes_args: Default::default(),
                        },
                        JavascriptSyncRpcRequest {
                            id: 102,
                            method: String::from("child_process.poll"),
                            args: vec![Value::from(target_child.clone()), Value::from(0)],
                            raw_bytes_args: Default::default(),
                        },
                        JavascriptSyncRpcRequest {
                            id: 103,
                            method: String::from("child_process.write_stdin"),
                            args: vec![
                                Value::from(target_child.clone()),
                                javascript_sync_rpc_bytes_value(b"must-not-write"),
                            ],
                            raw_bytes_args: Default::default(),
                        },
                        JavascriptSyncRpcRequest {
                            id: 104,
                            method: String::from("child_process.close_stdin"),
                            args: vec![Value::from(target_child.clone())],
                            raw_bytes_args: Default::default(),
                        },
                        JavascriptSyncRpcRequest {
                            id: 105,
                            method: String::from("child_process.kill"),
                            args: vec![Value::from(target_child.clone()), Value::from("SIGTERM")],
                            raw_bytes_args: Default::default(),
                        },
                        JavascriptSyncRpcRequest {
                            id: 106,
                            method: String::from("process.exec_fd_image_commit"),
                            args: vec![
                                Value::from("/bin/echo"),
                                Value::from("[]"),
                                Value::from("{}"),
                            ],
                            raw_bytes_args: Default::default(),
                        },
                        JavascriptSyncRpcRequest {
                            id: 107,
                            method: String::from("process.exec"),
                            args: vec![
                                Value::from("/bin/echo"),
                                Value::from("[]"),
                                Value::from("{}"),
                            ],
                            raw_bytes_args: Default::default(),
                        },
                        signal_state_rpc(108, libc::SIGUSR1 as u32, "ignore"),
                        JavascriptSyncRpcRequest {
                            id: 109,
                            method: String::from("process.kill"),
                            args: vec![Value::from(target_pid), Value::from("SIGTERM")],
                            raw_bytes_args: Default::default(),
                        },
                    ];
                    for request in requests {
                        let target =
                            owned_rpc_target(&sidecar, &vm_id, &root_id, path.clone(), request);
                        let service =
                            sidecar.prepare_owned_javascript_process_event_service(target);
                        drop(service);
                    }
                }

                let vm = sidecar.vms.get(&vm_id).expect("special cancel VM");
                assert!(vm.signal_states.is_empty());
                let root = vm
                    .active_processes
                    .get(&root_id)
                    .expect("special cancel root");
                assert!(root.child_processes.contains_key(&root_target_child));
                let descendant = root
                    .child_processes
                    .get(&child_id)
                    .expect("special cancel descendant");
                assert!(descendant
                    .child_processes
                    .contains_key(&descendant_target_child));
                assert!(vm
                    .kernel
                    .list_processes()
                    .get(&root_target_pid)
                    .is_some_and(|process| process.status != ProcessStatus::Exited));
                assert!(vm
                    .kernel
                    .list_processes()
                    .get(&descendant_target_pid)
                    .is_some_and(|process| process.status != ProcessStatus::Exited));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owned_child_poll_extracts_nested_internal_rpc_before_settling_parent() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let root_id = String::from("owned-poll-root");
                let child_id = String::from("owned-poll-child");
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("owned poll VM");
                    let mut root = binding_process(&mut vm, "owned poll root", None);
                    let mut child =
                        binding_process(&mut vm, "owned poll child", Some(root.kernel_pid));
                    child
                        .queue_pending_execution_event(
                            ActiveExecutionEvent::JavascriptSyncRpcRequest(signal_state_rpc(
                                11,
                                libc::SIGTERM as u32,
                                "ignore",
                            )),
                        )
                        .expect("queue nested child RPC");
                    root.child_processes.insert(child_id.clone(), child);
                    vm.active_processes.insert(root_id.clone(), root);
                }

                let poll_target = owned_rpc_target(
                    &sidecar,
                    &vm_id,
                    &root_id,
                    Vec::new(),
                    JavascriptSyncRpcRequest {
                        id: 12,
                        method: String::from("child_process.poll"),
                        args: vec![Value::from(child_id.clone()), Value::from(0)],
                        raw_bytes_args: Default::default(),
                    },
                );
                let service = sidecar.prepare_owned_javascript_process_event_service(poll_target);
                let _ = service.await;

                let vm = sidecar.vms.get(&vm_id).expect("owned poll VM");
                assert!(vm
                    .signal_states
                    .get(&child_id)
                    .is_some_and(|handlers| handlers.contains_key(&(libc::SIGTERM as u32))));
                let child = vm
                    .active_processes
                    .get(&root_id)
                    .and_then(|root| root.child_processes.get(&child_id))
                    .expect("owned poll child");
                assert!(
                    child.pending_execution_events.is_empty(),
                    "nested RPC must be durably claimed and serviced, not requeued"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn attached_and_detached_claims_are_exactly_once_and_capacity_bounded() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let (root_id, child_id, detached_id) = (
                    "root".to_owned(),
                    "child-1".to_owned(),
                    "detached".to_owned(),
                );
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("child claim VM");
                    let mut root = binding_process(&mut vm, "root", None);
                    let mut child = binding_process(&mut vm, "child", Some(root.kernel_pid));
                    child
                        .queue_pending_execution_event(rpc(1))
                        .expect("queue child RPC");
                    root.child_processes.insert(child_id.clone(), child);
                    let mut detached = binding_process(&mut vm, "detached", None);
                    detached
                        .queue_pending_execution_event(rpc(3))
                        .expect("queue detached RPC");
                    vm.active_processes.insert(root_id.clone(), root);
                    vm.active_processes.insert(detached_id.clone(), detached);
                    vm.detached_child_processes.insert(detached_id.clone());
                }

                let mut javascript = Vec::new();
                let mut python = Vec::new();
                let mut python_socket_completions = Vec::new();
                let mut child_bridge = Vec::new();
                sidecar
                    .pump_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("claim attached child RPC");
                assert_eq!(javascript.len(), 1);
                assert_eq!(javascript[0].process_id, root_id);
                assert_eq!(javascript[0].child_path, vec![child_id.clone()]);

                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("child claim VM");
                    vm.active_processes
                        .get_mut(&root_id)
                        .and_then(|root| root.child_processes.get_mut(&child_id))
                        .expect("attached child")
                        .queue_pending_execution_event(rpc(2))
                        .expect("queue second child RPC");
                }
                sidecar
                    .pump_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("capacity-saturated attached turn");
                assert_eq!(javascript.len(), 1, "capacity must not over-claim");

                javascript.clear();
                sidecar
                    .pump_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("claim second attached RPC");
                assert_eq!(javascript.len(), 1);
                assert_eq!(javascript[0].request.id, 2);

                javascript.clear();
                sidecar
                    .pump_detached_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("claim detached child RPC");
                assert_eq!(javascript.len(), 1);
                assert_eq!(javascript[0].process_id, detached_id);
                assert!(javascript[0].child_path.is_empty());

                javascript.clear();
                sidecar
                    .pump_detached_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("repeat detached turn");
                assert!(
                    javascript.is_empty(),
                    "a claimed event must have one consumer"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn root_public_event_rearm_reaches_terminal_after_exact_256_update_quantum() {
        tokio::task::LocalSet::new()
            .run_until(async {
                const UPDATE_COUNT: usize = 256;
                const EVENT_COUNT: usize = UPDATE_COUNT + 2;
                let (mut sidecar, vm_id) = sidecar_with_test_vm(EVENT_COUNT + 1).await;
                let root_id = String::from("root-exact-quantum");
                let process_event_notify = Arc::clone(&sidecar.process_event_notify);
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("exact-quantum VM");
                    let mut root = binding_process(&mut vm, "exact-quantum root", None)
                        .with_event_notify(Arc::clone(&process_event_notify));
                    root.pending_execution_event_count_limit = EVENT_COUNT;
                    for index in 0..UPDATE_COUNT {
                        root.queue_pending_execution_event(ActiveExecutionEvent::Stdout(
                            format!("update:{index}\n").into_bytes(),
                        ))
                        .expect("queue exact-quantum update");
                    }
                    root.queue_pending_execution_event(ActiveExecutionEvent::Stdout(
                        b"tool-result:42\n".to_vec(),
                    ))
                    .expect("queue tool result after exact quantum");
                    root.queue_pending_execution_event(ActiveExecutionEvent::Stdout(
                        b"terminal-response\n".to_vec(),
                    ))
                    .expect("queue terminal response after exact quantum");
                    vm.active_processes.insert(root_id.clone(), root);
                }

                let ownership =
                    OwnershipScope::vm("child-claim-connection", "child-claim-session", &vm_id);
                let target = crate::process_event_broker::ProcessEventTarget::new(
                    "child-claim-connection",
                    "child-claim-session",
                    &vm_id,
                    &root_id,
                )
                .expect("exact-quantum process target");
                sidecar
                    .process_event_broker
                    .claim_target(&ownership, target.clone())
                    .expect("claim exact-quantum target");
                let waiter = sidecar
                    .process_event_broker
                    .register_waiter(&ownership, target, OperationCancellation::new())
                    .expect("register exact-quantum waiter");

                // Every producer write above coalesced into one stored permit.
                // Consume it so all 258 pump turns, including the two events
                // after the exact update quantum, depend only on continuation
                // rearming by the bounded root drain.
                tokio::time::timeout(Duration::from_millis(50), process_event_notify.notified())
                    .await
                    .expect("initial coalesced root event edge");

                for index in 0..EVENT_COUNT {
                    let turn = sidecar
                        .pump_process_events_nowait(&ownership, EVENT_COUNT)
                        .expect("bounded exact-quantum root turn");
                    assert!(turn.emitted_any);
                    drop(turn);

                    let event = tokio::time::timeout(Duration::from_millis(50), waiter.next())
                        .await
                        .expect("exact-quantum broker delivery deadline")
                        .expect("exact-quantum broker delivery");
                    let ActiveExecutionEvent::Stdout(bytes) = event.event else {
                        panic!("expected exact-quantum stdout event");
                    };
                    let expected = match index {
                        0..UPDATE_COUNT => format!("update:{index}\n"),
                        UPDATE_COUNT => String::from("tool-result:42\n"),
                        _ => String::from("terminal-response\n"),
                    };
                    assert_eq!(bytes, expected.as_bytes());

                    if index + 1 < EVENT_COUNT {
                        tokio::time::timeout(
                            Duration::from_millis(50),
                            process_event_notify.notified(),
                        )
                        .await
                        .expect("remaining root source must preserve a continuation edge");
                    }
                }

                let empty = sidecar
                    .pump_process_events_nowait(&ownership, EVENT_COUNT)
                    .expect("empty exact-quantum follow-up turn");
                assert!(!empty.emitted_any);
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(10),
                        process_event_notify.notified(),
                    )
                    .await
                    .is_err(),
                    "an empty root follow-up turn must not hot-rearm itself"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn root_service_claim_capacity_rearms_without_a_new_producer_edge() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let root_id = String::from("root-capacity");
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("child claim VM");
                    let mut root = binding_process(&mut vm, "root capacity", None);
                    for request_id in 1..=3 {
                        root.queue_pending_execution_event(rpc(request_id))
                            .expect("queue root RPC");
                    }
                    vm.active_processes.insert(root_id.clone(), root);
                }

                // All three producers have already fired. Consume their one
                // coalesced permit so every later turn depends on pump rearm.
                sidecar.process_event_notify.notify_one();
                tokio::time::timeout(
                    Duration::from_millis(50),
                    sidecar.process_event_notify.notified(),
                )
                .await
                .expect("initial coalesced producer edge");
                let ownership =
                    OwnershipScope::session("child-claim-connection", "child-claim-session");

                for expected_request_id in 1..=3 {
                    let turn = sidecar
                        .pump_process_events_nowait(&ownership, 1)
                        .expect("bounded root service claim turn");
                    assert_eq!(turn.javascript_services.len(), 1);
                    assert_eq!(turn.javascript_services[0].request.id, expected_request_id);
                    drop(turn);
                    tokio::time::timeout(
                        Duration::from_millis(50),
                        sidecar.process_event_notify.notified(),
                    )
                    .await
                    .expect("capacity-limited turn must preserve a continuation edge");
                }

                let empty = sidecar
                    .pump_process_events_nowait(&ownership, 1)
                    .expect("empty follow-up turn");
                assert!(!empty.emitted_any);
                assert!(empty.javascript_services.is_empty());
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(10),
                        sidecar.process_event_notify.notified(),
                    )
                    .await
                    .is_err(),
                    "an empty turn must not hot-rearm itself"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn javascript_executor_source_rearms_after_its_coalesced_notify_is_consumed() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let root_id = String::from("root-javascript-source-rearm");
                let process_event_notify = Arc::clone(&sidecar.process_event_notify);
                consume_notify_permit(&process_event_notify).await;
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("JavaScript source VM");
                    let root = javascript_process(
                        &vm_id,
                        &mut vm,
                        "JavaScript source rearm root",
                        r#"
console.log("executor-source-one");
console.log("executor-source-two");
"#,
                        Arc::clone(&process_event_notify),
                    );
                    vm.active_processes.insert(root_id.clone(), root);
                }

                let ownership =
                    OwnershipScope::vm("child-claim-connection", "child-claim-session", &vm_id);
                let target = crate::process_event_broker::ProcessEventTarget::new(
                    "child-claim-connection",
                    "child-claim-session",
                    &vm_id,
                    &root_id,
                )
                .expect("JavaScript source target");
                sidecar
                    .process_event_broker
                    .claim_target(&ownership, target.clone())
                    .expect("claim JavaScript source target");
                let waiter = sidecar
                    .process_event_broker
                    .register_waiter(&ownership, target, OperationCancellation::new())
                    .expect("register JavaScript source waiter");

                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let complete = sidecar.vms.get(&vm_id).is_some_and(|vm| {
                            vm.active_processes.get(&root_id).is_some_and(|root| {
                                root.execution.has_exited()
                                    && root.execution.has_pending_events()
                                    && root.pending_execution_events.is_empty()
                            })
                        });
                        if complete {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                })
                .await
                .expect("JavaScript executor source fills before deadline");

                // The event bridge called notify for each source event, but the
                // broker coalesced them into one permit. Consume that sole edge
                // before polling so continuation depends on the executor's
                // receiver, not ActiveProcess's local staging deque.
                tokio::time::timeout(Duration::from_millis(50), process_event_notify.notified())
                    .await
                    .expect("coalesced JavaScript source edge");
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(10),
                        process_event_notify.notified(),
                    )
                    .await
                    .is_err(),
                    "the executor source must begin with only one coalesced permit"
                );

                let first_turn = sidecar
                    .pump_process_events_nowait(&ownership, 8)
                    .expect("first JavaScript executor-source turn");
                assert!(first_turn.emitted_any);
                drop(first_turn);
                let first = tokio::time::timeout(Duration::from_millis(50), waiter.next())
                    .await
                    .expect("first JavaScript source delivery deadline")
                    .expect("first JavaScript source delivery");
                let ActiveExecutionEvent::Stdout(first) = first.event else {
                    panic!("expected first JavaScript executor stdout event");
                };
                assert_eq!(first, b"executor-source-one\n");
                assert!(sidecar.vms.get(&vm_id).is_some_and(|vm| {
                    vm.active_processes.get(&root_id).is_some_and(|root| {
                        root.pending_execution_events.is_empty()
                            && root.execution.has_pending_events()
                    })
                }));

                tokio::time::timeout(Duration::from_millis(50), process_event_notify.notified())
                    .await
                    .expect(
                        "remaining JavaScript receiver events must restore a continuation edge",
                    );
                let second_turn = sidecar
                    .pump_process_events_nowait(&ownership, 8)
                    .expect("second JavaScript executor-source turn");
                assert!(second_turn.emitted_any);
                drop(second_turn);
                let second = tokio::time::timeout(Duration::from_millis(50), waiter.next())
                    .await
                    .expect("second JavaScript source delivery deadline")
                    .expect("second JavaScript source delivery");
                let ActiveExecutionEvent::Stdout(second) = second.event else {
                    panic!("expected second JavaScript executor stdout event");
                };
                assert_eq!(second, b"executor-source-two\n");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_child_bridge_relay_preserves_later_exit_until_release() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let root_id = String::from("root-relay-order");
                let child_id = String::from("child-relay-order");
                let relay_in_flight = {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("child relay VM");
                    let mut root = binding_process(&mut vm, "relay parent", None);
                    let mut child = binding_process(&mut vm, "relay child", Some(root.kernel_pid));
                    child
                        .queue_pending_execution_event(ActiveExecutionEvent::Exited(0))
                        .expect("queue exit behind saturated stdout relay");
                    let relay_in_flight = Arc::clone(&child.child_bridge_relay_in_flight);
                    root.child_processes.insert(child_id.clone(), child);
                    vm.active_processes.insert(root_id.clone(), root);
                    relay_in_flight
                };

                // Model a stdout event that already left the child queue after
                // the parent V8 command lane reported capacity. Its owned
                // relay keeps the exact payload while the source exit remains
                // durably queued behind it.
                let relay_notify = Arc::new(tokio::sync::Notify::new());
                let relay = ChildBridgeRelayLease::acquire(
                    Arc::clone(&relay_in_flight),
                    Arc::clone(&relay_notify),
                    &child_id,
                )
                .expect("acquire saturated stdout relay");
                let target = OwnedChildBridgeEventService {
                    ownership: OwnershipScope::vm(
                        "child-claim-connection",
                        "child-claim-session",
                        &vm_id,
                    ),
                    vm_id: vm_id.clone(),
                    root_process_id: root_id.clone(),
                    parent_path: Vec::new(),
                    event_type: "child_stdout",
                    payload: json!({
                        "sessionId": child_id,
                        "dataBase64": "b3JkZXJlZC1zdGRvdXQ=",
                    }),
                    vm: sidecar.vms.handle(&vm_id).expect("child relay VM handle"),
                    deadline: Instant::now() + Duration::from_secs(1),
                    _relay: Some(relay),
                    _reservation: None,
                };

                let mut javascript = Vec::new();
                let mut python = Vec::new();
                let mut python_socket_completions = Vec::new();
                let mut child_bridge = Vec::new();
                assert!(
                    !sidecar
                        .pump_child_process_events_nowait(
                            &vm_id,
                            &mut javascript,
                            &mut python,
                            &mut python_socket_completions,
                            &mut child_bridge,
                            256,
                        )
                        .expect("blocked relay pump turn"),
                    "exit must not overtake the owned stdout relay"
                );
                assert!(
                    sidecar.vms.get(&vm_id).is_some_and(|vm| {
                        vm.active_processes
                            .get(&root_id)
                            .and_then(|root| root.child_processes.get(&child_id))
                            .is_some_and(|child| {
                                matches!(
                                    child.pending_execution_events.front(),
                                    Some(ActiveExecutionEvent::Exited(0))
                                )
                            })
                    }),
                    "the source exit must remain durable while stdout retries"
                );

                drop(target);
                tokio::time::timeout(Duration::from_millis(50), relay_notify.notified())
                    .await
                    .expect("relay release must rearm the process pump");
                assert!(!relay_in_flight.load(Ordering::Acquire));

                let error = sidecar
                    .pump_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        256,
                    )
                    .expect_err("binding parent cannot accept the released V8 exit event");
                assert!(
                    error.to_string().contains(
                        "only embedded V8 executions can receive JavaScript stream events"
                    ),
                    "unexpected released exit error: {error}"
                );
                assert!(
                    sidecar.vms.get(&vm_id).is_some_and(|vm| {
                        vm.active_processes
                            .get(&root_id)
                            .is_some_and(|root| !root.child_processes.contains_key(&child_id))
                    }),
                    "the exit may be claimed only after the stdout relay releases"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_production_child_relay_preserves_v8_stream_lifecycle_order() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) = sidecar_with_test_vm_and_handle_command_limit(
                    agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
                    Some(1),
                )
                .await;
                let root_id = String::from("root-production-relay-order");
                let child_id = String::from("child-production-relay-order");
                let process_event_notify = Arc::clone(&sidecar.process_event_notify);
                consume_notify_permit(&process_event_notify).await;
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("production relay VM");
                    let root = javascript_process(
                        &vm_id,
                        &mut vm,
                        "production relay parent",
                        r#"
const { spawn } = require("node:child_process");

const events = [];
const child = spawn("/synthetic-production-relay-child", []);
child.stdout.on("data", (chunk) => events.push(`stdout:${String(chunk)}`));
child.stdout.on("end", () => events.push("end"));
child.on("exit", () => events.push("exit"));
child.on("close", () => {
  events.push("close");
  setTimeout(() => console.log(`relay-order:${JSON.stringify(events)}`), 0);
});
console.log("relay-ready");
"#,
                        Arc::clone(&process_event_notify),
                    );
                    vm.active_processes.insert(root_id.clone(), root);
                }

                let ownership =
                    OwnershipScope::vm("child-claim-connection", "child-claim-session", &vm_id);
                let target = crate::process_event_broker::ProcessEventTarget::new(
                    "child-claim-connection",
                    "child-claim-session",
                    &vm_id,
                    &root_id,
                )
                .expect("production relay target");
                sidecar
                    .process_event_broker
                    .claim_target(&ownership, target.clone())
                    .expect("claim production relay target");
                let waiter = sidecar
                    .process_event_broker
                    .register_waiter(&ownership, target, OperationCancellation::new())
                    .expect("register production relay waiter");

                let spawn_target = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let _ = tokio::time::timeout(
                            Duration::from_millis(20),
                            process_event_notify.notified(),
                        )
                        .await;
                        let mut turn = sidecar
                            .pump_process_events_nowait(&ownership, 8)
                            .expect("claim production child spawn");
                        assert!(turn.child_bridge_services.is_empty());
                        if let Some(target) = turn.javascript_services.pop() {
                            break target;
                        }
                    }
                })
                .await
                .expect("production child spawn request deadline");
                assert_eq!(spawn_target.request.method, "child_process.spawn");
                let spawn_request_id = spawn_target.request.id;
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("production relay VM");
                    let parent_pid = vm
                        .active_processes
                        .get(&root_id)
                        .expect("production relay parent")
                        .kernel_pid;
                    let child =
                        binding_process(&mut vm, "production relay child", Some(parent_pid))
                            .with_event_notify(Arc::clone(&process_event_notify));
                    let child_pid = child.kernel_pid;
                    let root = vm
                        .active_processes
                        .get_mut(&root_id)
                        .expect("production relay parent");
                    assert!(root
                        .execution
                        .claim_javascript_sync_rpc_response(spawn_request_id)
                        .expect("claim production child spawn response"));
                    root.child_processes.insert(child_id.clone(), child);
                    root.execution
                        .respond_claimed_javascript_sync_rpc_success(
                            spawn_request_id,
                            json!({ "childId": child_id, "pid": child_pid }),
                        )
                        .expect("respond to production child spawn");
                }
                drop(spawn_target);

                let ready = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let _ = tokio::time::timeout(
                            Duration::from_millis(20),
                            process_event_notify.notified(),
                        )
                        .await;
                        let turn = sidecar
                            .pump_process_events_nowait(&ownership, 8)
                            .expect("pump production relay parent ready event");
                        assert!(turn.javascript_services.is_empty());
                        drop(turn);
                        if let Ok(Ok(event)) =
                            tokio::time::timeout(Duration::from_millis(5), waiter.next()).await
                        {
                            if let ActiveExecutionEvent::Stdout(bytes) = event.event {
                                break bytes;
                            }
                        }
                    }
                })
                .await
                .expect("production relay parent ready deadline");
                assert_eq!(ready, b"relay-ready\n");

                {
                    let vm = sidecar.vms.get(&vm_id).expect("production relay VM");
                    let root = vm
                        .active_processes
                        .get(&root_id)
                        .expect("production relay parent");
                    root.execution
                        .pause()
                        .expect("pause production relay parent");
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
                {
                    let vm = sidecar.vms.get(&vm_id).expect("production relay VM");
                    let root = vm
                        .active_processes
                        .get(&root_id)
                        .expect("production relay parent");
                    root.execution
                        .send_javascript_stream_event("relay_capacity_filler", Value::Null)
                        .expect("fill one-slot production parent command lane");
                    let full = root
                        .execution
                        .send_javascript_stream_event("relay_capacity_probe", Value::Null)
                        .expect_err("paused one-slot parent command lane must be full");
                    assert!(full
                        .to_string()
                        .contains("ERR_AGENTOS_SESSION_COMMAND_LIMIT"));
                }
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("production relay VM");
                    let child = vm
                        .active_processes
                        .get_mut(&root_id)
                        .and_then(|root| root.child_processes.get_mut(&child_id))
                        .expect("production relay child");
                    child
                        .queue_pending_execution_event(ActiveExecutionEvent::Stdout(
                            b"ordered-stdout".to_vec(),
                        ))
                        .expect("queue production relay stdout");
                    child
                        .queue_pending_execution_event(ActiveExecutionEvent::Exited(0))
                        .expect("queue production relay exit");
                }

                let mut saturated_turn = sidecar
                    .pump_process_events_nowait(&ownership, 8)
                    .expect("pump saturated production child stdout");
                assert_eq!(saturated_turn.child_bridge_services.len(), 1);
                let stdout_relay = saturated_turn.child_bridge_services.pop().unwrap();
                assert_eq!(stdout_relay.event_type, "child_stdout");
                assert!(sidecar.vms.get(&vm_id).is_some_and(|vm| {
                    vm.active_processes
                        .get(&root_id)
                        .and_then(|root| root.child_processes.get(&child_id))
                        .is_some_and(|child| {
                            child.child_bridge_relay_in_flight.load(Ordering::Acquire)
                                && matches!(
                                    child.pending_execution_events.front(),
                                    Some(ActiveExecutionEvent::Exited(0))
                                )
                        })
                }));

                let stdout_delivery =
                    tokio::task::spawn_local(service_owned_child_bridge_event(stdout_relay));
                tokio::task::yield_now().await;
                assert!(!stdout_delivery.is_finished());
                {
                    let vm = sidecar.vms.get(&vm_id).expect("production relay VM");
                    vm.active_processes
                        .get(&root_id)
                        .expect("production relay parent")
                        .execution
                        .resume()
                        .expect("resume production relay parent");
                }
                tokio::time::timeout(Duration::from_secs(2), stdout_delivery)
                    .await
                    .expect("production stdout relay deadline")
                    .expect("join production stdout relay")
                    .expect("deliver production stdout relay");

                let mut exit_turn = sidecar
                    .pump_process_events_nowait(&ownership, 8)
                    .expect("pump production child exit after stdout relay");
                assert!(exit_turn.javascript_services.is_empty());
                if let Some(exit_relay) = exit_turn.child_bridge_services.pop() {
                    assert_eq!(exit_relay.event_type, "child_exit");
                    tokio::time::timeout(
                        Duration::from_secs(2),
                        service_owned_child_bridge_event(exit_relay),
                    )
                    .await
                    .expect("production exit relay deadline")
                    .expect("deliver production exit relay");
                }

                let report = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let _ = tokio::time::timeout(
                            Duration::from_millis(20),
                            process_event_notify.notified(),
                        )
                        .await;
                        let turn = sidecar
                            .pump_process_events_nowait(&ownership, 8)
                            .expect("pump production relay order report");
                        assert!(turn.javascript_services.is_empty());
                        assert!(turn.child_bridge_services.is_empty());
                        drop(turn);
                        if let Ok(Ok(event)) =
                            tokio::time::timeout(Duration::from_millis(5), waiter.next()).await
                        {
                            if let ActiveExecutionEvent::Stdout(bytes) = event.event {
                                break bytes;
                            }
                        }
                    }
                })
                .await
                .expect("production relay order report deadline");
                assert_eq!(
                    report,
                    b"relay-order:[\"stdout:ordered-stdout\",\"end\",\"exit\",\"close\"]\n"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn attached_and_detached_output_exit_progress_and_bridge_failure_are_observed() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) =
                    sidecar_with_test_vm(agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS)
                        .await;
                let root_id = String::from("root-output");
                let child_id = String::from("child-output");
                let (sync_tx, sync_rx) = tokio::sync::oneshot::channel();
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("child claim VM");
                    let mut root = binding_process(&mut vm, "root output", None);
                    let mut child = binding_process(&mut vm, "child output", Some(root.kernel_pid));
                    let child_pid = child.kernel_pid;
                    child
                        .queue_pending_execution_event(ActiveExecutionEvent::Stdout(
                            b"attached-output".to_vec(),
                        ))
                        .expect("queue attached output");
                    child
                        .queue_pending_execution_event(ActiveExecutionEvent::Exited(0))
                        .expect("queue attached exit");
                    root.pending_child_process_sync.insert(
                        child_id.clone(),
                        PendingChildProcessSync {
                            pid: child_pid,
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                            max_buffer: 1024,
                            deadline: None,
                            timeout_signal: String::from("SIGTERM"),
                            kill_sent: false,
                            timed_out: false,
                            max_buffer_exceeded: false,
                            completion: PendingChildProcessSyncCompletion::Javascript(sync_tx),
                        },
                    );
                    root.child_processes.insert(child_id.clone(), child);
                    vm.active_processes.insert(root_id.clone(), root);
                }

                let mut javascript = Vec::new();
                let mut python = Vec::new();
                let mut python_socket_completions = Vec::new();
                let mut child_bridge = Vec::new();
                assert!(sidecar
                    .pump_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("pump attached output and exit"));
                let sync_result = sync_rx
                    .await
                    .expect("attached sync completion channel")
                    .expect("attached sync completion result");
                assert_eq!(
                    sync_result.get("stdout").and_then(Value::as_str),
                    Some("attached-output")
                );
                assert_eq!(sync_result.get("code").and_then(Value::as_i64), Some(0));
                assert!(
                    sidecar
                        .vms
                        .get(&vm_id)
                        .and_then(|vm| {
                            vm.active_processes
                                .get(&root_id)
                                .map(|root| !root.child_processes.contains_key(&child_id))
                        })
                        .unwrap_or(false),
                    "attached child cleanup must complete with its exit delivery"
                );

                let detached_id = String::from("detached-output");
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("child claim VM");
                    let mut detached =
                        binding_process(&mut vm, "detached output", None).with_detached(true);
                    detached
                        .queue_pending_execution_event(ActiveExecutionEvent::Stdout(
                            b"detached-output".to_vec(),
                        ))
                        .expect("queue detached output");
                    detached
                        .queue_pending_execution_event(ActiveExecutionEvent::Exited(7))
                        .expect("queue detached exit");
                    vm.active_processes.insert(detached_id.clone(), detached);
                    vm.detached_child_processes.insert(detached_id.clone());
                }
                assert!(sidecar
                    .pump_detached_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("pump detached output and exit"));
                let detached_events = sidecar
                    .pending_process_events
                    .iter()
                    .filter(|envelope| envelope.process_id == detached_id)
                    .map(|envelope| &envelope.event)
                    .collect::<Vec<_>>();
                assert!(matches!(
                    detached_events.as_slice(),
                    [ActiveExecutionEvent::Stdout(bytes), ActiveExecutionEvent::Exited(7)]
                        if bytes == b"detached-output"
                ));
                assert!(
                    sidecar
                        .vms
                        .get(&vm_id)
                        .is_some_and(|vm| !vm.detached_child_processes.contains(&detached_id)),
                    "detached membership is removed only after its exit is durably queued"
                );

                let failure_root_id = String::from("root-bridge-failure");
                let failure_child_id = String::from("child-bridge-failure");
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("child claim VM");
                    let mut root = binding_process(&mut vm, "failure root", None);
                    let mut child =
                        binding_process(&mut vm, "failure child", Some(root.kernel_pid));
                    child
                        .queue_pending_execution_event(ActiveExecutionEvent::Stdout(vec![1]))
                        .expect("queue failing child bridge event");
                    root.child_processes.insert(failure_child_id.clone(), child);
                    vm.active_processes.insert(failure_root_id, root);
                }
                let error = sidecar
                    .pump_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect_err("unsupported parent bridge delivery must be observed");
                assert!(
                    error.to_string().contains(
                        "only embedded V8 executions can receive JavaScript stream events"
                    ),
                    "unexpected observed bridge error: {error}"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detached_descendant_exit_waits_for_public_event_capacity_before_cleanup() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut sidecar, vm_id) = sidecar_with_test_vm(1).await;
                let root_id = String::from("root");
                let child_id = String::from("detached-child");
                let detached_id = format!("{root_id}/{child_id}");
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("child claim VM");
                    let mut root = binding_process(&mut vm, "root", None);
                    let mut child =
                        binding_process(&mut vm, "detached child", Some(root.kernel_pid))
                            .with_detached(true);
                    child
                        .queue_pending_execution_event(ActiveExecutionEvent::Stdout(
                            b"ordered-before-exit".to_vec(),
                        ))
                        .expect("queue detached descendant stdout");
                    child
                        .queue_pending_execution_event(ActiveExecutionEvent::Exited(0))
                        .expect("queue detached descendant exit");
                    root.child_processes.insert(child_id.clone(), child);
                    vm.active_processes.insert(root_id.clone(), root);
                    vm.detached_child_processes.insert(detached_id.clone());
                }

                sidecar
                    .queue_pending_process_event(ProcessEventEnvelope {
                        connection_id: String::from("child-claim-connection"),
                        session_id: String::from("child-claim-session"),
                        vm_id: vm_id.clone(),
                        process_id: String::from("capacity-blocker"),
                        child_path: Vec::new(),
                        event: ActiveExecutionEvent::Stdout(vec![1]),
                    })
                    .expect("fill public event capacity");

                let mut javascript = Vec::new();
                let mut python = Vec::new();
                let mut python_socket_completions = Vec::new();
                let mut child_bridge = Vec::new();
                sidecar
                    .pump_detached_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("backpressured detached descendant turn");
                {
                    let vm = sidecar.vms.get(&vm_id).expect("child claim VM");
                    assert!(vm.detached_child_processes.contains(&detached_id));
                    let child = vm
                        .active_processes
                        .get(&root_id)
                        .and_then(|root| root.child_processes.get(&child_id))
                        .expect("exit cleanup must wait for public capacity");
                    assert!(matches!(
                        child.pending_execution_events.front(),
                        Some(ActiveExecutionEvent::Stdout(bytes))
                            if bytes == b"ordered-before-exit"
                    ));
                    assert!(matches!(
                        child.pending_execution_events.back(),
                        Some(ActiveExecutionEvent::Exited(0))
                    ));
                }

                let blocker = sidecar
                    .pending_process_events
                    .pop_front()
                    .expect("remove capacity blocker");
                assert_eq!(blocker.process_id, "capacity-blocker");
                sidecar.observe_pending_process_event_depth();

                sidecar
                    .pump_detached_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("detached descendant stdout turn after capacity returns");
                assert!(matches!(
                    sidecar.pending_process_events.front(),
                    Some(ProcessEventEnvelope {
                        process_id,
                        event: ActiveExecutionEvent::Stdout(bytes),
                        ..
                    }) if process_id == &detached_id && bytes == b"ordered-before-exit"
                ));
                {
                    let vm = sidecar.vms.get(&vm_id).expect("child claim VM");
                    assert!(vm.detached_child_processes.contains(&detached_id));
                    assert!(vm
                        .active_processes
                        .get(&root_id)
                        .is_some_and(|root| root.child_processes.contains_key(&child_id)));
                }

                let stdout = sidecar
                    .pending_process_events
                    .pop_front()
                    .expect("remove ordered detached stdout");
                assert!(matches!(stdout.event, ActiveExecutionEvent::Stdout(_)));
                sidecar.observe_pending_process_event_depth();

                sidecar
                    .pump_detached_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("detached descendant exit turn after capacity returns");
                let vm = sidecar.vms.get(&vm_id).expect("child claim VM");
                assert!(!vm.detached_child_processes.contains(&detached_id));
                assert!(
                    vm.active_processes
                        .get(&root_id)
                        .is_some_and(|root| !root.child_processes.contains_key(&child_id)),
                    "child cleanup should complete after the exit is durably queued"
                );
                drop(vm);
                assert!(matches!(
                    sidecar.pending_process_events.front(),
                    Some(ProcessEventEnvelope {
                        process_id,
                        event: ActiveExecutionEvent::Exited(0),
                        ..
                    }) if process_id == &detached_id
                ));
                assert_eq!(sidecar.pending_process_events.len(), 1);
                assert!(!sidecar
                    .pump_detached_child_process_events_nowait(
                        &vm_id,
                        &mut javascript,
                        &mut python,
                        &mut python_socket_completions,
                        &mut child_bridge,
                        1,
                    )
                    .expect("post-exit detached descendant turn"));
                assert_eq!(
                    sidecar.pending_process_events.len(),
                    1,
                    "durable detached exit must not be queued twice"
                );
            })
            .await;
    }
}
