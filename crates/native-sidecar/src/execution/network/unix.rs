use super::super::*;
use crate::state::SocketFairnessRetirement;

#[cfg(not(target_os = "linux"))]
fn abstract_unix_unsupported() -> SidecarError {
    sidecar_net_error(std::io::Error::from_raw_os_error(libc::ENOTSUP))
}

pub(in crate::execution) fn decode_abstract_unix_name(hex: &str) -> Result<Vec<u8>, SidecarError> {
    if !hex.len().is_multiple_of(2)
        || hex.len() > 214
        || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(SidecarError::InvalidState(String::from(
            "abstract Unix socket names must be at most 107 bytes of hexadecimal data",
        )));
    }
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).expect("validated hex digit");
            let low = (pair[1] as char).to_digit(16).expect("validated hex digit");
            Ok(((high << 4) | low) as u8)
        })
        .collect()
}

pub(in crate::execution) fn abstract_unix_node_path(name: &[u8]) -> String {
    let mut path = String::with_capacity(1 + name.len());
    path.push('\0');
    path.push_str(&String::from_utf8_lossy(name));
    path
}

pub(in crate::execution) fn abstract_unix_name_hex(name: &[u8]) -> String {
    let mut hex = String::with_capacity(name.len() * 2);
    for byte in name {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    hex
}

pub(in crate::execution) fn abstract_unix_host_address_key(name: &[u8]) -> String {
    format!("abstract:{}", abstract_unix_name_hex(name))
}

pub(in crate::execution) fn pathname_unix_host_address_key(path: &Path) -> String {
    format!("pathname:{}", path.to_string_lossy())
}

fn unix_host_address_key(address: &UnixSocketAddr) -> Option<String> {
    if let Some(path) = address.as_pathname() {
        return Some(pathname_unix_host_address_key(path));
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::net::SocketAddrExt;
        if let Some(name) = address.as_abstract_name() {
            return Some(abstract_unix_host_address_key(name));
        }
    }
    None
}

static NEXT_GUEST_UNIX_BINDING_GENERATION: AtomicU64 = AtomicU64::new(1);

pub(in crate::execution) fn guest_unix_binding_id(kernel_pid: u32, local_id: &str) -> String {
    format!("{kernel_pid}:{local_id}")
}

pub(in crate::execution) fn register_guest_unix_binding(
    registry: &GuestUnixAddressRegistry,
    binding_id: &str,
    host_address_key: &str,
    address: GuestUnixAddress,
    guest_device_inode: Option<(u64, u64)>,
    host_path: Option<PathBuf>,
) -> Result<(), SidecarError> {
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    if registry.contains_key(binding_id) {
        return Err(SidecarError::InvalidState(format!(
            "duplicate Unix binding id {binding_id}"
        )));
    }
    registry.insert(
        binding_id.to_owned(),
        GuestUnixAddressRegistryEntry {
            host_address_key: host_address_key.to_owned(),
            address,
            guest_device_inode,
            host_path,
            generation: NEXT_GUEST_UNIX_BINDING_GENERATION.fetch_add(1, Ordering::Relaxed),
            active_bindings: 1,
            queued_by_target: BTreeMap::new(),
            // A bound-but-not-yet-listening socket cannot accept peers. The
            // listener path installs its configured bounded capacity before
            // starting the acceptor.
            pending_connection_limit: 1,
            pending_connections: VecDeque::new(),
        },
    );
    Ok(())
}

fn set_guest_unix_pending_connection_limit(
    registry: &GuestUnixAddressRegistry,
    binding_id: &str,
    maximum: usize,
) -> Result<(), SidecarError> {
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    let entry = registry.get_mut(binding_id).ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "missing bound Unix address metadata for {binding_id}"
        ))
    })?;
    entry.pending_connection_limit = maximum.max(1);
    Ok(())
}

pub(in crate::execution) fn guest_unix_path_target(
    context: &SocketPathContext,
    guest_device_inode: (u64, u64),
) -> Result<Option<(PathBuf, String, GuestUnixAddress)>, SidecarError> {
    let registry = context
        .unix_bound_addresses
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    Ok(registry.iter().find_map(|(binding_id, entry)| {
        (entry.active_bindings > 0 && entry.guest_device_inode == Some(guest_device_inode)).then(
            || {
                entry
                    .host_path
                    .clone()
                    .map(|path| (path, binding_id.clone(), entry.address.clone()))
            },
        )?
    }))
}

fn guest_unix_address_for_host_key(
    registry: &GuestUnixAddressRegistry,
    host_address_key: &str,
) -> Result<Option<GuestUnixAddress>, SidecarError> {
    let registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    Ok(registry
        .values()
        .filter(|entry| entry.host_address_key == host_address_key)
        .max_by_key(|entry| entry.generation)
        .map(|entry| entry.address.clone()))
}

pub(in crate::execution) fn guest_unix_binding_for_host_key(
    registry: &GuestUnixAddressRegistry,
    host_address_key: &str,
) -> Result<Option<(String, GuestUnixAddress)>, SidecarError> {
    let registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    Ok(registry
        .iter()
        .filter(|(_, entry)| {
            entry.active_bindings > 0 && entry.host_address_key == host_address_key
        })
        .max_by_key(|(_, entry)| entry.generation)
        .map(|(binding_id, entry)| (binding_id.clone(), entry.address.clone())))
}

pub(in crate::execution) fn queue_guest_unix_peer(
    registry: &GuestUnixAddressRegistry,
    source_binding_id: &str,
    target_binding_id: &str,
) -> Result<(), SidecarError> {
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    let entry = registry.get_mut(source_binding_id).ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "missing bound Unix address metadata for {source_binding_id}"
        ))
    })?;
    let queued = entry
        .queued_by_target
        .entry(target_binding_id.to_owned())
        .or_default();
    *queued = queued.saturating_add(1);
    Ok(())
}

pub(in crate::execution) fn register_guest_unix_connection(
    registry: &GuestUnixAddressRegistry,
    target_binding_id: &str,
) -> Result<Arc<GuestUnixConnectionState>, SidecarError> {
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    let target = registry.get_mut(target_binding_id).ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "missing target Unix address metadata for {target_binding_id}"
        ))
    })?;
    if target.pending_connections.len() >= target.pending_connection_limit {
        return Err(SidecarError::host(
            "EAGAIN",
            format!(
                "Unix listener pending connection limit is {}; raise the listen backlog or limits.reactor.maxAsyncCompletions",
                target.pending_connection_limit
            ),
        ));
    }
    let state = Arc::new(GuestUnixConnectionState {
        accepted_peer_open: AtomicBool::new(true),
    });
    target.pending_connections.push_back(Arc::clone(&state));
    Ok(state)
}

fn rollback_guest_unix_connection(
    registry: &GuestUnixAddressRegistry,
    target_binding_id: &str,
    state: &Arc<GuestUnixConnectionState>,
) -> Result<(), SidecarError> {
    state.accepted_peer_open.store(false, Ordering::SeqCst);
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    let Some(target) = registry.get_mut(target_binding_id) else {
        return Ok(());
    };
    if let Some(index) = target
        .pending_connections
        .iter()
        .position(|pending| Arc::ptr_eq(pending, state))
    {
        target.pending_connections.remove(index);
    }
    Ok(())
}

fn rollback_guest_unix_peer(
    registry: &GuestUnixAddressRegistry,
    source_binding_id: &str,
    target_binding_id: &str,
) -> Result<(), SidecarError> {
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    let remove_source = if let Some(source) = registry.get_mut(source_binding_id) {
        if let Some(queued) = source.queued_by_target.get_mut(target_binding_id) {
            *queued = queued.saturating_sub(1);
            if *queued == 0 {
                source.queued_by_target.remove(target_binding_id);
            }
        }
        source.active_bindings == 0 && source.queued_by_target.is_empty()
    } else {
        false
    };
    if remove_source {
        registry.remove(source_binding_id);
    }
    Ok(())
}

/// Reserves the cross-endpoint metadata before the nonblocking OS connect is
/// started. The listener reactor may accept as soon as the kernel completes
/// connect, before the connector task is scheduled again, so registering this
/// state after `connect().await` is inherently racy.
struct PendingGuestUnixConnectMetadata {
    registry: GuestUnixAddressRegistry,
    source_binding_id: Option<String>,
    target_binding_id: String,
    connection_state: Arc<GuestUnixConnectionState>,
    armed: bool,
}

impl PendingGuestUnixConnectMetadata {
    fn register(
        registry: GuestUnixAddressRegistry,
        source_binding_id: Option<String>,
        target_binding_id: String,
    ) -> Result<Self, SidecarError> {
        let connection_state = register_guest_unix_connection(&registry, &target_binding_id)?;
        if let Some(source_binding_id) = source_binding_id.as_deref() {
            if let Err(error) =
                queue_guest_unix_peer(&registry, source_binding_id, &target_binding_id)
            {
                rollback_guest_unix_connection(&registry, &target_binding_id, &connection_state)?;
                return Err(error);
            }
        }
        Ok(Self {
            registry,
            source_binding_id,
            target_binding_id,
            connection_state,
            armed: true,
        })
    }

    fn commit(mut self) -> Arc<GuestUnixConnectionState> {
        self.armed = false;
        Arc::clone(&self.connection_state)
    }
}

