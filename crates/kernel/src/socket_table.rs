use crate::poll::{PollEvents, POLLERR, POLLHUP, POLLIN, POLLOUT};
use crate::vfs::normalize_path;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, MutexGuard};

pub type SocketId = u64;
pub type SocketResult<T> = Result<T, SocketTableError>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InetSocketAddress {
    host: String,
    port: u16,
}

impl InetSocketAddress {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SocketDomain {
    Inet,
    Inet6,
    Unix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SocketType {
    Stream,
    Datagram,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SocketState {
    Created,
    Bound,
    Listening,
    Connected,
}

impl SocketState {
    pub const fn counts_as_listener(self) -> bool {
        matches!(self, Self::Listening)
    }

    pub const fn counts_as_connection(self) -> bool {
        matches!(self, Self::Connected)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketShutdown {
    Read,
    Write,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatagramSocketOption {
    ReuseAddr,
    ReusePort,
    Broadcast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketSpec {
    pub domain: SocketDomain,
    pub socket_type: SocketType,
}

impl SocketSpec {
    pub const fn new(domain: SocketDomain, socket_type: SocketType) -> Self {
        Self {
            domain,
            socket_type,
        }
    }

    pub const fn tcp() -> Self {
        Self::new(SocketDomain::Inet, SocketType::Stream)
    }

    pub const fn udp() -> Self {
        Self::new(SocketDomain::Inet, SocketType::Datagram)
    }

    pub const fn unix_stream() -> Self {
        Self::new(SocketDomain::Unix, SocketType::Stream)
    }

    pub const fn unix_datagram() -> Self {
        Self::new(SocketDomain::Unix, SocketType::Datagram)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketRecord {
    id: SocketId,
    owner_pid: u32,
    spec: SocketSpec,
    state: SocketState,
    local_address: Option<InetSocketAddress>,
    peer_address: Option<InetSocketAddress>,
    local_unix_path: Option<String>,
    peer_unix_path: Option<String>,
    listener_state: Option<ListenerState>,
    connection_state: Option<ConnectionState>,
    datagram_state: Option<DatagramState>,
}

impl SocketRecord {
    pub const fn id(&self) -> SocketId {
        self.id
    }

    pub const fn owner_pid(&self) -> u32 {
        self.owner_pid
    }

    pub const fn spec(&self) -> SocketSpec {
        self.spec
    }

    pub const fn state(&self) -> SocketState {
        self.state
    }

    pub fn local_address(&self) -> Option<&InetSocketAddress> {
        self.local_address.as_ref()
    }

    pub fn peer_address(&self) -> Option<&InetSocketAddress> {
        self.peer_address.as_ref()
    }

    pub fn local_unix_path(&self) -> Option<&str> {
        self.local_unix_path.as_deref()
    }

    pub fn peer_unix_path(&self) -> Option<&str> {
        self.peer_unix_path.as_deref()
    }

    pub fn listen_backlog(&self) -> Option<usize> {
        self.listener_state.as_ref().map(|state| state.backlog)
    }

    pub fn pending_accept_count(&self) -> usize {
        self.listener_state
            .as_ref()
            .map(|state| state.pending_accepts.len())
            .unwrap_or(0)
    }

    pub fn peer_socket_id(&self) -> Option<SocketId> {
        self.connection_state
            .as_ref()
            .and_then(|state| state.peer_socket_id)
    }

    pub fn buffered_read_bytes(&self) -> usize {
        self.connection_state
            .as_ref()
            .map(|state| state.recv_buffer.len())
            .unwrap_or(0)
    }

    pub fn read_shutdown(&self) -> bool {
        self.connection_state
            .as_ref()
            .map(|state| state.read_shutdown)
            .unwrap_or(false)
    }

    pub fn write_shutdown(&self) -> bool {
        self.connection_state
            .as_ref()
            .map(|state| state.write_shutdown)
            .unwrap_or(false)
    }

    pub fn peer_write_shutdown(&self) -> bool {
        self.connection_state
            .as_ref()
            .map(|state| state.peer_write_shutdown)
            .unwrap_or(false)
    }

    pub fn queued_datagrams(&self) -> usize {
        self.datagram_state
            .as_ref()
            .map(|state| state.recv_queue.len())
            .unwrap_or(0)
    }

    pub fn reuse_address(&self) -> bool {
        self.datagram_state
            .as_ref()
            .map(|state| state.reuse_addr)
            .unwrap_or(false)
    }

    pub fn reuse_port(&self) -> bool {
        self.datagram_state
            .as_ref()
            .map(|state| state.reuse_port)
            .unwrap_or(false)
    }

    pub fn broadcast_enabled(&self) -> bool {
        self.datagram_state
            .as_ref()
            .map(|state| state.broadcast)
            .unwrap_or(false)
    }

    pub fn multicast_membership_count(&self) -> usize {
        self.datagram_state
            .as_ref()
            .map(|state| state.multicast_memberships.len())
            .unwrap_or(0)
    }

    pub fn has_multicast_membership(&self, membership: &SocketMulticastMembership) -> bool {
        self.datagram_state
            .as_ref()
            .map(|state| state.multicast_memberships.contains(membership))
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedDatagram {
    source_address: Option<InetSocketAddress>,
    payload: Vec<u8>,
}

impl ReceivedDatagram {
    pub fn source_address(&self) -> Option<&InetSocketAddress> {
        self.source_address.as_ref()
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn into_parts(self) -> (Option<InetSocketAddress>, Vec<u8>) {
        (self.source_address, self.payload)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SocketTableSnapshot {
    pub sockets: usize,
    pub listeners: usize,
    pub connections: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SocketMulticastMembership {
    group_address: String,
    interface_address: Option<String>,
}

impl SocketMulticastMembership {
    pub fn new(group_address: impl Into<String>, interface_address: Option<String>) -> Self {
        Self {
            group_address: group_address.into(),
            interface_address,
        }
    }

    pub fn group_address(&self) -> &str {
        &self.group_address
    }

    pub fn interface_address(&self) -> Option<&str> {
        self.interface_address.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketTableError {
    code: &'static str,
    message: String,
}

impl SocketTableError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn not_found(socket_id: SocketId) -> Self {
        Self {
            code: "ENOENT",
            message: format!("no such socket {socket_id}"),
        }
    }

    fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            code: "EINVAL",
            message: message.into(),
        }
    }

    fn address_in_use(message: impl Into<String>) -> Self {
        Self {
            code: "EADDRINUSE",
            message: message.into(),
        }
    }

    fn address_not_available(message: impl Into<String>) -> Self {
        Self {
            code: "EADDRNOTAVAIL",
            message: message.into(),
        }
    }

    fn not_found_address(message: impl Into<String>) -> Self {
        Self {
            code: "ECONNREFUSED",
            message: message.into(),
        }
    }

    fn would_block(message: impl Into<String>) -> Self {
        Self {
            code: "EAGAIN",
            message: message.into(),
        }
    }

    fn not_connected(message: impl Into<String>) -> Self {
        Self {
            code: "ENOTCONN",
            message: message.into(),
        }
    }

    fn broken_pipe(message: impl Into<String>) -> Self {
        Self {
            code: "EPIPE",
            message: message.into(),
        }
    }
}

impl fmt::Display for SocketTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for SocketTableError {}

#[derive(Debug, Default)]
struct SocketTableState {
    sockets: BTreeMap<SocketId, SocketRecord>,
    by_owner: BTreeMap<u32, BTreeSet<SocketId>>,
    bound_inet_streams: BTreeMap<InetSocketAddress, SocketId>,
    bound_inet_datagrams: BTreeMap<InetSocketAddress, BTreeSet<SocketId>>,
    bound_unix_streams: BTreeMap<String, SocketId>,
    multicast_groups: BTreeMap<SocketMulticastMembership, BTreeSet<SocketId>>,
    next_socket_id: SocketId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListenerState {
    backlog: usize,
    pending_accepts: VecDeque<PendingConnection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ConnectionState {
    peer_socket_id: Option<SocketId>,
    recv_buffer: VecDeque<u8>,
    read_shutdown: bool,
    write_shutdown: bool,
    peer_write_shutdown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingConnection {
    peer_address: Option<InetSocketAddress>,
    peer_unix_path: Option<String>,
    accepted_socket_id: Option<SocketId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct DatagramState {
    recv_queue: VecDeque<QueuedDatagram>,
    reuse_addr: bool,
    reuse_port: bool,
    broadcast: bool,
    multicast_memberships: BTreeSet<SocketMulticastMembership>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueuedDatagram {
    source_address: Option<InetSocketAddress>,
    payload: Vec<u8>,
}

#[derive(Debug, Default)]
struct SocketTableInner {
    state: Mutex<SocketTableState>,
}

#[derive(Debug, Clone, Default)]
pub struct SocketTable {
    inner: Arc<SocketTableInner>,
}

impl SocketTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allocate(&self, owner_pid: u32, spec: SocketSpec) -> SocketRecord {
        self.allocate_with_state(owner_pid, spec, SocketState::Created)
    }

    pub fn allocate_with_state(
        &self,
        owner_pid: u32,
        spec: SocketSpec,
        state: SocketState,
    ) -> SocketRecord {
        let mut table = lock_or_recover(&self.inner.state);
        let socket_id = next_socket_id(&mut table);
        let record = SocketRecord {
            id: socket_id,
            owner_pid,
            spec,
            state,
            local_address: None,
            peer_address: None,
            local_unix_path: None,
            peer_unix_path: None,
            listener_state: None,
            connection_state: default_connection_state(spec, state),
            datagram_state: default_datagram_state(spec),
        };
        table.sockets.insert(socket_id, record.clone());
        table
            .by_owner
            .entry(owner_pid)
            .or_default()
            .insert(socket_id);
        record
    }

    pub fn get(&self, socket_id: SocketId) -> Option<SocketRecord> {
        lock_or_recover(&self.inner.state)
            .sockets
            .get(&socket_id)
            .cloned()
    }

    pub fn update_state(
        &self,
        socket_id: SocketId,
        new_state: SocketState,
    ) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get_mut(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        validate_state_transition(record.state, new_state)?;
        record.state = new_state;
        if new_state != SocketState::Listening {
            record.listener_state = None;
        }
        if new_state == SocketState::Connected && supports_connection_lifecycle(record.spec) {
            record
                .connection_state
                .get_or_insert_with(ConnectionState::default);
        } else if new_state != SocketState::Connected {
            record.connection_state = None;
        }
        Ok(record.clone())
    }

    pub fn bind_inet(
        &self,
        socket_id: SocketId,
        address: InetSocketAddress,
    ) -> SocketResult<SocketRecord> {
        let address = normalize_inet_address(address);
        let mut table = lock_or_recover(&self.inner.state);
        let existing = table
            .sockets
            .get(&socket_id)
            .cloned()
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        if !supports_inet_bind(existing.spec) {
            return Err(SocketTableError::invalid_argument(format!(
                "socket {socket_id} is not an INET socket"
            )));
        }
        let conflicting_ids =
            lookup_conflicting_bound_inet_socket_ids(&table, existing.spec, &address);
        if has_incompatible_inet_bind_conflict(&table, &existing, &conflicting_ids) {
            return Err(SocketTableError::address_in_use(format!(
                "address {}:{} is already bound",
                address.host(),
                address.port()
            )));
        }
        let cloned = {
            let record = table
                .sockets
                .get_mut(&socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;

            match record.state {
                SocketState::Created => {}
                SocketState::Bound if record.local_address.as_ref() == Some(&address) => {
                    return Ok(record.clone());
                }
                SocketState::Bound | SocketState::Listening | SocketState::Connected => {
                    return Err(SocketTableError::invalid_argument(format!(
                        "socket {socket_id} cannot bind in state {:?}",
                        record.state
                    )));
                }
            }

            record.local_address = Some(address.clone());
            record.peer_address = None;
            record.local_unix_path = None;
            record.peer_unix_path = None;
            record.listener_state = None;
            record.connection_state = None;
            record.state = SocketState::Bound;
            record.clone()
        };
        register_bound_inet_socket(&mut table, cloned.spec, address, socket_id);
        Ok(cloned)
    }

    pub fn set_datagram_socket_option(
        &self,
        socket_id: SocketId,
        option: DatagramSocketOption,
        enabled: bool,
    ) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get_mut(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        let datagram_state = datagram_state_mut(record)?;

        match option {
            DatagramSocketOption::ReuseAddr => datagram_state.reuse_addr = enabled,
            DatagramSocketOption::ReusePort => datagram_state.reuse_port = enabled,
            DatagramSocketOption::Broadcast => datagram_state.broadcast = enabled,
        }

        Ok(record.clone())
    }

    pub fn add_multicast_membership(
        &self,
        socket_id: SocketId,
        membership: SocketMulticastMembership,
    ) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let normalized_membership = {
            let record = table
                .sockets
                .get(&socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;
            validate_multicast_socket(record)?;
            normalize_multicast_membership(record.spec, membership)?
        };

        let cloned = {
            let record = table
                .sockets
                .get_mut(&socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;
            let datagram_state = datagram_state_mut(record)?;
            datagram_state
                .multicast_memberships
                .insert(normalized_membership.clone());
            record.clone()
        };

        table
            .multicast_groups
            .entry(normalized_membership)
            .or_default()
            .insert(socket_id);
        Ok(cloned)
    }

    pub fn drop_multicast_membership(
        &self,
        socket_id: SocketId,
        membership: SocketMulticastMembership,
    ) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let normalized_membership = {
            let record = table
                .sockets
                .get(&socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;
            validate_multicast_socket(record)?;
            normalize_multicast_membership(record.spec, membership)?
        };

        let cloned = {
            let record = table
                .sockets
                .get_mut(&socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;
            let datagram_state = datagram_state_mut(record)?;
            if !datagram_state
                .multicast_memberships
                .remove(&normalized_membership)
            {
                return Err(SocketTableError::address_not_available(format!(
                    "socket {socket_id} has not joined multicast group {}",
                    normalized_membership.group_address()
                )));
            }
            record.clone()
        };

        if let Some(members) = table.multicast_groups.get_mut(&normalized_membership) {
            members.remove(&socket_id);
            if members.is_empty() {
                table.multicast_groups.remove(&normalized_membership);
            }
        }

        Ok(cloned)
    }

    pub fn bind_unix(
        &self,
        socket_id: SocketId,
        path: impl Into<String>,
    ) -> SocketResult<SocketRecord> {
        let path = normalize_unix_socket_path(path.into())?;
        let mut table = lock_or_recover(&self.inner.state);
        let existing = table
            .sockets
            .get(&socket_id)
            .cloned()
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        if !supports_unix_stream_lifecycle(existing.spec) {
            return Err(SocketTableError::invalid_argument(format!(
                "socket {socket_id} is not a Unix stream socket"
            )));
        }
        let existing_id = table.bound_unix_streams.get(&path).copied();
        let cloned = {
            let record = table
                .sockets
                .get_mut(&socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;

            if let Some(bound_socket_id) = existing_id {
                if bound_socket_id != socket_id {
                    return Err(SocketTableError::address_in_use(format!(
                        "path {path} is already bound"
                    )));
                }
            }

            match record.state {
                SocketState::Created => {}
                SocketState::Bound if record.local_unix_path.as_deref() == Some(path.as_str()) => {
                    return Ok(record.clone());
                }
                SocketState::Bound | SocketState::Listening | SocketState::Connected => {
                    return Err(SocketTableError::invalid_argument(format!(
                        "socket {socket_id} cannot bind in state {:?}",
                        record.state
                    )));
                }
            }

            record.local_address = None;
            record.peer_address = None;
            record.local_unix_path = Some(path.clone());
            record.peer_unix_path = None;
            record.listener_state = None;
            record.connection_state = None;
            record.state = SocketState::Bound;
            record.clone()
        };
        table.bound_unix_streams.insert(path, socket_id);
        Ok(cloned)
    }

    pub fn listen(&self, socket_id: SocketId, backlog: usize) -> SocketResult<SocketRecord> {
        if backlog == 0 {
            return Err(SocketTableError::invalid_argument(
                "listener backlog must be greater than zero",
            ));
        }

        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get_mut(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;

        if !supports_listener_lifecycle(record.spec) {
            return Err(SocketTableError::invalid_argument(format!(
                "socket {socket_id} is not a stream socket"
            )));
        }
        if record.state != SocketState::Bound || !has_bound_endpoint(record) {
            return Err(SocketTableError::invalid_argument(format!(
                "socket {socket_id} must be bound before listen"
            )));
        }

        record.state = SocketState::Listening;
        record.listener_state = Some(ListenerState {
            backlog,
            pending_accepts: VecDeque::new(),
        });
        Ok(record.clone())
    }

    pub fn enqueue_incoming_tcp_connection(
        &self,
        listener_socket_id: SocketId,
        peer_address: InetSocketAddress,
    ) -> SocketResult<()> {
        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get_mut(&listener_socket_id)
            .ok_or_else(|| SocketTableError::not_found(listener_socket_id))?;

        if record.state != SocketState::Listening {
            return Err(SocketTableError::invalid_argument(format!(
                "socket {listener_socket_id} is not listening"
            )));
        }

        let listener_state = record.listener_state.as_mut().ok_or_else(|| {
            SocketTableError::invalid_argument(format!(
                "socket {listener_socket_id} has no listener state"
            ))
        })?;

        if listener_state.pending_accepts.len() >= listener_state.backlog {
            return Err(SocketTableError::would_block(format!(
                "listener {listener_socket_id} backlog is full"
            )));
        }

        listener_state.pending_accepts.push_back(PendingConnection {
            peer_address: Some(peer_address),
            peer_unix_path: None,
            accepted_socket_id: None,
        });
        Ok(())
    }

    pub fn accept(&self, listener_socket_id: SocketId) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let (owner_pid, spec, local_address, local_unix_path, pending) = {
            let record = table
                .sockets
                .get_mut(&listener_socket_id)
                .ok_or_else(|| SocketTableError::not_found(listener_socket_id))?;

            if record.state != SocketState::Listening {
                return Err(SocketTableError::invalid_argument(format!(
                    "socket {listener_socket_id} is not listening"
                )));
            }

            let listener_state = record.listener_state.as_mut().ok_or_else(|| {
                SocketTableError::invalid_argument(format!(
                    "socket {listener_socket_id} has no listener state"
                ))
            })?;
            let pending = listener_state.pending_accepts.pop_front().ok_or_else(|| {
                SocketTableError::would_block(format!(
                    "listener {listener_socket_id} has no pending connections"
                ))
            })?;

            (
                record.owner_pid,
                record.spec,
                record.local_address.clone(),
                record.local_unix_path.clone(),
                pending,
            )
        };

        if let Some(accepted_socket_id) = pending.accepted_socket_id {
            return table
                .sockets
                .get(&accepted_socket_id)
                .cloned()
                .ok_or_else(|| SocketTableError::not_found(accepted_socket_id));
        }

        let socket_id = next_socket_id(&mut table);
        let record = SocketRecord {
            id: socket_id,
            owner_pid,
            spec,
            state: SocketState::Connected,
            local_address,
            peer_address: pending.peer_address,
            local_unix_path,
            peer_unix_path: pending.peer_unix_path,
            listener_state: None,
            connection_state: default_connection_state(spec, SocketState::Connected),
            datagram_state: default_datagram_state(spec),
        };
        table.sockets.insert(socket_id, record.clone());
        table
            .by_owner
            .entry(owner_pid)
            .or_default()
            .insert(socket_id);
        Ok(record)
    }

    pub fn connect_pair(
        &self,
        socket_id: SocketId,
        peer_socket_id: SocketId,
    ) -> SocketResult<(SocketRecord, SocketRecord)> {
        if socket_id == peer_socket_id {
            return Err(SocketTableError::invalid_argument(
                "socket cannot connect to itself",
            ));
        }

        let mut table = lock_or_recover(&self.inner.state);
        let mut socket = table
            .sockets
            .remove(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        let Some(mut peer) = table.sockets.remove(&peer_socket_id) else {
            table.sockets.insert(socket_id, socket);
            return Err(SocketTableError::not_found(peer_socket_id));
        };

        if let Err(error) = validate_connect_pair(&socket, &peer) {
            table.sockets.insert(socket_id, socket);
            table.sockets.insert(peer_socket_id, peer);
            return Err(error);
        }

        socket.state = SocketState::Connected;
        socket.peer_address = peer.local_address.clone();
        socket.peer_unix_path = peer.local_unix_path.clone();
        socket.listener_state = None;
        socket.connection_state = Some(ConnectionState {
            peer_socket_id: Some(peer_socket_id),
            ..ConnectionState::default()
        });

        peer.state = SocketState::Connected;
        peer.peer_address = socket.local_address.clone();
        peer.peer_unix_path = socket.local_unix_path.clone();
        peer.listener_state = None;
        peer.connection_state = Some(ConnectionState {
            peer_socket_id: Some(socket_id),
            ..ConnectionState::default()
        });

        let socket_clone = socket.clone();
        let peer_clone = peer.clone();
        table.sockets.insert(socket_id, socket);
        table.sockets.insert(peer_socket_id, peer);
        Ok((socket_clone, peer_clone))
    }

    pub fn find_bound_inet_socket(
        &self,
        spec: SocketSpec,
        address: &InetSocketAddress,
    ) -> Option<SocketRecord> {
        let address = normalize_inet_address(address.clone());
        let table = lock_or_recover(&self.inner.state);
        let socket_id = lookup_bound_inet_socket(&table, spec, &address)?;
        table.sockets.get(&socket_id).cloned()
    }

    pub fn connect_to_bound_inet_stream(
        &self,
        socket_id: SocketId,
        target_address: InetSocketAddress,
    ) -> SocketResult<()> {
        let target_address = normalize_inet_address(target_address);
        let mut table = lock_or_recover(&self.inner.state);
        let listener_socket_id =
            lookup_bound_inet_socket_in_table(&table.bound_inet_streams, &target_address)
                .ok_or_else(|| {
                    SocketTableError::not_found_address(format!(
                        "no listening socket bound at {}:{}",
                        target_address.host(),
                        target_address.port()
                    ))
                })?;

        if socket_id == listener_socket_id {
            return Err(SocketTableError::invalid_argument(
                "socket cannot connect to its own listening endpoint",
            ));
        }

        let mut client = table
            .sockets
            .remove(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        let accepted_socket_id = next_socket_id(&mut table);

        let result = (|| {
            let listener = table
                .sockets
                .get_mut(&listener_socket_id)
                .ok_or_else(|| SocketTableError::not_found(listener_socket_id))?;
            validate_connect_to_listener(&client, listener)?;

            let listener_state = listener.listener_state.as_mut().ok_or_else(|| {
                SocketTableError::invalid_argument(format!(
                    "socket {listener_socket_id} has no listener state"
                ))
            })?;
            if listener_state.pending_accepts.len() >= listener_state.backlog {
                return Err(SocketTableError::would_block(format!(
                    "listener {listener_socket_id} backlog is full"
                )));
            }

            let accepted = SocketRecord {
                id: accepted_socket_id,
                owner_pid: listener.owner_pid,
                spec: listener.spec,
                state: SocketState::Connected,
                local_address: listener.local_address.clone(),
                peer_address: client.local_address.clone(),
                local_unix_path: None,
                peer_unix_path: None,
                listener_state: None,
                connection_state: Some(ConnectionState {
                    peer_socket_id: Some(socket_id),
                    ..ConnectionState::default()
                }),
                datagram_state: default_datagram_state(listener.spec),
            };

            listener_state.pending_accepts.push_back(PendingConnection {
                peer_address: client.local_address.clone(),
                peer_unix_path: None,
                accepted_socket_id: Some(accepted_socket_id),
            });

            client.state = SocketState::Connected;
            client.peer_address = listener.local_address.clone();
            client.peer_unix_path = None;
            client.listener_state = None;
            client.connection_state = Some(ConnectionState {
                peer_socket_id: Some(accepted_socket_id),
                ..ConnectionState::default()
            });

            Ok(accepted)
        })();

        match result {
            Ok(accepted) => {
                table.sockets.insert(socket_id, client);
                table.sockets.insert(accepted_socket_id, accepted.clone());
                table
                    .by_owner
                    .entry(accepted.owner_pid)
                    .or_default()
                    .insert(accepted_socket_id);
                Ok(())
            }
            Err(error) => {
                table.sockets.insert(socket_id, client);
                Err(error)
            }
        }
    }

    pub fn find_bound_unix_socket(&self, path: &str) -> Option<SocketRecord> {
        let path = normalize_unix_socket_path(path).ok()?;
        let table = lock_or_recover(&self.inner.state);
        let socket_id = table.bound_unix_streams.get(&path).copied()?;
        table.sockets.get(&socket_id).cloned()
    }

    pub fn connect_to_bound_unix_stream(
        &self,
        socket_id: SocketId,
        target_path: impl Into<String>,
    ) -> SocketResult<()> {
        let target_path = normalize_unix_socket_path(target_path.into())?;
        let mut table = lock_or_recover(&self.inner.state);
        let listener_socket_id = table
            .bound_unix_streams
            .get(&target_path)
            .copied()
            .ok_or_else(|| {
                SocketTableError::not_found_address(format!(
                    "no listening socket bound at path {target_path}"
                ))
            })?;

        if socket_id == listener_socket_id {
            return Err(SocketTableError::invalid_argument(
                "socket cannot connect to its own listening endpoint",
            ));
        }

        let mut client = table
            .sockets
            .remove(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        let accepted_socket_id = next_socket_id(&mut table);

        let result = (|| {
            let listener = table
                .sockets
                .get_mut(&listener_socket_id)
                .ok_or_else(|| SocketTableError::not_found(listener_socket_id))?;
            validate_connect_to_listener(&client, listener)?;

            let listener_state = listener.listener_state.as_mut().ok_or_else(|| {
                SocketTableError::invalid_argument(format!(
                    "socket {listener_socket_id} has no listener state"
                ))
            })?;
            if listener_state.pending_accepts.len() >= listener_state.backlog {
                return Err(SocketTableError::would_block(format!(
                    "listener {listener_socket_id} backlog is full"
                )));
            }

            let accepted = SocketRecord {
                id: accepted_socket_id,
                owner_pid: listener.owner_pid,
                spec: listener.spec,
                state: SocketState::Connected,
                local_address: None,
                peer_address: None,
                local_unix_path: listener.local_unix_path.clone(),
                peer_unix_path: client.local_unix_path.clone(),
                listener_state: None,
                connection_state: Some(ConnectionState {
                    peer_socket_id: Some(socket_id),
                    ..ConnectionState::default()
                }),
                datagram_state: default_datagram_state(listener.spec),
            };

            listener_state.pending_accepts.push_back(PendingConnection {
                peer_address: None,
                peer_unix_path: client.local_unix_path.clone(),
                accepted_socket_id: Some(accepted_socket_id),
            });

            client.state = SocketState::Connected;
            client.peer_address = None;
            client.peer_unix_path = listener.local_unix_path.clone();
            client.listener_state = None;
            client.connection_state = Some(ConnectionState {
                peer_socket_id: Some(accepted_socket_id),
                ..ConnectionState::default()
            });

            Ok(accepted)
        })();

        match result {
            Ok(accepted) => {
                table.sockets.insert(socket_id, client);
                table.sockets.insert(accepted_socket_id, accepted.clone());
                table
                    .by_owner
                    .entry(accepted.owner_pid)
                    .or_default()
                    .insert(accepted_socket_id);
                Ok(())
            }
            Err(error) => {
                table.sockets.insert(socket_id, client);
                Err(error)
            }
        }
    }

    pub fn send_to_bound_udp_socket(
        &self,
        socket_id: SocketId,
        target_address: InetSocketAddress,
        data: &[u8],
    ) -> SocketResult<usize> {
        let target_address = normalize_inet_address(target_address);
        let mut table = lock_or_recover(&self.inner.state);
        let sender = table
            .sockets
            .get(&socket_id)
            .cloned()
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        validate_bound_udp_sender(&sender)?;

        let receiver_socket_id = table
            .bound_inet_datagrams
            .get(&target_address)
            .and_then(|socket_ids| socket_ids.first().copied())
            .ok_or_else(|| {
                SocketTableError::not_found_address(format!(
                    "no UDP socket bound at {}:{}",
                    target_address.host(),
                    target_address.port()
                ))
            })?;
        let receiver = table
            .sockets
            .get_mut(&receiver_socket_id)
            .ok_or_else(|| SocketTableError::not_found(receiver_socket_id))?;
        validate_bound_udp_receiver(receiver)?;

        let datagram_state = receiver.datagram_state.as_mut().ok_or_else(|| {
            SocketTableError::invalid_argument(format!(
                "socket {receiver_socket_id} does not support datagrams"
            ))
        })?;
        datagram_state.recv_queue.push_back(QueuedDatagram {
            source_address: sender.local_address.clone(),
            payload: data.to_vec(),
        });
        Ok(data.len())
    }

    pub fn recv_datagram(
        &self,
        socket_id: SocketId,
        max_bytes: usize,
    ) -> SocketResult<Option<ReceivedDatagram>> {
        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get_mut(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        validate_bound_udp_receiver(record)?;

        let datagram_state = record.datagram_state.as_mut().ok_or_else(|| {
            SocketTableError::invalid_argument(format!(
                "socket {socket_id} does not support datagrams"
            ))
        })?;
        let Some(datagram) = datagram_state.recv_queue.pop_front() else {
            return Err(SocketTableError::would_block(format!(
                "socket {socket_id} has no queued datagrams"
            )));
        };

        let payload = if datagram.payload.len() > max_bytes {
            datagram.payload[..max_bytes].to_vec()
        } else {
            datagram.payload
        };
        Ok(Some(ReceivedDatagram {
            source_address: datagram.source_address,
            payload,
        }))
    }

    pub fn poll(&self, socket_id: SocketId, requested: PollEvents) -> SocketResult<PollEvents> {
        let table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;

        let mut events = PollEvents::empty();
        match record.state {
            SocketState::Listening => {
                if requested.intersects(POLLIN) && record.pending_accept_count() > 0 {
                    events |= POLLIN;
                }
            }
            SocketState::Connected => {
                let connection = record.connection_state.as_ref().ok_or_else(|| {
                    SocketTableError::not_connected(format!("socket {socket_id} is not connected"))
                })?;
                let peer = connection
                    .peer_socket_id
                    .and_then(|peer_socket_id| table.sockets.get(&peer_socket_id));

                if requested.intersects(POLLIN) && !connection.recv_buffer.is_empty() {
                    events |= POLLIN;
                }
                if connection.peer_write_shutdown || peer.is_none() {
                    events |= POLLHUP;
                }

                if requested.intersects(POLLOUT) && !connection.write_shutdown {
                    if peer
                        .and_then(|peer| peer.connection_state.as_ref())
                        .map(|peer_connection| peer_connection.read_shutdown)
                        .unwrap_or(true)
                    {
                        events |= POLLERR;
                    } else {
                        events |= POLLOUT;
                    }
                }
            }
            SocketState::Bound if supports_inet_datagram_lifecycle(record.spec) => {
                let datagram_state = record.datagram_state.as_ref().ok_or_else(|| {
                    SocketTableError::invalid_argument(format!(
                        "socket {socket_id} does not support datagrams"
                    ))
                })?;
                if requested.intersects(POLLIN) && !datagram_state.recv_queue.is_empty() {
                    events |= POLLIN;
                }
                if requested.intersects(POLLOUT) {
                    events |= POLLOUT;
                }
            }
            SocketState::Created | SocketState::Bound => {}
        }

        Ok(events)
    }

    pub fn write(&self, socket_id: SocketId, data: &[u8]) -> SocketResult<usize> {
        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get(&socket_id)
            .cloned()
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        let connection = record.connection_state.as_ref().ok_or_else(|| {
            SocketTableError::not_connected(format!("socket {socket_id} is not connected"))
        })?;
        if record.state != SocketState::Connected {
            return Err(SocketTableError::not_connected(format!(
                "socket {socket_id} is not connected"
            )));
        }
        if connection.write_shutdown {
            return Err(SocketTableError::broken_pipe(format!(
                "socket {socket_id} write side is shut down"
            )));
        }

        let peer_socket_id = connection.peer_socket_id.ok_or_else(|| {
            SocketTableError::broken_pipe(format!("socket {socket_id} peer is closed"))
        })?;
        let peer = table.sockets.get_mut(&peer_socket_id).ok_or_else(|| {
            SocketTableError::broken_pipe(format!("socket {socket_id} peer is closed"))
        })?;
        let peer_connection = peer.connection_state.as_mut().ok_or_else(|| {
            SocketTableError::broken_pipe(format!("socket {socket_id} peer is closed"))
        })?;
        if peer_connection.read_shutdown {
            return Err(SocketTableError::broken_pipe(format!(
                "socket {peer_socket_id} read side is shut down"
            )));
        }

        peer_connection.recv_buffer.extend(data.iter().copied());
        Ok(data.len())
    }

    pub fn read(&self, socket_id: SocketId, max_bytes: usize) -> SocketResult<Option<Vec<u8>>> {
        if max_bytes == 0 {
            return Ok(Some(Vec::new()));
        }

        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get(&socket_id)
            .cloned()
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        if record.state != SocketState::Connected {
            return Err(SocketTableError::not_connected(format!(
                "socket {socket_id} is not connected"
            )));
        }

        let connection = record.connection_state.as_ref().ok_or_else(|| {
            SocketTableError::not_connected(format!("socket {socket_id} is not connected"))
        })?;
        if connection.read_shutdown {
            return Ok(None);
        }
        if !connection.recv_buffer.is_empty() {
            let record = table
                .sockets
                .get_mut(&socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;
            let connection = record.connection_state.as_mut().ok_or_else(|| {
                SocketTableError::not_connected(format!("socket {socket_id} is not connected"))
            })?;
            let read_len = connection.recv_buffer.len().min(max_bytes);
            let bytes = connection.recv_buffer.drain(..read_len).collect::<Vec<_>>();
            return Ok(Some(bytes));
        }

        let peer_open = connection
            .peer_socket_id
            .map(|peer_socket_id| table.sockets.contains_key(&peer_socket_id))
            .unwrap_or(false);
        if connection.peer_write_shutdown || !peer_open {
            return Ok(None);
        }

        Err(SocketTableError::would_block(format!(
            "socket {socket_id} has no readable data"
        )))
    }

    pub fn shutdown(&self, socket_id: SocketId, how: SocketShutdown) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .remove(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;

        if record.state != SocketState::Connected {
            table.sockets.insert(socket_id, record);
            return Err(SocketTableError::not_connected(format!(
                "socket {socket_id} is not connected"
            )));
        }

        let Some(mut connection) = record.connection_state.clone() else {
            table.sockets.insert(socket_id, record);
            return Err(SocketTableError::not_connected(format!(
                "socket {socket_id} is not connected"
            )));
        };

        if matches!(how, SocketShutdown::Read | SocketShutdown::Both) {
            connection.recv_buffer.clear();
            connection.read_shutdown = true;
        }
        if matches!(how, SocketShutdown::Write | SocketShutdown::Both) {
            connection.write_shutdown = true;
            if let Some(peer_socket_id) = connection.peer_socket_id {
                if let Some(peer) = table.sockets.get_mut(&peer_socket_id) {
                    if let Some(peer_connection) = peer.connection_state.as_mut() {
                        peer_connection.peer_write_shutdown = true;
                    }
                }
            }
        }

        let mut record = record;
        record.connection_state = Some(connection);
        let cloned = record.clone();
        table.sockets.insert(socket_id, record);
        Ok(cloned)
    }

    pub fn remove(&self, socket_id: SocketId) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        remove_socket(&mut table, socket_id).ok_or_else(|| SocketTableError::not_found(socket_id))
    }

    pub fn remove_all_for_pid(&self, owner_pid: u32) -> Vec<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let Some(socket_ids) = table.by_owner.remove(&owner_pid) else {
            return Vec::new();
        };

        socket_ids
            .into_iter()
            .filter_map(|socket_id| remove_socket(&mut table, socket_id))
            .collect()
    }

    pub fn snapshot(&self) -> SocketTableSnapshot {
        let table = lock_or_recover(&self.inner.state);
        let mut snapshot = SocketTableSnapshot {
            sockets: table.sockets.len(),
            ..SocketTableSnapshot::default()
        };
        for record in table.sockets.values() {
            if record.state.counts_as_listener() {
                snapshot.listeners += 1;
            }
            if record.state.counts_as_connection() {
                snapshot.connections += 1;
            }
        }
        snapshot
    }
}

fn next_socket_id(table: &mut SocketTableState) -> SocketId {
    if table.next_socket_id == 0 {
        table.next_socket_id = 1;
    }
    let socket_id = table.next_socket_id;
    table.next_socket_id = table.next_socket_id.saturating_add(1);
    socket_id
}

fn validate_state_transition(current: SocketState, next: SocketState) -> SocketResult<()> {
    if current == SocketState::Connected && next != SocketState::Connected {
        return Err(SocketTableError::invalid_argument(format!(
            "invalid socket state transition from {current:?} to {next:?}"
        )));
    }
    Ok(())
}

fn validate_connect_pair(socket: &SocketRecord, peer: &SocketRecord) -> SocketResult<()> {
    if !supports_connection_lifecycle(socket.spec) {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} does not support stream connections",
            socket.id
        )));
    }
    if !supports_connection_lifecycle(peer.spec) {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} does not support stream connections",
            peer.id
        )));
    }
    if !matches!(socket.state, SocketState::Created | SocketState::Bound) {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} cannot connect in state {:?}",
            socket.id, socket.state
        )));
    }
    if !matches!(peer.state, SocketState::Created | SocketState::Bound) {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} cannot connect in state {:?}",
            peer.id, peer.state
        )));
    }
    Ok(())
}

fn default_connection_state(spec: SocketSpec, state: SocketState) -> Option<ConnectionState> {
    if state == SocketState::Connected && supports_connection_lifecycle(spec) {
        Some(ConnectionState::default())
    } else {
        None
    }
}

fn default_datagram_state(spec: SocketSpec) -> Option<DatagramState> {
    if supports_inet_datagram_lifecycle(spec) {
        Some(DatagramState::default())
    } else {
        None
    }
}

fn supports_connection_lifecycle(spec: SocketSpec) -> bool {
    matches!(spec.socket_type, SocketType::Stream)
}

fn supports_listener_lifecycle(spec: SocketSpec) -> bool {
    matches!(spec.socket_type, SocketType::Stream)
        && matches!(
            spec.domain,
            SocketDomain::Inet | SocketDomain::Inet6 | SocketDomain::Unix
        )
}

fn supports_inet_bind(spec: SocketSpec) -> bool {
    matches!(spec.domain, SocketDomain::Inet | SocketDomain::Inet6)
        && matches!(spec.socket_type, SocketType::Stream | SocketType::Datagram)
}

fn supports_unix_stream_lifecycle(spec: SocketSpec) -> bool {
    matches!(spec.socket_type, SocketType::Stream) && matches!(spec.domain, SocketDomain::Unix)
}

fn supports_inet_stream_lifecycle(spec: SocketSpec) -> bool {
    matches!(spec.socket_type, SocketType::Stream)
        && matches!(spec.domain, SocketDomain::Inet | SocketDomain::Inet6)
}

fn supports_inet_datagram_lifecycle(spec: SocketSpec) -> bool {
    matches!(spec.socket_type, SocketType::Datagram)
        && matches!(spec.domain, SocketDomain::Inet | SocketDomain::Inet6)
}

fn lookup_conflicting_bound_inet_socket_ids(
    table: &SocketTableState,
    spec: SocketSpec,
    address: &InetSocketAddress,
) -> Vec<SocketId> {
    if supports_inet_stream_lifecycle(spec) {
        table
            .bound_inet_streams
            .iter()
            .find_map(|(bound_address, socket_id)| {
                inet_stream_bind_addresses_overlap(bound_address, address).then_some(*socket_id)
            })
            .into_iter()
            .collect()
    } else if supports_inet_datagram_lifecycle(spec) {
        table
            .bound_inet_datagrams
            .iter()
            .filter(|(bound_address, _)| inet_stream_bind_addresses_overlap(bound_address, address))
            .flat_map(|(_, socket_ids)| socket_ids.iter().copied())
            .collect()
    } else {
        Vec::new()
    }
}

fn lookup_bound_inet_socket(
    table: &SocketTableState,
    spec: SocketSpec,
    address: &InetSocketAddress,
) -> Option<SocketId> {
    if supports_inet_stream_lifecycle(spec) {
        lookup_bound_inet_socket_in_table(&table.bound_inet_streams, address)
    } else if supports_inet_datagram_lifecycle(spec) {
        lookup_bound_inet_datagram_socket_in_table(&table.bound_inet_datagrams, address)
    } else {
        None
    }
}

fn inet_stream_bind_addresses_overlap(
    existing: &InetSocketAddress,
    requested: &InetSocketAddress,
) -> bool {
    if existing == requested {
        return true;
    }

    wildcard_inet_address(existing).as_ref() == Some(requested)
        || wildcard_inet_address(requested).as_ref() == Some(existing)
}

fn lookup_bound_inet_socket_in_table(
    sockets: &BTreeMap<InetSocketAddress, SocketId>,
    address: &InetSocketAddress,
) -> Option<SocketId> {
    sockets.get(address).copied().or_else(|| {
        wildcard_inet_address(address).and_then(|wildcard| sockets.get(&wildcard).copied())
    })
}

fn lookup_bound_inet_datagram_socket_in_table(
    sockets: &BTreeMap<InetSocketAddress, BTreeSet<SocketId>>,
    address: &InetSocketAddress,
) -> Option<SocketId> {
    sockets
        .get(address)
        .and_then(|socket_ids| socket_ids.first().copied())
        .or_else(|| {
            wildcard_inet_address(address).and_then(|wildcard| {
                sockets
                    .get(&wildcard)
                    .and_then(|socket_ids| socket_ids.first().copied())
            })
        })
}

fn register_bound_inet_socket(
    table: &mut SocketTableState,
    spec: SocketSpec,
    address: InetSocketAddress,
    socket_id: SocketId,
) {
    if supports_inet_stream_lifecycle(spec) {
        table.bound_inet_streams.insert(address, socket_id);
    } else if supports_inet_datagram_lifecycle(spec) {
        table
            .bound_inet_datagrams
            .entry(address)
            .or_default()
            .insert(socket_id);
    }
}

fn validate_connect_to_listener(
    client: &SocketRecord,
    listener: &SocketRecord,
) -> SocketResult<()> {
    if !supports_connection_lifecycle(client.spec) {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} does not support stream connections",
            client.id
        )));
    }
    if !supports_listener_lifecycle(listener.spec) {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} is not a stream listener",
            listener.id
        )));
    }
    if !matches!(client.state, SocketState::Created | SocketState::Bound) {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} cannot connect in state {:?}",
            client.id, client.state
        )));
    }
    if listener.state != SocketState::Listening {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} is not listening",
            listener.id
        )));
    }
    Ok(())
}

