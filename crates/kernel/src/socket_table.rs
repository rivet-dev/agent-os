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
        };
        table.sockets.insert(socket_id, record.clone());
        table
            .by_owner
            .entry(owner_pid)
            .or_default()
            .insert(socket_id);
        Ok(record)
    }

    pub fn remove(&self, socket_id: SocketId) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .remove(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        unregister_bound_inet_stream(&mut table, &record);
        if let Some(owner_sockets) = table.by_owner.get_mut(&record.owner_pid) {
            owner_sockets.remove(&socket_id);
            if owner_sockets.is_empty() {
                table.by_owner.remove(&record.owner_pid);
            }
        }
        Ok(record)
    }

    pub fn remove_all_for_pid(&self, owner_pid: u32) -> Vec<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let Some(socket_ids) = table.by_owner.remove(&owner_pid) else {
            return Vec::new();
        };

        socket_ids
            .into_iter()
            .filter_map(|socket_id| {
                let record = table.sockets.remove(&socket_id)?;
                unregister_bound_inet_stream(&mut table, &record);
                Some(record)
            })
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
    if matches!(
        (current, next),
        (SocketState::Listening, SocketState::Connected)
            | (SocketState::Connected, SocketState::Listening)
    ) {
        return Err(SocketTableError::invalid_argument(format!(
            "invalid socket state transition from {current:?} to {next:?}"
        )));
    }
    Ok(())
}

fn supports_inet_stream_lifecycle(spec: SocketSpec) -> bool {
    matches!(spec.socket_type, SocketType::Stream)
        && matches!(spec.domain, SocketDomain::Inet | SocketDomain::Inet6)
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