impl Drop for PendingGuestUnixConnectMetadata {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(source_binding_id) = self.source_binding_id.as_deref() {
            if let Err(error) =
                rollback_guest_unix_peer(&self.registry, source_binding_id, &self.target_binding_id)
            {
                eprintln!(
                    "ERR_AGENTOS_UNIX_CONNECT_METADATA_ROLLBACK: failed to roll back peer metadata: {error}"
                );
            }
        }
        if let Err(error) = rollback_guest_unix_connection(
            &self.registry,
            &self.target_binding_id,
            &self.connection_state,
        ) {
            eprintln!(
                "ERR_AGENTOS_UNIX_CONNECT_METADATA_ROLLBACK: failed to roll back connection metadata: {error}"
            );
        }
    }
}

fn accept_guest_unix_connection(
    registry: &GuestUnixAddressRegistry,
    target_binding_id: &str,
) -> Result<Arc<GuestUnixConnectionState>, SidecarError> {
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    registry
        .get_mut(target_binding_id)
        .and_then(|target| target.pending_connections.pop_front())
        .ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "missing pending Unix connection metadata for {target_binding_id}"
            ))
        })
}

pub(in crate::execution) fn close_pending_guest_unix_connections(
    registry: &GuestUnixAddressRegistry,
    target_binding_id: &str,
) -> Result<(), SidecarError> {
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    if let Some(target) = registry.get_mut(target_binding_id) {
        for state in target.pending_connections.drain(..) {
            state.accepted_peer_open.store(false, Ordering::SeqCst);
        }
    }
    Ok(())
}

pub(in crate::execution) fn guest_unix_connection_peer_open(
    state: Option<&Arc<GuestUnixConnectionState>>,
) -> bool {
    state.is_some_and(|state| state.accepted_peer_open.load(Ordering::SeqCst))
}

fn consume_guest_unix_peer(
    registry: &GuestUnixAddressRegistry,
    source_host_address_key: &str,
    target_binding_id: &str,
) -> Result<Option<GuestUnixAddress>, SidecarError> {
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    let source_binding_id = registry
        .iter()
        .filter(|(_, entry)| {
            entry.host_address_key == source_host_address_key
                && entry
                    .queued_by_target
                    .get(target_binding_id)
                    .copied()
                    .unwrap_or_default()
                    > 0
        })
        .max_by_key(|(_, entry)| entry.generation)
        .map(|(binding_id, _)| binding_id.clone());
    let Some(source_binding_id) = source_binding_id else {
        return Ok(None);
    };
    let entry = registry
        .get_mut(&source_binding_id)
        .expect("selected Unix binding remains registered");
    let address = entry.address.clone();
    if let Some(queued) = entry.queued_by_target.get_mut(target_binding_id) {
        *queued = queued.saturating_sub(1);
        if *queued == 0 {
            entry.queued_by_target.remove(target_binding_id);
        }
    }
    if entry.active_bindings == 0 && entry.queued_by_target.is_empty() {
        registry.remove(&source_binding_id);
    }
    Ok(Some(address))
}

pub(in crate::execution) fn release_guest_unix_binding(
    registry: &GuestUnixAddressRegistry,
    binding_id: &str,
) -> Result<(), SidecarError> {
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    let Some(entry) = registry.get_mut(binding_id) else {
        return Ok(());
    };
    entry.active_bindings = entry.active_bindings.saturating_sub(1);
    if entry.active_bindings == 0 && entry.queued_by_target.is_empty() {
        registry.remove(binding_id);
    }
    Ok(())
}

pub(in crate::execution) fn rollback_guest_unix_binding(
    registry: &GuestUnixAddressRegistry,
    binding_id: &str,
) -> Result<(), SidecarError> {
    registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?
        .remove(binding_id);
    Ok(())
}

pub(in crate::execution) fn rollback_guest_unix_path_binding(
    registry: &GuestUnixAddressRegistry,
    binding_id: &str,
    kernel: &mut SidecarKernel,
    guest_path: &str,
    host_path: &Path,
) -> Result<(), SidecarError> {
    let registry_error = rollback_guest_unix_binding(registry, binding_id).err();
    cleanup_private_unix_socket_path(host_path);
    let marker_error = kernel.remove_file(guest_path).err().map(kernel_error);
    match (registry_error, marker_error) {
        (None, None) => Ok(()),
        (Some(error), None) | (None, Some(error)) => Err(error),
        (Some(registry_error), Some(marker_error)) => Err(SidecarError::Execution(format!(
            "failed to roll back Unix socket metadata: {registry_error}; failed to remove Unix socket node {guest_path}: {marker_error}"
        ))),
    }
}

pub(in crate::execution) fn purge_guest_unix_target(
    registry: &GuestUnixAddressRegistry,
    target_binding_id: &str,
) -> Result<(), SidecarError> {
    let mut registry = registry
        .lock()
        .map_err(|_| SidecarError::InvalidState(String::from("Unix address registry poisoned")))?;
    registry.retain(|_, entry| {
        entry.queued_by_target.remove(target_binding_id);
        entry.active_bindings != 0 || !entry.queued_by_target.is_empty()
    });
    Ok(())
}

pub(in crate::execution) fn host_abstract_unix_name(
    context: &SocketPathContext,
    guest_name: &[u8],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"agentos-unix-abstract-v1\0");
    digest.update(context.unix_abstract_namespace);
    digest.update(guest_name);
    digest.finalize().into()
}

pub(in crate::execution) fn guest_autobind_unix_name(
    kernel_pid: u32,
    listener_id: &str,
    nonce: u32,
) -> [u8; 5] {
    let digest = Sha256::digest(format!(
        "agentos-unix-autobind-v1\0{kernel_pid}\0{listener_id}\0{nonce}"
    ));
    let value =
        ((u32::from(digest[0]) << 12) | (u32::from(digest[1]) << 4) | (u32::from(digest[2]) >> 4))
            & 0x000f_ffff;
    format!("{value:05x}")
        .as_bytes()
        .try_into()
        .expect("five hexadecimal digits")
}

impl ActiveUnixSocket {
    pub(in crate::execution) fn connect(
        host_path: &Path,
        guest_path: &str,
        resources: Arc<ResourceLedger>,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        let stream = UnixStream::connect(host_path).map_err(sidecar_net_error)?;
        Self::from_stream(
            stream,
            None,
            None,
            Some(guest_path.to_owned()),
            resources,
            runtime_context,
            reactor_limits,
        )
    }

