use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

pub type SocketId = u64;
pub type SocketResult<T> = Result<T, SocketTableError>;

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
    next_socket_id: SocketId,
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
        Ok(record.clone())
    }

    pub fn remove(&self, socket_id: SocketId) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .remove(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
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
            .filter_map(|socket_id| table.sockets.remove(&socket_id))
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

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