fn has_bound_endpoint(record: &SocketRecord) -> bool {
    record.local_address.is_some() || record.local_unix_path.is_some()
}

fn validate_bound_udp_sender(sender: &SocketRecord) -> SocketResult<()> {
    if !supports_inet_datagram_lifecycle(sender.spec) {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} is not an INET datagram socket",
            sender.id
        )));
    }
    if sender.state != SocketState::Bound || sender.local_address.is_none() {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} must be bound before sending datagrams",
            sender.id
        )));
    }
    Ok(())
}

fn validate_bound_udp_receiver(receiver: &SocketRecord) -> SocketResult<()> {
    if !supports_inet_datagram_lifecycle(receiver.spec) {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} is not an INET datagram socket",
            receiver.id
        )));
    }
    if receiver.state != SocketState::Bound || receiver.local_address.is_none() {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} must be bound to receive datagrams",
            receiver.id
        )));
    }
    Ok(())
}

fn datagram_state_mut(record: &mut SocketRecord) -> SocketResult<&mut DatagramState> {
    if !supports_inet_datagram_lifecycle(record.spec) {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} is not an INET datagram socket",
            record.id
        )));
    }
    record.datagram_state.as_mut().ok_or_else(|| {
        SocketTableError::invalid_argument(format!(
            "socket {} does not support datagrams",
            record.id
        ))
    })
}