    pub(in crate::execution) fn from_stream(
        stream: UnixStream,
        listener_id: Option<String>,
        local_path: Option<String>,
        remote_path: Option<String>,
        resources: Arc<ResourceLedger>,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        Self::from_stream_with_metadata(
            stream,
            listener_id,
            local_path,
            remote_path,
            None,
            None,
            None,
            None,
            resources,
            runtime_context,
            reactor_limits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::execution) fn from_stream_with_metadata(
        stream: UnixStream,
        listener_id: Option<String>,
        local_path: Option<String>,
        remote_path: Option<String>,
        local_abstract_path_hex: Option<String>,
        remote_abstract_path_hex: Option<String>,
        local_registry_binding_id: Option<String>,
        private_host_path: Option<PathBuf>,
        resources: Arc<ResourceLedger>,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        let read_stream = stream.try_clone().map_err(sidecar_net_error)?;
        let write_stream = stream.try_clone().map_err(sidecar_net_error)?;
        let fairness_identity = Arc::new(OnceLock::new());
        let fairness_identity_committed = Arc::new(tokio::sync::Notify::new());
        let fairness_retirement =
            SocketFairnessRetirement::new(Arc::clone(&fairness_identity), runtime_context.clone());
        let plain_commands = spawn_unix_plain_socket_transport(
            &runtime_context,
            write_stream,
            &resources,
            reactor_limits,
            Arc::clone(&fairness_identity),
            Arc::clone(&fairness_identity_committed),
        )?;
        let stream = Arc::new(Mutex::new(stream));
        let (sender, events) = async_completion_channel(
            runtime_context.clone(),
            socket_completion_capacity(reactor_limits),
            tcp_socket_event_retained_bytes,
        );
        let event_pusher = SocketReadinessSubscribers::new(&resources);
        let application_read_interest = Arc::new(AtomicBool::new(false));
        let application_read_notify = Arc::new(tokio::sync::Notify::new());
        let read_event_notify = Arc::new(tokio::sync::Notify::new());
        let saw_local_shutdown = Arc::new(AtomicBool::new(false));
        let saw_remote_end = Arc::new(AtomicBool::new(false));
        let close_notified = Arc::new(AtomicBool::new(false));
        spawn_unix_socket_reader(
            runtime_context.clone(),
            read_stream,
            sender.clone(),
            Arc::clone(&event_pusher),
            Arc::clone(&read_event_notify),
            Arc::clone(&application_read_interest),
            Arc::clone(&application_read_notify),
            Arc::clone(&saw_local_shutdown),
            Arc::clone(&saw_remote_end),
            Arc::clone(&close_notified),
            Arc::clone(&resources),
            reactor_limits,
            Arc::clone(&fairness_identity),
            Arc::clone(&fairness_identity_committed),
        )?;

        Ok(Self {
            reactor_limits,
            fairness_identity,
            fairness_identity_committed,
            fairness_retirement,
            description_lease: Arc::new(SocketDescriptionLease::default()),
            stream,
            plain_commands,
            events: Arc::new(Mutex::new(events)),
            event_sender: sender,
            read_event_notify,
            event_pusher: Arc::clone(&event_pusher),
            readiness_registration: SocketReadinessRegistration::new(
                event_pusher,
                Some(Arc::clone(&application_read_interest)),
                Some(Arc::clone(&application_read_notify)),
            ),
            application_read_interest,
            application_read_notify,
            listener_id,
            local_path,
            remote_path,
            local_abstract_path_hex,
            remote_abstract_path_hex,
            local_registry_binding_id,
            remote_registry_binding_id: None,
            connection_state: None,
            private_host_path,
            saw_local_shutdown,
            saw_remote_end,
            close_notified,
            pending_read_event: Arc::new(Mutex::new(None)),
            read_state: Arc::new(Mutex::new(SocketReadState::default())),
            description_handles: Arc::new(()),
            listener_connection_retirement: None,
            resources,
        })
    }

    pub(in crate::execution) fn clone_for_fd_transfer(&self) -> Self {
        Self {
            reactor_limits: self.reactor_limits,
            fairness_identity: Arc::clone(&self.fairness_identity),
            fairness_identity_committed: Arc::clone(&self.fairness_identity_committed),
            fairness_retirement: Arc::clone(&self.fairness_retirement),
            description_lease: Arc::clone(&self.description_lease),
            stream: Arc::clone(&self.stream),
            plain_commands: self.plain_commands.clone(),
            events: Arc::clone(&self.events),
            event_sender: self.event_sender.clone(),
            read_event_notify: Arc::clone(&self.read_event_notify),
            event_pusher: Arc::clone(&self.event_pusher),
            readiness_registration: SocketReadinessRegistration::new(
                Arc::clone(&self.event_pusher),
                Some(Arc::clone(&self.application_read_interest)),
                Some(Arc::clone(&self.application_read_notify)),
            ),
            application_read_interest: Arc::clone(&self.application_read_interest),
            application_read_notify: Arc::clone(&self.application_read_notify),
            listener_id: self.listener_id.clone(),
            local_path: self.local_path.clone(),
            remote_path: self.remote_path.clone(),
            local_abstract_path_hex: self.local_abstract_path_hex.clone(),
            remote_abstract_path_hex: self.remote_abstract_path_hex.clone(),
            local_registry_binding_id: self.local_registry_binding_id.clone(),
            remote_registry_binding_id: self.remote_registry_binding_id.clone(),
            connection_state: self.connection_state.clone(),
            private_host_path: self.private_host_path.clone(),
            saw_local_shutdown: Arc::clone(&self.saw_local_shutdown),
            saw_remote_end: Arc::clone(&self.saw_remote_end),
            close_notified: Arc::clone(&self.close_notified),
            pending_read_event: Arc::clone(&self.pending_read_event),
            read_state: Arc::clone(&self.read_state),
            description_handles: Arc::clone(&self.description_handles),
            listener_connection_retirement: self.listener_connection_retirement.clone(),
            resources: Arc::clone(&self.resources),
        }
    }

    pub(in crate::execution) fn is_final_description_handle(&self) -> bool {
        Arc::strong_count(&self.description_handles) == 1
    }

    pub(in crate::execution) fn retain_description_lease(
        &self,
        lease: Arc<agentos_runtime::capability::CapabilityLease>,
    ) {
        self.description_lease.retain(lease);
    }

    pub(in crate::execution) fn set_event_pusher(
        &self,
        session: Option<ExecutionWakeHandle>,
        identity: Option<(
            agentos_runtime::capability::CapabilityId,
            agentos_runtime::capability::CapabilityGeneration,
        )>,
        owner_notify: Arc<tokio::sync::Notify>,
    ) {
        let (Some(session), Some((capability_id, capability_generation))) = (session, identity)
        else {
            return;
        };
        self.readiness_registration.register(
            Some(session),
            Some((capability_id, capability_generation)),
            owner_notify,
            agentos_runtime::readiness::ReadyFlags::READABLE,
        );
    }

    pub(in crate::execution) fn set_fairness_identity(
        &self,
        identity: Option<(u64, u64)>,
    ) -> Result<(), SidecarError> {
        let identity = identity.ok_or_else(|| {
            SidecarError::host(
                "ERR_AGENTOS_FAIRNESS_IDENTITY",
                String::from("Unix socket capability was committed outside a VM runtime scope"),
            )
        })?;
        self.fairness_identity.set(identity).map_err(|_| {
            SidecarError::host(
                "ERR_AGENTOS_FAIRNESS_IDENTITY",
                String::from("Unix socket capability identity was committed more than once"),
            )
        })?;
        self.fairness_identity_committed.notify_waiters();
        Ok(())
    }

    pub(in crate::execution) fn set_application_read_interest(
        &self,
        enabled: bool,
    ) -> Result<(), SidecarError> {
        self.readiness_registration
            .set_application_read_interest(enabled)?;
        Ok(())
    }

    pub(in crate::execution) fn poll(
        &mut self,
        _wait: Duration,
    ) -> Result<Option<TcpSocketEvent>, SidecarError> {
        if let Some(event) = self
            .pending_read_event
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("Unix pending read event lock poisoned"))
            })?
            .take()
        {
            return Ok(Some(event));
        }
        match self
            .events
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("Unix socket event channel lock poisoned"))
            })?
            .try_recv()
        {
            Ok(event) => Ok(Some(event)),
            Err(TokioTryRecvError::Empty | TokioTryRecvError::Disconnected) => Ok(None),
        }
    }

    pub(in crate::execution) fn poll_limited(
        &mut self,
        wait: Duration,
        max_bytes: usize,
    ) -> Result<Option<TcpSocketEvent>, SidecarError> {
        let pending = self
            .pending_read_event
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("Unix pending read event lock poisoned"))
            })?
            .take();
        let event = match pending {
            Some(event) => Some(event),
            None => self.poll(wait)?,
        };
        let (event, remainder) = limit_tcp_socket_event(event, max_bytes);
        if let Some(remainder) = remainder {
            *self.pending_read_event.lock().map_err(|_| {
                SidecarError::InvalidState(String::from("Unix pending read event lock poisoned"))
            })? = Some(remainder);
        }
        Ok(event)
    }

    pub(in crate::execution) fn socket_info(&self) -> Value {
        unix_socket_info_value(self.local_path.as_deref(), self.remote_path.as_deref())
    }

    pub(in crate::execution) fn socket_info_with_registry(
        &mut self,
        registry: &GuestUnixAddressRegistry,
    ) -> Result<Value, SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("Unix socket lock poisoned")))?;
        let live_local = unix_host_address_key(&stream.local_addr().map_err(sidecar_net_error)?);
        let live_remote = unix_host_address_key(&stream.peer_addr().map_err(sidecar_net_error)?);
        drop(stream);
        let local = live_local
            .as_deref()
            .map(|key| guest_unix_address_for_host_key(registry, key))
            .transpose()?
            .flatten();
        let remote = if let (Some(key), Some(target_binding_id)) = (
            live_remote.as_deref(),
            self.remote_registry_binding_id.as_deref(),
        ) {
            consume_guest_unix_peer(registry, key, target_binding_id)?
                .or(guest_unix_address_for_host_key(registry, key)?)
        } else {
            live_remote
                .as_deref()
                .map(|key| guest_unix_address_for_host_key(registry, key))
                .transpose()?
                .flatten()
        };
        if let Some(address) = remote.as_ref() {
            self.remote_path = Some(address.path.clone());
            self.remote_abstract_path_hex = address.abstract_path_hex.clone();
        }
        let local_path = local
            .as_ref()
            .map(|address| address.path.as_str())
            .or(self.local_path.as_deref());
        let remote_path = remote
            .as_ref()
            .map(|address| address.path.as_str())
            .or(self.remote_path.as_deref());
        let mut info = unix_socket_info_value(local_path, remote_path);
        if let Some(object) = info.as_object_mut() {
            object.insert(
                String::from("localAbstractPathHex"),
                json!(local
                    .as_ref()
                    .and_then(|address| address.abstract_path_hex.as_deref())
                    .or(self.local_abstract_path_hex.as_deref())),
            );
            object.insert(
                String::from("remoteAbstractPathHex"),
                json!(remote
                    .as_ref()
                    .and_then(|address| address.abstract_path_hex.as_deref())
                    .or(self.remote_abstract_path_hex.as_deref())),
            );
        }
        Ok(info)
    }

    pub(in crate::execution) fn cache_remote_peer_metadata(
        &mut self,
        registry: &GuestUnixAddressRegistry,
    ) -> Result<(), SidecarError> {
        if self.remote_registry_binding_id.is_none() || self.remote_path.is_some() {
            return Ok(());
        }
        self.socket_info_with_registry(registry)?;
        Ok(())
    }

    pub(in crate::execution) fn bind_path(
        &mut self,
        host_path: &Path,
        guest_path: &str,
        binding_id: &str,
    ) -> Result<(), SidecarError> {
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent).map_err(sidecar_net_error)?;
        }
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("Unix socket lock poisoned")))?;
        let address = UnixAddr::new(host_path)
            .map_err(|error| sidecar_net_error(std::io::Error::from_raw_os_error(error as i32)))?;
        bind_socket(stream.as_raw_fd(), &address)
            .map_err(|error| sidecar_net_error(std::io::Error::from_raw_os_error(error as i32)))?;
        drop(stream);
        self.local_path = Some(guest_path.to_owned());
        self.local_abstract_path_hex = None;
        self.local_registry_binding_id = Some(binding_id.to_owned());
        self.private_host_path = Some(host_path.to_path_buf());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(in crate::execution) fn bind_abstract(
        &mut self,
        host_name: &[u8],
        guest_name: &[u8],
        binding_id: &str,
    ) -> Result<(), SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("Unix socket lock poisoned")))?;
        let address = UnixAddr::new_abstract(host_name)
            .map_err(|error| sidecar_net_error(std::io::Error::from_raw_os_error(error as i32)))?;
        bind_socket(stream.as_raw_fd(), &address)
            .map_err(|error| sidecar_net_error(std::io::Error::from_raw_os_error(error as i32)))?;
        drop(stream);
        self.local_path = Some(abstract_unix_node_path(guest_name));
        self.local_abstract_path_hex = Some(abstract_unix_name_hex(guest_name));
        self.local_registry_binding_id = Some(binding_id.to_owned());
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub(in crate::execution) fn bind_abstract(
        &mut self,
        _host_name: &[u8],
        _guest_name: &[u8],
        _binding_id: &str,
    ) -> Result<(), SidecarError> {
        Err(abstract_unix_unsupported())
    }

    pub(in crate::execution) fn write_all(&self, contents: &[u8]) -> Result<usize, SidecarError> {
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("Unix socket lock poisoned")))?;
        write_all_nonblocking(&mut *stream, contents, self.reactor_limits)?;
        Ok(contents.len())
    }

    pub(in crate::execution) fn begin_plain_write(
        &self,
        contents: &[u8],
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>,
        SidecarError,
    > {
        let payload = reserve_plain_socket_write_payload(&self.resources, contents)?;
        let (completion, response) = tokio::sync::oneshot::channel();
        self.plain_commands
            .try_send(NativePlainSocketCommand::Write {
                payload,
                completion,
            })
            .map_err(plain_socket_command_admission_error)?;
        Ok(response)
    }

    pub(in crate::execution) fn begin_plain_shutdown(
        &self,
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>,
        SidecarError,
    > {
        let reservation = reserve_plain_socket_command(&self.resources)?;
        let (completion, response) = tokio::sync::oneshot::channel();
        self.saw_local_shutdown.store(true, Ordering::SeqCst);
        self.plain_commands
            .try_send(NativePlainSocketCommand::Shutdown {
                _command_reservation: reservation,
                completion,
            })
            .map_err(plain_socket_command_admission_error)?;
        if self.saw_remote_end.load(Ordering::SeqCst)
            && !self.close_notified.swap(true, Ordering::SeqCst)
            && self
                .event_sender
                .try_send(TcpSocketEvent::Close { had_error: false })
                .is_ok()
        {
            push_socket_event(&self.event_pusher, "close");
        }
        Ok(response)
    }

    pub(in crate::execution) fn shutdown_write(&self) -> Result<(), SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("Unix socket lock poisoned")))?;
        self.saw_local_shutdown.store(true, Ordering::SeqCst);
        stream
            .shutdown(Shutdown::Write)
            .map_err(sidecar_net_error)?;
        if self.saw_remote_end.load(Ordering::SeqCst)
            && !self.close_notified.swap(true, Ordering::SeqCst)
        {
            if let Err(error) = self
                .event_sender
                .try_send(TcpSocketEvent::Close { had_error: false })
            {
                eprintln!(
                    "ERR_AGENTOS_SOCKET_EVENT_DROPPED: Unix socket close event was not admitted: {error}"
                );
            }
        }
        Ok(())
    }

    pub(in crate::execution) fn close(&self) -> Result<(), SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("Unix socket lock poisoned")))?;
        stream.shutdown(Shutdown::Both).map_err(sidecar_net_error)
    }
}

