use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
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
    listener_state: Option<ListenerState>,
    connection_state: Option<ConnectionState>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SocketTableSnapshot {
    pub sockets: usize,
    pub listeners: usize,
    pub connections: usize,
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
    next_socket_id: SocketId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListenerState {
    backlog: usize,
    pending_accepts: VecDeque<PendingTcpConnection>,
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
struct PendingTcpConnection {
    peer_address: InetSocketAddress,
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
            listener_state: None,
            connection_state: default_connection_state(spec, state),
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
        let mut table = lock_or_recover(&self.inner.state);
        let existing_id = table.bound_inet_streams.get(&address).copied();
        let cloned = {
            let record = table
                .sockets
                .get_mut(&socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;

            if !supports_inet_stream_lifecycle(record.spec) {
                return Err(SocketTableError::invalid_argument(format!(
                    "socket {socket_id} is not an INET stream socket"
                )));
            }

            if let Some(bound_socket_id) = existing_id {
                if bound_socket_id != socket_id {
                    return Err(SocketTableError::address_in_use(format!(
                        "address {}:{} is already bound",
                        address.host(),
                        address.port()
                    )));
                }
            }

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
            record.listener_state = None;
            record.connection_state = None;
            record.state = SocketState::Bound;
            record.clone()
        };
        table.bound_inet_streams.insert(address, socket_id);
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

        if !supports_inet_stream_lifecycle(record.spec) {
            return Err(SocketTableError::invalid_argument(format!(
                "socket {socket_id} is not an INET stream socket"
            )));
        }
        if record.state != SocketState::Bound || record.local_address.is_none() {
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

        listener_state
            .pending_accepts
            .push_back(PendingTcpConnection { peer_address });
        Ok(())
    }

    pub fn accept(&self, listener_socket_id: SocketId) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let (owner_pid, spec, local_address, peer_address) = {
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
                pending.peer_address,
            )
        };

        let socket_id = next_socket_id(&mut table);
        let record = SocketRecord {
            id: socket_id,
            owner_pid,
            spec,
            state: SocketState::Connected,
            local_address,
            peer_address: Some(peer_address),
            listener_state: None,
            connection_state: default_connection_state(spec, SocketState::Connected),
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
        socket.listener_state = None;
        socket.connection_state = Some(ConnectionState {
            peer_socket_id: Some(peer_socket_id),
            ..ConnectionState::default()
        });

        peer.state = SocketState::Connected;
        peer.peer_address = socket.local_address.clone();
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

fn supports_connection_lifecycle(spec: SocketSpec) -> bool {
    matches!(spec.socket_type, SocketType::Stream)
}

fn supports_inet_stream_lifecycle(spec: SocketSpec) -> bool {
    matches!(spec.socket_type, SocketType::Stream)
        && matches!(spec.domain, SocketDomain::Inet | SocketDomain::Inet6)
}

fn remove_socket(table: &mut SocketTableState, socket_id: SocketId) -> Option<SocketRecord> {
    let record = table.sockets.remove(&socket_id)?;
    unregister_bound_inet_stream(table, &record);
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

fn unregister_bound_inet_stream(table: &mut SocketTableState, record: &SocketRecord) {
    let Some(address) = record.local_address.as_ref() else {
        return;
    };
    if table.bound_inet_streams.get(address).copied() == Some(record.id) {
        table.bound_inet_streams.remove(address);
    }
}

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