fn validate_multicast_socket(record: &SocketRecord) -> SocketResult<()> {
    validate_bound_udp_receiver(record)?;
    if record.spec.domain != SocketDomain::Inet {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} multicast membership is only implemented for IPv4 datagrams",
            record.id
        )));
    }
    Ok(())
}

fn normalize_multicast_membership(
    spec: SocketSpec,
    membership: SocketMulticastMembership,
) -> SocketResult<SocketMulticastMembership> {
    let group_address = membership.group_address.trim().to_ascii_lowercase();
    let interface_address = membership
        .interface_address
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());

    match spec.domain {
        SocketDomain::Inet => {
            let parsed = group_address.parse::<Ipv4Addr>().map_err(|_| {
                SocketTableError::invalid_argument(format!(
                    "invalid IPv4 multicast address {group_address}"
                ))
            })?;
            if !parsed.is_multicast() {
                return Err(SocketTableError::invalid_argument(format!(
                    "address {group_address} is not an IPv4 multicast group"
                )));
            }
        }
        SocketDomain::Inet6 => {
            let parsed = group_address.parse::<Ipv6Addr>().map_err(|_| {
                SocketTableError::invalid_argument(format!(
                    "invalid IPv6 multicast address {group_address}"
                ))
            })?;
            if !parsed.is_multicast() {
                return Err(SocketTableError::invalid_argument(format!(
                    "address {group_address} is not an IPv6 multicast group"
                )));
            }
        }
        SocketDomain::Unix => {
            return Err(SocketTableError::invalid_argument(
                "unix sockets do not support multicast membership",
            ));
        }
    }

    Ok(SocketMulticastMembership::new(
        group_address,
        interface_address,
    ))
}