#[derive(Clone, Debug)]
pub(in crate::execution) enum NativeUnixConnectTarget {
    Path(PathBuf),
    Abstract(Vec<u8>),
}

async fn connect_native_unix_socket(
    socket: Option<Socket>,
    target: &NativeUnixConnectTarget,
) -> Result<UnixStream, SidecarError> {
    let socket = socket
        .map(Ok)
        .unwrap_or_else(|| Socket::new(Domain::UNIX, Type::STREAM, None))
        .map_err(sidecar_net_error)?;
    socket.set_nonblocking(true).map_err(sidecar_net_error)?;
    let connect_result = match target {
        NativeUnixConnectTarget::Path(path) => socket
            .connect(&SockAddr::unix(path).map_err(sidecar_net_error)?)
            .map_err(sidecar_net_error),
        #[cfg(target_os = "linux")]
        NativeUnixConnectTarget::Abstract(name) => {
            let address = UnixAddr::new_abstract(name).map_err(|error| {
                sidecar_net_error(std::io::Error::from_raw_os_error(error as i32))
            })?;
            connect_socket(socket.as_raw_fd(), &address)
                .map_err(|error| sidecar_net_error(std::io::Error::from_raw_os_error(error as i32)))
        }
        #[cfg(not(target_os = "linux"))]
        NativeUnixConnectTarget::Abstract(_) => return Err(abstract_unix_unsupported()),
    };
    if let Err(error) = connect_result {
        let code = guest_error_code(&error);
        if !matches!(code, Some("EINPROGRESS" | "EALREADY" | "EAGAIN")) {
            return Err(error);
        }
    }
    let stream: UnixStream = socket.into();
    stream.set_nonblocking(true).map_err(sidecar_net_error)?;
    let stream = tokio::net::UnixStream::from_std(stream).map_err(sidecar_net_error)?;
    stream.writable().await.map_err(sidecar_net_error)?;
    if let Some(error) = stream.take_error().map_err(sidecar_net_error)? {
        return Err(sidecar_net_error(error));
    }
    stream.peer_addr().map_err(sidecar_net_error)?;
    stream.into_std().map_err(sidecar_net_error)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::execution) fn defer_native_unix_connect(
    process: &mut ActiveProcess,
    request_id: u64,
    pending_capability: PendingCapability,
    target: NativeUnixConnectTarget,
    remote_address: GuestUnixAddress,
    unix_bound_addresses: GuestUnixAddressRegistry,
    target_binding_id: String,
    bound_listener: Option<(String, ActiveUnixListener)>,
) -> Result<HostServiceResponse, SidecarError> {
    let socket_id = process.allocate_unix_socket_id();
    let runtime = process.runtime_context.clone();
    let task_runtime = runtime.clone();
    let resources = Arc::clone(process.runtime_context.resources());
    let limits = reactor_io_limits(&process.limits);
    let bound_socket_result = bound_listener.as_ref().map(|(_, listener)| {
        listener
            .bound_socket
            .as_ref()
            .ok_or_else(|| sidecar_net_error(std::io::Error::from_raw_os_error(libc::EINVAL)))?
            .try_clone()
            .map_err(sidecar_net_error)
    });
    let bound_socket = match bound_socket_result.transpose() {
        Ok(socket) => socket,
        Err(error) => {
            if let Some((listener_id, listener)) = bound_listener {
                process.unix_listeners.insert(listener_id, listener);
            }
            return Err(error);
        }
    };
    let local_address = bound_listener
        .as_ref()
        .map(|(_, listener)| GuestUnixAddress {
            path: listener.path.clone(),
            abstract_path_hex: listener.abstract_path_hex.clone(),
        });
    let local_registry_binding_id = bound_listener
        .as_ref()
        .map(|(_, listener)| listener.registry_binding_id.clone());
    let private_host_path = bound_listener
        .as_ref()
        .and_then(|(_, listener)| listener.private_host_path.clone());
    let connected = Arc::new(Mutex::new(PendingNetConnectState {
        connected: None,
        bound_unix_listener: bound_listener,
    }));
    let task_connected = Arc::clone(&connected);
    if process.pending_net_connects.contains_key(&request_id) {
        restore_pending_bound_unix_connect(process, &connected)?;
        return Err(SidecarError::host(
            "ERR_AGENTOS_SOCKET_CONNECT_STATE",
            format!("request {request_id} already has a pending connect"),
        ));
    }
    let connect_metadata = match PendingGuestUnixConnectMetadata::register(
        Arc::clone(&unix_bound_addresses),
        local_registry_binding_id.clone(),
        target_binding_id.clone(),
    ) {
        Ok(metadata) => metadata,
        Err(error) => {
            restore_pending_bound_unix_connect(process, &connected)?;
            return Err(error);
        }
    };
    process
        .pending_net_connects
        .insert(request_id, Arc::clone(&connected));
    let (respond_to, receiver) = tokio::sync::oneshot::channel();
    let spawn = runtime.spawn(agentos_runtime::TaskClass::Socket, async move {
        let result = match crate::execution::operation_deadline_timeout(
            "Unix socket connect",
            limits.operation_deadline,
            connect_native_unix_socket(bound_socket, &target),
        )
        .await
        {
            Ok(Ok(stream)) => {
                let built = ActiveUnixSocket::from_stream_with_metadata(
                    stream,
                    None,
                    local_address.as_ref().map(|address| address.path.clone()),
                    Some(remote_address.path.clone()),
                    local_address
                        .as_ref()
                        .and_then(|address| address.abstract_path_hex.clone()),
                    remote_address.abstract_path_hex.clone(),
                    local_registry_binding_id.clone(),
                    private_host_path,
                    resources,
                    task_runtime,
                    limits,
                );
                match built {
                    Ok(mut socket) => {
                        socket.connection_state = Some(connect_metadata.commit());
                        socket.remote_registry_binding_id = Some(target_binding_id);
                        task_connected
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .connected = Some(PendingNetConnect::Unix {
                            socket_id,
                            socket: Box::new(socket),
                            pending_capability,
                            remote_path: remote_address.path,
                            remote_abstract_path_hex: remote_address.abstract_path_hex,
                        });
                        Ok(Value::Null)
                    }
                    Err(error) => Err(deferred_connect_error(error)),
                }
            }
            Ok(Err(error)) => Err(deferred_connect_error(error)),
            Err(_) => Err(crate::state::DeferredRpcError {
                code: String::from("ETIMEDOUT"),
                message: format!(
                    "Unix connect exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                    limits.operation_deadline.as_millis()
                ),
                details: None,
            }),
        };
        if respond_to.send(result).is_err() {
            eprintln!("ERR_AGENTOS_SOCKET_COMPLETION_DROPPED: Unix connect caller stopped waiting");
        }
    });
    if let Err(error) = spawn {
        if let Some(pending) = process.pending_net_connects.remove(&request_id) {
            restore_pending_bound_unix_connect(process, &pending)?;
        }
        return Err(SidecarError::from(error));
    }
    Ok(HostServiceResponse::Deferred {
        receiver,
        timeout: None,
        task_class: agentos_runtime::TaskClass::Socket,
    })
}