fn has_incompatible_inet_bind_conflict(
    table: &SocketTableState,
    record: &SocketRecord,
    conflicting_ids: &[SocketId],
) -> bool {
    conflicting_ids.iter().any(|conflicting_id| {
        if *conflicting_id == record.id {
            return false;
        }

        let Some(existing) = table.sockets.get(conflicting_id) else {
            return false;
        };

        if supports_inet_datagram_lifecycle(record.spec) {
            !inet_datagram_bind_shares_port(record, existing)
        } else {
            true
        }
    })
}

fn inet_datagram_bind_shares_port(requested: &SocketRecord, existing: &SocketRecord) -> bool {
    (requested.reuse_port() && existing.reuse_port())
        || (requested.reuse_address() && existing.reuse_address())
}

fn remove_socket(table: &mut SocketTableState, socket_id: SocketId) -> Option<SocketRecord> {
    let record = table.sockets.remove(&socket_id)?;
    unregister_bound_socket(table, &record);
    unregister_multicast_memberships(table, &record);
    if let Some(listener_state) = record.listener_state.as_ref() {
        let pending_socket_ids = listener_state
            .pending_accepts
            .iter()
            .filter_map(|pending| pending.accepted_socket_id)
            .collect::<Vec<_>>();
        for pending_socket_id in pending_socket_ids {
            let _ = remove_socket(table, pending_socket_id);
        }
    }
    if let Some(connection) = record.connection_state.as_ref() {
        if let Some(peer_socket_id) = connection.peer_socket_id {
            if let Some(peer) = table.sockets.get_mut(&peer_socket_id) {
                if let Some(peer_connection) = peer.connection_state.as_mut() {
                    if peer_connection.peer_socket_id == Some(socket_id) {
                        peer_connection.peer_socket_id = None;
                    }
                    peer_connection.peer_write_shutdown = true;
                }
            }
        }
    }
    if let Some(owner_sockets) = table.by_owner.get_mut(&record.owner_pid) {
        owner_sockets.remove(&socket_id);
        if owner_sockets.is_empty() {
            table.by_owner.remove(&record.owner_pid);
        }
    }
    Some(record)
}