// ActiveUnixListener moved to crate::state

impl ActiveUnixListener {
    #[allow(clippy::too_many_arguments)]
    fn from_bound_socket(
        socket: Socket,
        guest_path: String,
        abstract_path_hex: Option<String>,
        registry_binding_id: String,
        private_host_path: Option<PathBuf>,
        guest_node_path: Option<String>,
        runtime_context: agentos_runtime::RuntimeContext,
        backlog: Option<u32>,
    ) -> Self {
        let event_pusher = SocketReadinessSubscribers::new(runtime_context.resources());
        let (sender, events) =
            async_completion_channel(runtime_context, 1, unix_listener_event_retained_bytes);
        drop(sender);
        let (close_sender, close_completion) = tokio::sync::oneshot::channel();
        drop(close_sender);
        Self {
            listener: None,
            bound_socket: Some(socket),
            events: Arc::new(Mutex::new(events)),
            event_pusher: Arc::clone(&event_pusher),
            readiness_registration: SocketReadinessRegistration::new(event_pusher, None, None),
            close_notify: Arc::new(tokio::sync::Notify::new()),
            close_completion: Arc::new(Mutex::new(Some(close_completion))),
            acceptor_started: false,
            path: guest_path,
            abstract_path_hex,
            registry_binding_id,
            private_host_path,
            guest_node_path,
            backlog: usize::try_from(backlog.unwrap_or(DEFAULT_NET_BACKLOG))
                .expect("default backlog fits within usize"),
            active_connection_ids: Arc::new(Mutex::new(BTreeSet::new())),
            description_handles: Arc::new(()),
            description_lease: Arc::new(SocketDescriptionLease::default()),
            pending_event: Arc::new(Mutex::new(None)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::execution) fn bind_unlistened(
        host_path: &Path,
        guest_path: &str,
        registry_binding_id: String,
        runtime_context: agentos_runtime::RuntimeContext,
    ) -> Result<Self, SidecarError> {
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent).map_err(sidecar_net_error)?;
        }
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None).map_err(sidecar_net_error)?;
        socket
            .bind(&SockAddr::unix(host_path).map_err(sidecar_net_error)?)
            .map_err(sidecar_net_error)?;
        Ok(Self::from_bound_socket(
            socket,
            guest_path.to_owned(),
            None,
            registry_binding_id,
            Some(host_path.to_path_buf()),
            Some(guest_path.to_owned()),
            runtime_context,
            None,
        ))
    }

    #[cfg(target_os = "linux")]
    pub(in crate::execution) fn bind_abstract_unlistened(
        host_name: &[u8],
        guest_name: &[u8],
        registry_binding_id: String,
        runtime_context: agentos_runtime::RuntimeContext,
    ) -> Result<Self, SidecarError> {
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None).map_err(sidecar_net_error)?;
        let address = UnixAddr::new_abstract(host_name)
            .map_err(|error| sidecar_net_error(std::io::Error::from_raw_os_error(error as i32)))?;
        bind_socket(socket.as_raw_fd(), &address)
            .map_err(|error| sidecar_net_error(std::io::Error::from_raw_os_error(error as i32)))?;
        Ok(Self::from_bound_socket(
            socket,
            abstract_unix_node_path(guest_name),
            Some(abstract_unix_name_hex(guest_name)),
            registry_binding_id,
            None,
            None,
            runtime_context,
            None,
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub(in crate::execution) fn bind_abstract_unlistened(
        _host_name: &[u8],
        _guest_name: &[u8],
        _registry_binding_id: String,
        _runtime_context: agentos_runtime::RuntimeContext,
    ) -> Result<Self, SidecarError> {
        Err(abstract_unix_unsupported())
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::execution) fn listen_bound(
        mut self,
        context: SocketPathContext,
        backlog: Option<u32>,
        capabilities: CapabilityRegistry,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        set_guest_unix_pending_connection_limit(
            &context.unix_bound_addresses,
            &self.registry_binding_id,
            listener_accept_capacity(backlog, reactor_limits),
        )?;
        let socket = self
            .bound_socket
            .take()
            .ok_or_else(|| sidecar_net_error(std::io::Error::from_raw_os_error(libc::EINVAL)))?;
        let backlog_value = backlog.unwrap_or(DEFAULT_NET_BACKLOG);
        socket
            .listen(i32::try_from(backlog_value).unwrap_or(i32::MAX))
            .map_err(sidecar_net_error)?;
        socket.set_nonblocking(true).map_err(sidecar_net_error)?;
        let private_host_path = self.private_host_path.take();
        let guest_node_path = self.guest_node_path.take();
        let event_pusher = Arc::clone(&self.event_pusher);
        let description_handles = Arc::clone(&self.description_handles);
        let mut listened = Self::from_listener(
            socket.into(),
            self.path.clone(),
            self.abstract_path_hex.clone(),
            self.registry_binding_id.clone(),
            private_host_path,
            guest_node_path,
            context,
            backlog,
            capabilities,
            runtime_context,
            reactor_limits,
        )?;
        listened.readiness_registration =
            SocketReadinessRegistration::new(Arc::clone(&event_pusher), None, None);
        listened.event_pusher = event_pusher;
        listened.description_handles = description_handles;
        listened.description_lease = Arc::clone(&self.description_lease);
        Ok(listened)
    }

    pub(in crate::execution) fn relisten(
        &mut self,
        registry: &GuestUnixAddressRegistry,
        backlog: u32,
        reactor_limits: ReactorIoLimits,
    ) -> Result<(), SidecarError> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| sidecar_net_error(std::io::Error::from_raw_os_error(libc::EINVAL)))?;
        SockRef::from(listener)
            .listen(i32::try_from(backlog).unwrap_or(i32::MAX))
            .map_err(sidecar_net_error)?;
        set_guest_unix_pending_connection_limit(
            registry,
            &self.registry_binding_id,
            listener_accept_capacity(Some(backlog), reactor_limits),
        )?;
        self.backlog = usize::try_from(backlog).unwrap_or(usize::MAX);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::execution) fn bind(
        host_path: &Path,
        guest_path: &str,
        registry_binding_id: String,
        context: SocketPathContext,
        backlog: Option<u32>,
        capabilities: CapabilityRegistry,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent).map_err(sidecar_net_error)?;
        }
        let listener = UnixListener::bind(host_path).map_err(sidecar_net_error)?;
        listener.set_nonblocking(true).map_err(sidecar_net_error)?;
        Self::from_listener(
            listener,
            guest_path.to_owned(),
            None,
            registry_binding_id,
            Some(host_path.to_path_buf()),
            Some(guest_path.to_owned()),
            context,
            backlog,
            capabilities,
            runtime_context,
            reactor_limits,
        )
    }

    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    pub(in crate::execution) fn bind_abstract(
        host_name: &[u8],
        guest_name: &[u8],
        registry_binding_id: String,
        context: SocketPathContext,
        backlog: Option<u32>,
        capabilities: CapabilityRegistry,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None).map_err(sidecar_net_error)?;
        let address = UnixAddr::new_abstract(host_name)
            .map_err(|error| sidecar_net_error(std::io::Error::from_raw_os_error(error as i32)))?;
        bind_socket(socket.as_raw_fd(), &address)
            .map_err(|error| sidecar_net_error(std::io::Error::from_raw_os_error(error as i32)))?;
        socket
            .listen(i32::try_from(backlog.unwrap_or(DEFAULT_NET_BACKLOG)).unwrap_or(i32::MAX))
            .map_err(sidecar_net_error)?;
        socket.set_nonblocking(true).map_err(sidecar_net_error)?;
        Self::from_listener(
            socket.into(),
            abstract_unix_node_path(guest_name),
            Some(abstract_unix_name_hex(guest_name)),
            registry_binding_id,
            None,
            None,
            context,
            backlog,
            capabilities,
            runtime_context,
            reactor_limits,
        )
    }

    #[cfg(not(target_os = "linux"))]
    #[allow(clippy::too_many_arguments)]
    pub(in crate::execution) fn bind_abstract(
        _host_name: &[u8],
        _guest_name: &[u8],
        _registry_binding_id: String,
        _context: SocketPathContext,
        _backlog: Option<u32>,
        _capabilities: CapabilityRegistry,
        _runtime_context: agentos_runtime::RuntimeContext,
        _reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        Err(abstract_unix_unsupported())
    }

    #[allow(clippy::too_many_arguments)]
    fn from_listener(
        listener: UnixListener,
        guest_path: String,
        abstract_path_hex: Option<String>,
        registry_binding_id: String,
        private_host_path: Option<PathBuf>,
        guest_node_path: Option<String>,
        context: SocketPathContext,
        backlog: Option<u32>,
        capabilities: CapabilityRegistry,
        runtime_context: agentos_runtime::RuntimeContext,
        reactor_limits: ReactorIoLimits,
    ) -> Result<Self, SidecarError> {
        let accept_capacity = listener_accept_capacity(backlog, reactor_limits);
        set_guest_unix_pending_connection_limit(
            &context.unix_bound_addresses,
            &registry_binding_id,
            accept_capacity,
        )?;
        let event_pusher = SocketReadinessSubscribers::new(capabilities.resources().as_ref());
        let close_notify = Arc::new(tokio::sync::Notify::new());
        let (close_complete, close_completion) = tokio::sync::oneshot::channel();
        let events = spawn_unix_listener_acceptor(
            runtime_context,
            listener.try_clone().map_err(sidecar_net_error)?,
            guest_path.clone(),
            abstract_path_hex.clone(),
            context.unix_bound_addresses,
            registry_binding_id.clone(),
            Arc::clone(&event_pusher),
            Arc::clone(&close_notify),
            close_complete,
            accept_capacity,
            capabilities,
            reactor_limits,
        )?;
        Ok(Self {
            listener: Some(listener),
            bound_socket: None,
            events: Arc::new(Mutex::new(events)),
            event_pusher: Arc::clone(&event_pusher),
            readiness_registration: SocketReadinessRegistration::new(event_pusher, None, None),
            close_notify,
            close_completion: Arc::new(Mutex::new(Some(close_completion))),
            acceptor_started: true,
            path: guest_path.to_owned(),
            abstract_path_hex,
            registry_binding_id,
            private_host_path,
            guest_node_path,
            backlog: usize::try_from(backlog.unwrap_or(DEFAULT_NET_BACKLOG))
                .expect("default backlog fits within usize"),
            active_connection_ids: Arc::new(Mutex::new(BTreeSet::new())),
            description_handles: Arc::new(()),
            description_lease: Arc::new(SocketDescriptionLease::default()),
            pending_event: Arc::new(Mutex::new(None)),
        })
    }

    pub(in crate::execution) fn clone_for_fd_transfer(&self) -> Result<Self, SidecarError> {
        Ok(Self {
            listener: self
                .listener
                .as_ref()
                .map(UnixListener::try_clone)
                .transpose()
                .map_err(sidecar_net_error)?,
            bound_socket: self
                .bound_socket
                .as_ref()
                .map(Socket::try_clone)
                .transpose()
                .map_err(sidecar_net_error)?,
            events: Arc::clone(&self.events),
            event_pusher: Arc::clone(&self.event_pusher),
            readiness_registration: SocketReadinessRegistration::new(
                Arc::clone(&self.event_pusher),
                None,
                None,
            ),
            close_notify: Arc::clone(&self.close_notify),
            close_completion: Arc::clone(&self.close_completion),
            acceptor_started: self.acceptor_started,
            path: self.path.clone(),
            abstract_path_hex: self.abstract_path_hex.clone(),
            registry_binding_id: self.registry_binding_id.clone(),
            private_host_path: self.private_host_path.clone(),
            guest_node_path: self.guest_node_path.clone(),
            backlog: self.backlog,
            active_connection_ids: Arc::clone(&self.active_connection_ids),
            description_handles: Arc::clone(&self.description_handles),
            description_lease: Arc::clone(&self.description_lease),
            pending_event: Arc::clone(&self.pending_event),
        })
    }

    pub(in crate::execution) fn is_final_description_handle(&self) -> bool {
        Arc::strong_count(&self.description_handles) == 1
    }

    pub(in crate::execution) fn path(&self) -> &str {
        &self.path
    }

    pub(in crate::execution) fn set_event_pusher(
        &self,
        session: Option<ExecutionWakeHandle>,
        identity: Option<(
            agentos_runtime::capability::CapabilityId,
            agentos_runtime::capability::CapabilityGeneration,
        )>,
        owner_notify: Arc<tokio::sync::Notify>,
    ) {
        let (Some(session), Some((capability_id, capability_generation))) = (session, identity)
        else {
            return;
        };
        self.readiness_registration.register(
            Some(session),
            Some((capability_id, capability_generation)),
            owner_notify,
            agentos_runtime::readiness::ReadyFlags::ACCEPT,
        );
    }

    pub(in crate::execution) fn poll(
        &mut self,
        wait: Duration,
    ) -> Result<Option<UnixListenerEvent>, SidecarError> {
        if let Some(event) = self
            .pending_event
            .lock()
            .map_err(|_| SidecarError::host("EIO", "Unix listener pending event lock poisoned"))?
            .take()
        {
            return Ok(Some(event));
        }
        let _ = wait;
        match self
            .events
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "Unix listener event channel lock poisoned",
                ))
            })?
            .try_recv()
        {
            Ok(event) => Ok(Some(event)),
            Err(TokioTryRecvError::Empty | TokioTryRecvError::Disconnected) => Ok(None),
        }
    }

    /// Non-destructive listener readiness probe used by combined POSIX poll.
    pub(in crate::execution) fn probe_readable(&mut self) -> Result<bool, SidecarError> {
        if self
            .pending_event
            .lock()
            .map_err(|_| SidecarError::host("EIO", "Unix listener pending event lock poisoned"))?
            .is_some()
        {
            return Ok(true);
        }
        let event = self.poll(Duration::ZERO)?;
        let mut pending = self
            .pending_event
            .lock()
            .map_err(|_| SidecarError::host("EIO", "Unix listener pending event lock poisoned"))?;
        *pending = event;
        Ok(pending.is_some())
    }

    pub(in crate::execution) fn close(
        self,
    ) -> Pin<Box<dyn Future<Output = Result<(), tokio::sync::oneshot::error::RecvError>> + Send>>
    {
        if !self.acceptor_started {
            return Box::pin(async { Ok(()) });
        }
        // `notify_one` retains a permit if the acceptor is between select
        // points. `notify_waiters` could lose that close signal and strand the
        // listener task until a later connection arrived.
        self.close_notify.notify_one();
        let completion = self
            .close_completion
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        Box::pin(async move {
            match completion {
                Some(completion) => completion.await,
                None => {
                    let (sender, receiver) = tokio::sync::oneshot::channel();
                    drop(sender);
                    receiver.await
                }
            }
        })
    }

    pub(in crate::execution) fn active_connection_count(&self) -> usize {
        self.active_connection_ids
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }

    pub(in crate::execution) fn register_connection(
        &self,
        socket_id: &str,
    ) -> Arc<ListenerConnectionRetirement> {
        self.active_connection_ids
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(socket_id.to_string());
        ListenerConnectionRetirement::new(&self.active_connection_ids, socket_id.to_string())
    }

    pub(in crate::execution) fn retain_description_lease(
        &self,
        lease: Arc<agentos_runtime::capability::CapabilityLease>,
    ) {
        self.description_lease.retain(lease);
    }
}