fn unregister_bound_socket(table: &mut SocketTableState, record: &SocketRecord) {
    let Some(address) = record.local_address.as_ref() else {
        if supports_unix_stream_lifecycle(record.spec) {
            if let Some(path) = record.local_unix_path.as_ref() {
                if table.bound_unix_streams.get(path).copied() == Some(record.id) {
                    table.bound_unix_streams.remove(path);
                }
            }
        }
        return;
    };
    if supports_inet_stream_lifecycle(record.spec)
        && table.bound_inet_streams.get(address).copied() == Some(record.id)
    {
        table.bound_inet_streams.remove(address);
    }
    if supports_inet_datagram_lifecycle(record.spec) {
        if let Some(socket_ids) = table.bound_inet_datagrams.get_mut(address) {
            socket_ids.remove(&record.id);
            if socket_ids.is_empty() {
                table.bound_inet_datagrams.remove(address);
            }
        }
    }
}

fn unregister_multicast_memberships(table: &mut SocketTableState, record: &SocketRecord) {
    let Some(datagram_state) = record.datagram_state.as_ref() else {
        return;
    };

    for membership in &datagram_state.multicast_memberships {
        if let Some(socket_ids) = table.multicast_groups.get_mut(membership) {
            socket_ids.remove(&record.id);
            if socket_ids.is_empty() {
                table.multicast_groups.remove(membership);
            }
        }
    }
}

fn normalize_inet_address(address: InetSocketAddress) -> InetSocketAddress {
    match address.host().to_ascii_lowercase().as_str() {
        "localhost" => InetSocketAddress::new("127.0.0.1", address.port()),
        _ => address,
    }
}

fn wildcard_inet_address(address: &InetSocketAddress) -> Option<InetSocketAddress> {
    match address.host() {
        "127.0.0.1" => Some(InetSocketAddress::new("0.0.0.0", address.port())),
        "::1" => Some(InetSocketAddress::new("::", address.port())),
        _ => None,
    }
}

fn normalize_unix_socket_path(path: impl AsRef<str>) -> SocketResult<String> {
    let normalized = normalize_path(path.as_ref());
    if normalized == "/" {
        return Err(SocketTableError::invalid_argument(
            "unix socket path must not be empty or root",
        ));
    }
    Ok(normalized)
}

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