fn cleanup_private_unix_socket_path(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!(
            "failed to remove private Unix socket {}: {error}",
            path.display()
        ),
    }
}

pub(in crate::execution) fn release_unix_listener_capability(
    process: &mut ActiveProcess,
    listener_id: &str,
    listener: &ActiveUnixListener,
) -> Result<(), SidecarError> {
    listener.readiness_registration.retire();
    process.release_description_capability(
        &NativeCapabilityKey::UnixListener(listener_id.to_owned()),
        None,
        &listener.description_lease,
    )
}

impl Drop for PendingUnixConnectionGuard {
    fn drop(&mut self) {
        if let Some(state) = self.state.as_ref() {
            state.accepted_peer_open.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for ActiveUnixSocket {
    fn drop(&mut self) {
        if !self.is_final_description_handle() {
            return;
        }
        if self.listener_id.is_some() {
            if let Some(state) = self.connection_state.as_ref() {
                state.accepted_peer_open.store(false, Ordering::SeqCst);
            }
        }
        if let Some(path) = self.private_host_path.as_deref() {
            cleanup_private_unix_socket_path(path);
        }
    }
}

impl Drop for ActiveUnixListener {
    fn drop(&mut self) {
        if !self.is_final_description_handle() {
            return;
        }
        if let Some(path) = self.private_host_path.as_deref() {
            cleanup_private_unix_socket_path(path);
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod guest_unix_metadata_tests {
    use super::*;

    fn registry() -> GuestUnixAddressRegistry {
        Arc::new(Mutex::new(BTreeMap::new()))
    }

    fn register_abstract(registry: &GuestUnixAddressRegistry, binding_id: &str, host_key: &str) {
        register_guest_unix_binding(
            registry,
            binding_id,
            host_key,
            GuestUnixAddress {
                path: format!("\0{binding_id}"),
                abstract_path_hex: Some(String::from("61")),
            },
            None,
            None,
        )
        .expect("register Unix address");
    }

    #[test]
    fn pending_connection_guard_closes_rejected_peer() {
        let state = Arc::new(GuestUnixConnectionState {
            accepted_peer_open: AtomicBool::new(true),
        });
        {
            let _guard = PendingUnixConnectionGuard {
                state: Some(Arc::clone(&state)),
            };
        }
        assert!(!guest_unix_connection_peer_open(Some(&state)));
    }

    #[test]
    fn listener_close_marks_unaccepted_connections_closed() {
        let registry = registry();
        register_abstract(&registry, "target", "abstract:target");
        let state = register_guest_unix_connection(&registry, "target")
            .expect("register pending connection");
        close_pending_guest_unix_connections(&registry, "target")
            .expect("close pending connections");
        assert!(!guest_unix_connection_peer_open(Some(&state)));
        assert!(registry
            .lock()
            .expect("registry")
            .get("target")
            .expect("target")
            .pending_connections
            .is_empty());
    }

    #[test]
    fn pending_connection_metadata_is_bounded_by_listener_capacity() {
        let registry = registry();
        register_abstract(&registry, "target", "abstract:target");
        set_guest_unix_pending_connection_limit(&registry, "target", 1)
            .expect("set pending connection limit");

        let first = register_guest_unix_connection(&registry, "target")
            .expect("first pending connection fits");
        let error = register_guest_unix_connection(&registry, "target")
            .expect_err("second pending connection exceeds listener capacity");

        assert_eq!(guest_error_code(&error), Some("EAGAIN"));
        assert!(error.to_string().contains("listen backlog"));
        assert_eq!(
            registry
                .lock()
                .expect("registry")
                .get("target")
                .expect("target")
                .pending_connections
                .len(),
            1
        );
        assert!(guest_unix_connection_peer_open(Some(&first)));
    }

    #[test]
    fn connector_metadata_is_visible_before_reactor_accept() {
        let registry = registry();
        register_abstract(&registry, "target", "abstract:target");

        let metadata = PendingGuestUnixConnectMetadata::register(
            Arc::clone(&registry),
            None,
            String::from("target"),
        )
        .expect("reserve pending connection metadata");
        let accepted = accept_guest_unix_connection(&registry, "target")
            .expect("reactor may accept before connector resumes");
        let connected = metadata.commit();

        assert!(Arc::ptr_eq(&accepted, &connected));
        assert!(guest_unix_connection_peer_open(Some(&connected)));
    }

    #[test]
    fn failed_connect_rolls_back_reserved_peer_and_connection_metadata() {
        let registry = registry();
        register_abstract(&registry, "target", "abstract:target");
        register_abstract(&registry, "source", "abstract:source");

        let state = {
            let metadata = PendingGuestUnixConnectMetadata::register(
                Arc::clone(&registry),
                Some(String::from("source")),
                String::from("target"),
            )
            .expect("reserve connect metadata");
            let state = Arc::clone(&metadata.connection_state);
            drop(metadata);
            state
        };

        assert!(!guest_unix_connection_peer_open(Some(&state)));
        let registry = registry.lock().expect("registry");
        assert!(registry["target"].pending_connections.is_empty());
        assert!(registry["source"].queued_by_target.is_empty());
    }

    #[test]
    fn consumed_late_peer_metadata_returns_registry_to_baseline() {
        let registry = registry();
        register_abstract(&registry, "target", "abstract:target");
        register_abstract(&registry, "source", "abstract:source");
        queue_guest_unix_peer(&registry, "source", "target").expect("queue peer metadata");
        release_guest_unix_binding(&registry, "source").expect("release source binding");
        let address = consume_guest_unix_peer(&registry, "abstract:source", "target")
            .expect("consume peer metadata");
        assert!(address.is_some());
        let registry = registry.lock().expect("registry");
        assert!(registry.contains_key("target"));
        assert!(!registry.contains_key("source"));
    }
}

#[cfg(test)]
mod transferred_unix_alias_transport_tests {
    use super::*;

    fn exercise_surviving_unix_alias(close_sender: bool) {
        let process_runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create transferred Unix test runtime");
        let process_context = process_runtime.context();
        let generation = process_context
            .allocate_vm_generation()
            .expect("allocate transferred Unix test generation");
        let resources = Arc::clone(process_context.resources());
        let runtime = process_context.scoped_for_vm(Arc::clone(&resources), generation);
        let (stream, mut peer) = UnixStream::pair().expect("create Unix alias test pair");
        peer.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set Unix peer read timeout");
        let original = ActiveUnixSocket::from_stream(
            stream,
            None,
            None,
            None,
            resources,
            runtime.clone(),
            reactor_io_limits(&crate::limits::VmLimits::default()),
        )
        .expect("create original Unix description");
        original
            .set_fairness_identity(Some((82_001, generation)))
            .expect("commit Unix fairness identity");
        let transferred = original.clone_for_fd_transfer();
        assert!(!original.is_final_description_handle());
        assert!(!transferred.is_final_description_handle());

        let survivor = if close_sender {
            drop(original);
            transferred
        } else {
            drop(transferred);
            original
        };
        assert!(survivor.is_final_description_handle());

        survivor
            .application_read_interest
            .store(true, Ordering::Release);
        survivor.application_read_notify.notify_waiters();
        peer.write_all(b"host-to-unix")
            .expect("write to surviving Unix alias");
        let event = runtime.handle().block_on(async {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let received = {
                        let mut events = survivor.events.lock().expect("Unix event queue");
                        events.try_recv()
                    };
                    match received {
                        Ok(event) => return event,
                        Err(TokioTryRecvError::Empty) => tokio::task::yield_now().await,
                        Err(TokioTryRecvError::Disconnected) => {
                            panic!("Unix event queue disconnected")
                        }
                    }
                }
            })
            .await
            .expect("surviving Unix alias receives a transport wake")
        });
        match event {
            TcpSocketEvent::Data { bytes, .. } => assert_eq!(bytes, b"host-to-unix"),
            other => panic!("expected Unix data after alias close, got {other:?}"),
        }

        let completion = survivor
            .begin_plain_write(b"unix-to-host")
            .expect("write through surviving Unix alias");
        runtime.handle().block_on(async {
            tokio::time::timeout(Duration::from_secs(2), completion)
                .await
                .expect("surviving Unix alias write completes")
                .expect("surviving Unix alias write completion sender")
                .expect("surviving Unix alias write succeeds");
        });
        let mut bytes = [0_u8; 12];
        peer.read_exact(&mut bytes)
            .expect("peer reads surviving Unix alias write");
        assert_eq!(&bytes, b"unix-to-host");

        drop(peer);
        drop(survivor);
        runtime.close_admission();
    }

    #[test]
    fn transferred_unix_child_close_leaves_sender_wake_read_and_write_live() {
        exercise_surviving_unix_alias(false);
    }

    #[test]
    fn transferred_unix_sender_close_leaves_child_wake_read_and_write_live() {
        exercise_surviving_unix_alias(true);
    }
}

fn spawn_unix_plain_socket_transport(
    runtime: &agentos_runtime::RuntimeContext,
    stream: UnixStream,
    resources: &Arc<ResourceLedger>,
    limits: ReactorIoLimits,
    fairness_identity: Arc<OnceLock<(u64, u64)>>,
    fairness_identity_committed: Arc<tokio::sync::Notify>,
) -> Result<TokioSender<NativePlainSocketCommand>, SidecarError> {
    stream.set_nonblocking(true).map_err(sidecar_net_error)?;
    let (commands, receiver) = tokio_channel(plain_socket_command_capacity(resources)?);
    let cancellation = runtime.clone();
    runtime
        .spawn(agentos_runtime::TaskClass::Socket, async move {
            let transport_runtime = cancellation.clone();
            let transport = async move {
                match tokio::net::UnixStream::from_std(stream) {
                    Ok(stream) => {
                        run_plain_socket_transport(
                            PlainSocketWriteStream::Unix(stream),
                            receiver,
                            transport_runtime,
                            limits,
                            fairness_identity,
                            fairness_identity_committed,
                        )
                        .await
                    }
                    Err(error) => eprintln!("ERR_AGENTOS_SOCKET_TRANSPORT: {error}"),
                }
            };
            tokio::select! {
                () = cancellation.admission_closed() => {}
                () = transport => {}
            }
        })
        .map_err(SidecarError::from)?;
    Ok(commands)
}

#[allow(clippy::too_many_arguments)] // one admitted listener's owned reactor state
fn spawn_unix_listener_acceptor(
    runtime: agentos_runtime::RuntimeContext,
    listener: UnixListener,
    guest_path: String,
    local_abstract_path_hex: Option<String>,
    unix_bound_addresses: GuestUnixAddressRegistry,
    target_binding_id: String,
    event_pusher: Arc<SocketReadinessSubscribers>,
    close_notify: Arc<tokio::sync::Notify>,
    close_complete: tokio::sync::oneshot::Sender<()>,
    accept_capacity: usize,
    capabilities: CapabilityRegistry,
    limits: ReactorIoLimits,
) -> Result<AsyncCompletionReceiver<UnixListenerEvent>, SidecarError> {
    let (sender, receiver) = async_completion_channel(
        runtime.clone(),
        accept_capacity,
        unix_listener_event_retained_bytes,
    );
    let completion = UnixListenerTaskCompletion(Some(close_complete));
    runtime
        .spawn(agentos_runtime::TaskClass::Listener, async move {
            let _completion = completion;
            let listener = match tokio::io::unix::AsyncFd::new(listener) {
                Ok(listener) => listener,
                Err(error) => {
                    if sender
                        .send(UnixListenerEvent::Error {
                            code: io_error_code(&error),
                            message: error.to_string(),
                        })
                        .await
                        .is_ok()
                    {
                        push_listener_event(&event_pusher);
                    }
                    return;
                }
            };
            let mut accepts_this_turn = 0;
            loop {
                let mut ready = tokio::select! {
                    ready = listener.readable() => {
                        match ready {
                            Ok(ready) => ready,
                            Err(error) => {
                                if sender
                                    .send(UnixListenerEvent::Error {
                                        code: io_error_code(&error),
                                        message: error.to_string(),
                                    })
                                    .await
                                    .is_ok()
                                {
                                    push_listener_event(&event_pusher);
                                }
                                return;
                            }
                        }
                    }
                    _ = close_notify.notified() => return,
                };
                let capability = tokio::select! {
                    capability = capabilities.reserve_when_available(CapabilityKind::UnixSocket) => {
                        match capability {
                            Ok(capability) => capability,
                            Err(error) => {
                                if sender.send(UnixListenerEvent::Error {
                                    code: Some(String::from("ERR_AGENTOS_RESOURCE_LIMIT")),
                                    message: error.to_string(),
                                }).await.is_ok() {
                                    push_listener_event(&event_pusher);
                                }
                                return;
                            }
                        }
                    }
                    _ = close_notify.notified() => return,
                };
                let event = match ready.try_io(|inner| inner.get_ref().accept()) {
                    Ok(Ok((stream, remote_addr))) => {
                        let metadata = (|| {
                            let connection_state = accept_guest_unix_connection(
                                &unix_bound_addresses,
                                &target_binding_id,
                            )?;
                            let remote = match unix_host_address_key(&remote_addr) {
                                Some(key) => consume_guest_unix_peer(
                                    &unix_bound_addresses,
                                    &key,
                                    &target_binding_id,
                                )?,
                                None => None,
                            };
                            Ok::<_, SidecarError>((connection_state, remote))
                        })();
                        match metadata {
                            Ok((connection_state, remote)) => UnixListenerEvent::Connection {
                                socket: PendingUnixSocket {
                                    stream,
                                    local_path: Some(guest_path.clone()),
                                    remote_path: remote.as_ref().map(|address| address.path.clone()),
                                    local_abstract_path_hex: local_abstract_path_hex.clone(),
                                    remote_abstract_path_hex: remote.and_then(|address| address.abstract_path_hex),
                                    connection_guard: PendingUnixConnectionGuard {
                                        state: Some(connection_state),
                                    },
                                },
                                capability,
                            },
                            Err(error) => {
                                if let Err(shutdown_error) = stream.shutdown(Shutdown::Both) {
                                    eprintln!(
                                        "ERR_AGENTOS_UNIX_SOCKET_CLEANUP: failed to shut down rejected accepted socket: {shutdown_error}"
                                    );
                                }
                                UnixListenerEvent::Error {
                                    code: Some(host_service_error_code(&error)),
                                    message: error.to_string(),
                                }
                            }
                        }
                    }
                    Ok(Err(error)) => UnixListenerEvent::Error {
                        code: io_error_code(&error),
                        message: error.to_string(),
                    },
                    Err(_would_block) => continue,
                };
                if sender.send(event).await.is_err() {
                    return;
                }
                push_listener_event(&event_pusher);
                accepts_this_turn += 1;
                if accepts_this_turn
                    >= limits
                        .accept_quantum
                        .min(limits.operation_quantum)
                        .max(1)
                {
                    tokio::task::yield_now().await;
                    accepts_this_turn = 0;
                }
            }
        })
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    Ok(receiver)
}

struct UnixListenerTaskCompletion(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for UnixListenerTaskCompletion {
    fn drop(&mut self) {
        if let Some(completion) = self.0.take() {
            let _ = completion.send(());
        }
    }
}

fn push_listener_event(event_pusher: &Arc<SocketReadinessSubscribers>) {
    for target in event_pusher.targets() {
        if let Err(error) = target.publish_readiness(agentos_runtime::readiness::ReadyFlags::ACCEPT)
        {
            eprintln!("ERR_AGENTOS_NET_LISTENER_WAKE: failed to queue listener wake: {error}");
        }
    }
}

pub(in crate::execution) fn push_socket_event(
    event_pusher: &Arc<SocketReadinessSubscribers>,
    event: &'static str,
) {
    NET_TCP_TRACE_COUNTERS
        .socket_read_push_attempts
        .fetch_add(1, Ordering::Relaxed);
    let targets = event_pusher.targets();
    if targets.is_empty() {
        NET_TCP_TRACE_COUNTERS
            .socket_read_push_missing
            .fetch_add(1, Ordering::Relaxed);
        return;
    }
    let flags = match event {
        "data" => agentos_runtime::readiness::ReadyFlags::READABLE,
        "end" => agentos_runtime::readiness::ReadyFlags::END,
        "error" => agentos_runtime::readiness::ReadyFlags::ERROR,
        "close" => agentos_runtime::readiness::ReadyFlags::CLOSE,
        _ => {
            NET_TCP_TRACE_COUNTERS
                .socket_read_push_errors
                .fetch_add(1, Ordering::Relaxed);
            eprintln!("ERR_AGENTOS_NET_SOCKET_WAKE_UNKNOWN: unknown socket wake {event}");
            return;
        }
    };
    for target in targets {
        match target.publish_readiness(flags) {
            Ok(true) => {
                NET_TCP_TRACE_COUNTERS
                    .socket_read_push_sent
                    .fetch_add(1, Ordering::Relaxed);
            }
            Ok(false) => {}
            Err(error) => {
                NET_TCP_TRACE_COUNTERS
                    .socket_read_push_errors
                    .fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "ERR_AGENTOS_NET_SOCKET_WAKE: capability={} generation={} event={event}: {error}",
                    target.capability_id, target.capability_generation
                );
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the reader task receives explicit shared lifecycle flags owned by its socket"
)]
fn spawn_unix_socket_reader(
    runtime: agentos_runtime::RuntimeContext,
    stream: UnixStream,
    sender: AsyncCompletionSender<TcpSocketEvent>,
    event_pusher: Arc<SocketReadinessSubscribers>,
    read_event_notify: Arc<tokio::sync::Notify>,
    application_read_interest: Arc<AtomicBool>,
    application_read_notify: Arc<tokio::sync::Notify>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
    resources: Arc<ResourceLedger>,
    limits: ReactorIoLimits,
    fairness_identity: Arc<OnceLock<(u64, u64)>>,
    fairness_identity_committed: Arc<tokio::sync::Notify>,
) -> Result<(), SidecarError> {
    let (mut buffer, _read_buffer_reservation) =
        reserve_socket_read_buffer(&resources, limits.byte_quantum)?;
    let cancellation = runtime.clone();
    runtime
        .spawn(agentos_runtime::TaskClass::Socket, async move {
            let reader_runtime = cancellation.clone();
            let reader = async move {
                if let Err(error) = stream.set_nonblocking(true) {
                    send_async_socket_error_and_close(
                        &sender,
                        &event_pusher,
                        &close_notified,
                        io_error_code(&error),
                        error.to_string(),
                    )
                    .await;
                    read_event_notify.notify_one();
                    return;
                }
                let stream = match tokio::net::UnixStream::from_std(stream) {
                    Ok(stream) => stream,
                    Err(error) => {
                        send_async_socket_error_and_close(
                            &sender,
                            &event_pusher,
                            &close_notified,
                            io_error_code(&error),
                            error.to_string(),
                        )
                        .await;
                        read_event_notify.notify_one();
                        return;
                    }
                };
                loop {
                    while !application_read_interest.load(Ordering::Acquire) {
                        let notified = application_read_notify.notified();
                        if application_read_interest.load(Ordering::Acquire) {
                            break;
                        }
                        notified.await;
                    }
                    let ready = tokio::select! {
                        result = stream.readable() => result,
                        _ = application_read_notify.notified() => continue,
                    };
                    if let Err(error) = ready {
                        let code = io_error_code(&error);
                        send_async_socket_error_and_close(
                            &sender,
                            &event_pusher,
                            &close_notified,
                            code,
                            error.to_string(),
                        )
                        .await;
                        read_event_notify.notify_one();
                        break;
                    }
                    let turn = match acquire_plain_socket_fair_turn(
                        &reader_runtime,
                        limits,
                        &fairness_identity,
                        &fairness_identity_committed,
                    )
                    .await
                    {
                        Ok(turn) => turn,
                        Err(error) => {
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                Some(String::from("ERR_AGENTOS_FAIRNESS")),
                                error.to_string(),
                            )
                            .await;
                            read_event_notify.notify_one();
                            break;
                        }
                    };
                    let read_capacity = buffer.len().min(turn.allowance().bytes).max(1);
                    let read_result = stream.try_read(&mut buffer[..read_capacity]);
                    let used_bytes = read_result.as_ref().copied().unwrap_or(0);
                    if let Err(error) = turn.complete(FairBudget::new(1, used_bytes), false) {
                        send_async_socket_error_and_close(
                            &sender,
                            &event_pusher,
                            &close_notified,
                            Some(String::from("ERR_AGENTOS_FAIRNESS")),
                            error.to_string(),
                        )
                        .await;
                        read_event_notify.notify_one();
                        break;
                    }
                    match read_result {
                        Ok(0) => {
                            saw_remote_end.store(true, Ordering::SeqCst);
                            if sender.send(TcpSocketEvent::End).await.is_err() {
                                break;
                            }
                            push_socket_event(&event_pusher, "end");
                            read_event_notify.notify_one();
                            if saw_local_shutdown.load(Ordering::SeqCst)
                                && !close_notified.swap(true, Ordering::SeqCst)
                                && sender
                                    .send(TcpSocketEvent::Close { had_error: false })
                                    .await
                                    .is_ok()
                            {
                                push_socket_event(&event_pusher, "close");
                                read_event_notify.notify_one();
                            }
                            break;
                        }
                        Ok(bytes_read) => {
                            let Some(reservation) = reserve_socket_event_bytes_or_close(
                                &resources,
                                bytes_read,
                                &sender,
                                &event_pusher,
                                &close_notified,
                            )
                            .await
                            else {
                                read_event_notify.notify_one();
                                break;
                            };
                            if sender
                                .send(TcpSocketEvent::Data {
                                    bytes: buffer[..bytes_read].to_vec(),
                                    reservation: SharedReservation::new(reservation),
                                    source_reservations: Vec::new(),
                                })
                                .await
                                .is_err()
                            {
                                break;
                            }
                            push_socket_event(&event_pusher, "data");
                            read_event_notify.notify_one();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                        Err(error) => {
                            let code = io_error_code(&error);
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                code,
                                error.to_string(),
                            )
                            .await;
                            read_event_notify.notify_one();
                            break;
                        }
                    }
                }
            };
            tokio::select! {
                () = cancellation.admission_closed() => {}
                () = reader => {}
            }
        })
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    Ok(())
}
