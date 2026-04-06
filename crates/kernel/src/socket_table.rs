use crate::resource_accounting::ResourceLimits;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;

pub type SocketId = u64;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SocketAddress {
    Inet { host: String, port: u16 },
    Unix { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatagramPacket {
    pub from: SocketAddress,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketError {
    code: &'static str,
    message: String,
}

impl SocketError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for SocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for SocketError {}

pub type SocketResult<T> = Result<T, SocketError>;

#[derive(Debug, Clone)]
struct ListenerEntry {
    owner_pid: u32,
    local: SocketAddress,
    backlog: usize,
    pending: VecDeque<SocketId>,
    active: BTreeSet<SocketId>,
}

#[derive(Debug, Clone)]
struct StreamEntry {
    owner_pid: u32,
    peer: Option<SocketId>,
    listener_id: Option<SocketId>,
    recv: VecDeque<Vec<u8>>,
    was_connected: bool,
}

#[derive(Debug, Clone)]
struct DatagramEntry {
    owner_pid: u32,
    local: Option<SocketAddress>,
    recv: VecDeque<DatagramPacket>,
}

#[derive(Debug, Clone)]
enum SocketEntry {
    Listener(ListenerEntry),
    Stream(StreamEntry),
    Datagram(DatagramEntry),
}

impl SocketEntry {
    fn owner_pid(&self) -> u32 {
        match self {
            Self::Listener(entry) => entry.owner_pid,
            Self::Stream(entry) => entry.owner_pid,
            Self::Datagram(entry) => entry.owner_pid,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SocketTable {
    limits: ResourceLimits,
    next_socket_id: SocketId,
    next_ephemeral_port: u16,
    entries: BTreeMap<SocketId, SocketEntry>,
    listener_index: BTreeMap<SocketAddress, SocketId>,
    datagram_index: BTreeMap<SocketAddress, SocketId>,
}

impl Default for SocketTable {
    fn default() -> Self {
        Self::new(ResourceLimits::default())
    }
}

impl SocketTable {
    pub fn new(limits: ResourceLimits) -> Self {
        Self {
            limits,
            next_socket_id: 1,
            next_ephemeral_port: 49_152,
            entries: BTreeMap::new(),
            listener_index: BTreeMap::new(),
            datagram_index: BTreeMap::new(),
        }
    }

    pub fn socket_count(&self) -> usize {
        self.entries.len()
    }

    pub fn connection_count(&self) -> usize {
        let mut pairs = BTreeSet::new();
        for (socket_id, entry) in &self.entries {
            if let SocketEntry::Stream(stream) = entry {
                if let Some(peer_id) = stream.peer {
                    let pair = if socket_id < &peer_id {
                        (*socket_id, peer_id)
                    } else {
                        (peer_id, *socket_id)
                    };
                    pairs.insert(pair);
                }
            }
        }
        pairs.len()
    }

    pub fn contains(&self, socket_id: SocketId) -> bool {
        self.entries.contains_key(&socket_id)
    }

    pub fn create_tcp_listener(
        &mut self,
        owner_pid: u32,
        host: impl Into<String>,
        port: u16,
        backlog: usize,
    ) -> SocketResult<SocketId> {
        self.create_listener(
            owner_pid,
            SocketAddress::Inet {
                host: host.into(),
                port,
            },
            backlog,
        )
    }

    pub fn create_unix_listener(
        &mut self,
        owner_pid: u32,
        path: impl Into<String>,
        backlog: usize,
    ) -> SocketResult<SocketId> {
        self.create_listener(
            owner_pid,
            SocketAddress::Unix { path: path.into() },
            backlog,
        )
    }

    pub fn connect_tcp(
        &mut self,
        owner_pid: u32,
        host: impl Into<String>,
        port: u16,
    ) -> SocketResult<SocketId> {
        let host = host.into();
        let remote = SocketAddress::Inet {
            host: host.clone(),
            port,
        };
        let local = SocketAddress::Inet {
            host,
            port: self.allocate_ephemeral_port()?,
        };
        self.connect_stream(owner_pid, &remote, local)
    }

    pub fn connect_unix(
        &mut self,
        owner_pid: u32,
        path: impl Into<String>,
    ) -> SocketResult<SocketId> {
        let remote = SocketAddress::Unix { path: path.into() };
        let client_id = self.next_socket_id;
        let local = SocketAddress::Unix {
            path: format!("@client-{client_id}"),
        };
        self.connect_stream(owner_pid, &remote, local)
    }

    pub fn accept(&mut self, listener_id: SocketId) -> SocketResult<Option<SocketId>> {
        let listener = match self.entries.get_mut(&listener_id) {
            Some(SocketEntry::Listener(listener)) => listener,
            Some(_) => {
                return Err(SocketError::new(
                    "EINVAL",
                    format!("socket {listener_id} is not a listener"),
                ));
            }
            None => {
                return Err(SocketError::new(
                    "EBADF",
                    format!("unknown socket {listener_id}"),
                ));
            }
        };

        let Some(socket_id) = listener.pending.pop_front() else {
            return Ok(None);
        };
        listener.active.insert(socket_id);
        Ok(Some(socket_id))
    }

    pub fn send(&mut self, socket_id: SocketId, payload: Vec<u8>) -> SocketResult<usize> {
        let peer_id = match self.entries.get(&socket_id) {
            Some(SocketEntry::Stream(stream)) => {
                if let Some(peer_id) = stream.peer {
                    peer_id
                } else if stream.was_connected {
                    return Err(SocketError::new(
                        "EPIPE",
                        format!("socket {socket_id} is disconnected"),
                    ));
                } else {
                    return Err(SocketError::new(
                        "ENOTCONN",
                        format!("socket {socket_id} is not connected"),
                    ));
                }
            }
            Some(_) => {
                return Err(SocketError::new(
                    "EINVAL",
                    format!("socket {socket_id} does not support stream send"),
                ));
            }
            None => {
                return Err(SocketError::new(
                    "EBADF",
                    format!("unknown socket {socket_id}"),
                ));
            }
        };

        match self.entries.get_mut(&peer_id) {
            Some(SocketEntry::Stream(stream)) => {
                let bytes = payload.len();
                stream.recv.push_back(payload);
                Ok(bytes)
            }
            _ => Err(SocketError::new(
                "EPIPE",
                format!("peer socket {peer_id} is no longer available"),
            )),
        }
    }

    pub fn recv(&mut self, socket_id: SocketId) -> SocketResult<Option<Vec<u8>>> {
        let stream = match self.entries.get_mut(&socket_id) {
            Some(SocketEntry::Stream(stream)) => stream,
            Some(_) => {
                return Err(SocketError::new(
                    "EINVAL",
                    format!("socket {socket_id} does not support stream recv"),
                ));
            }
            None => {
                return Err(SocketError::new(
                    "EBADF",
                    format!("unknown socket {socket_id}"),
                ));
            }
        };

        if let Some(chunk) = stream.recv.pop_front() {
            return Ok(Some(chunk));
        }

        if stream.was_connected {
            return Ok(None);
        }

        Err(SocketError::new(
            "ENOTCONN",
            format!("socket {socket_id} is not connected"),
        ))
    }

    pub fn create_udp_socket(&mut self, owner_pid: u32) -> SocketResult<SocketId> {
        self.check_socket_capacity(1)?;
        let socket_id = self.allocate_socket_id();
        self.entries.insert(
            socket_id,
            SocketEntry::Datagram(DatagramEntry {
                owner_pid,
                local: None,
                recv: VecDeque::new(),
            }),
        );
        Ok(socket_id)
    }

    pub fn bind_udp(
        &mut self,
        socket_id: SocketId,
        host: impl Into<String>,
        port: u16,
    ) -> SocketResult<()> {
        let local = SocketAddress::Inet {
            host: host.into(),
            port,
        };
        if self.datagram_index.contains_key(&local) {
            return Err(SocketError::new(
                "EADDRINUSE",
                format!("UDP address {:?} is already bound", local),
            ));
        }

        let socket = match self.entries.get_mut(&socket_id) {
            Some(SocketEntry::Datagram(socket)) => socket,
            Some(_) => {
                return Err(SocketError::new(
                    "EINVAL",
                    format!("socket {socket_id} is not a datagram socket"),
                ));
            }
            None => {
                return Err(SocketError::new(
                    "EBADF",
                    format!("unknown socket {socket_id}"),
                ));
            }
        };

        socket.local = Some(local.clone());
        self.datagram_index.insert(local, socket_id);
        Ok(())
    }

    pub fn send_to(
        &mut self,
        socket_id: SocketId,
        host: impl Into<String>,
        port: u16,
        payload: Vec<u8>,
    ) -> SocketResult<usize> {
        let source = match self.entries.get(&socket_id) {
            Some(SocketEntry::Datagram(socket)) => socket.local.clone().ok_or_else(|| {
                SocketError::new(
                    "EINVAL",
                    format!("UDP socket {socket_id} must be bound before send_to"),
                )
            })?,
            Some(_) => {
                return Err(SocketError::new(
                    "EINVAL",
                    format!("socket {socket_id} is not a datagram socket"),
                ));
            }
            None => {
                return Err(SocketError::new(
                    "EBADF",
                    format!("unknown socket {socket_id}"),
                ));
            }
        };

        let destination = SocketAddress::Inet {
            host: host.into(),
            port,
        };
        let peer_id = self
            .datagram_index
            .get(&destination)
            .copied()
            .ok_or_else(|| {
                SocketError::new(
                    "ECONNREFUSED",
                    format!("no UDP socket bound at {:?}", destination),
                )
            })?;

        let bytes = payload.len();
        match self.entries.get_mut(&peer_id) {
            Some(SocketEntry::Datagram(socket)) => {
                socket.recv.push_back(DatagramPacket {
                    from: source,
                    data: payload,
                });
                Ok(bytes)
            }
            _ => Err(SocketError::new(
                "ECONNREFUSED",
                format!("destination socket {peer_id} is unavailable"),
            )),
        }
    }

    pub fn recv_from(&mut self, socket_id: SocketId) -> SocketResult<Option<DatagramPacket>> {
        let socket = match self.entries.get_mut(&socket_id) {
            Some(SocketEntry::Datagram(socket)) => socket,
            Some(_) => {
                return Err(SocketError::new(
                    "EINVAL",
                    format!("socket {socket_id} is not a datagram socket"),
                ));
            }
            None => {
                return Err(SocketError::new(
                    "EBADF",
                    format!("unknown socket {socket_id}"),
                ));
            }
        };

        if socket.local.is_none() {
            return Err(SocketError::new(
                "EINVAL",
                format!("UDP socket {socket_id} must be bound before recv_from"),
            ));
        }

        Ok(socket.recv.pop_front())
    }

    pub fn close(&mut self, socket_id: SocketId) -> SocketResult<()> {
        if !self.entries.contains_key(&socket_id) {
            return Err(SocketError::new(
                "EBADF",
                format!("unknown socket {socket_id}"),
            ));
        }
        self.close_inner(socket_id);
        Ok(())
    }

    pub fn cleanup_process(&mut self, pid: u32) {
        let owned_ids: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(socket_id, entry)| (entry.owner_pid() == pid).then_some(*socket_id))
            .collect();
        for socket_id in owned_ids {
            if self.entries.contains_key(&socket_id) {
                self.close_inner(socket_id);
            }
        }
    }

    fn create_listener(
        &mut self,
        owner_pid: u32,
        local: SocketAddress,
        backlog: usize,
    ) -> SocketResult<SocketId> {
        if self.listener_index.contains_key(&local) {
            return Err(SocketError::new(
                "EADDRINUSE",
                format!("listener {:?} already exists", local),
            ));
        }

        self.check_socket_capacity(1)?;
        let socket_id = self.allocate_socket_id();
        self.listener_index.insert(local.clone(), socket_id);
        self.entries.insert(
            socket_id,
            SocketEntry::Listener(ListenerEntry {
                owner_pid,
                local,
                backlog: backlog.max(1),
                pending: VecDeque::new(),
                active: BTreeSet::new(),
            }),
        );
        Ok(socket_id)
    }

    fn connect_stream(
        &mut self,
        owner_pid: u32,
        remote: &SocketAddress,
        _local: SocketAddress,
    ) -> SocketResult<SocketId> {
        let listener_id = self.listener_index.get(remote).copied().ok_or_else(|| {
            SocketError::new("ECONNREFUSED", format!("no listener bound at {:?}", remote))
        })?;
        let (server_owner, backlog_full) = match self.entries.get(&listener_id) {
            Some(SocketEntry::Listener(listener)) => (
                listener.owner_pid,
                listener.pending.len() + listener.active.len() >= listener.backlog,
            ),
            _ => {
                return Err(SocketError::new(
                    "ECONNREFUSED",
                    format!("listener {listener_id} is unavailable"),
                ));
            }
        };

        if backlog_full {
            return Err(SocketError::new(
                "EAGAIN",
                format!("listener {listener_id} backlog is full"),
            ));
        }

        self.check_socket_capacity(2)?;
        self.check_connection_capacity(1)?;

        let client_id = self.allocate_socket_id();
        let server_id = self.allocate_socket_id();
        self.entries.insert(
            client_id,
            SocketEntry::Stream(StreamEntry {
                owner_pid,
                peer: Some(server_id),
                listener_id: None,
                recv: VecDeque::new(),
                was_connected: true,
            }),
        );
        self.entries.insert(
            server_id,
            SocketEntry::Stream(StreamEntry {
                owner_pid: server_owner,
                peer: Some(client_id),
                listener_id: Some(listener_id),
                recv: VecDeque::new(),
                was_connected: true,
            }),
        );

        let listener = match self.entries.get_mut(&listener_id) {
            Some(SocketEntry::Listener(listener)) => listener,
            _ => unreachable!("listener vanished after lookup"),
        };
        listener.pending.push_back(server_id);

        Ok(client_id)
    }

    fn close_inner(&mut self, socket_id: SocketId) {
        let Some(entry) = self.entries.remove(&socket_id) else {
            return;
        };

        match entry {
            SocketEntry::Listener(listener) => {
                self.listener_index.remove(&listener.local);
                let mut children: Vec<_> = listener.pending.into_iter().collect();
                children.extend(listener.active);
                for child_id in children {
                    self.close_inner(child_id);
                }
            }
            SocketEntry::Stream(stream) => {
                if let Some(listener_id) = stream.listener_id {
                    if let Some(SocketEntry::Listener(listener)) =
                        self.entries.get_mut(&listener_id)
                    {
                        listener
                            .pending
                            .retain(|pending_id| pending_id != &socket_id);
                        listener.active.remove(&socket_id);
                    }
                }
                if let Some(peer_id) = stream.peer {
                    if let Some(SocketEntry::Stream(peer)) = self.entries.get_mut(&peer_id) {
                        peer.peer = None;
                    }
                }
            }
            SocketEntry::Datagram(socket) => {
                if let Some(local) = socket.local {
                    self.datagram_index.remove(&local);
                }
            }
        }
    }

    fn check_socket_capacity(&self, additional: usize) -> SocketResult<()> {
        if let Some(limit) = self.limits.max_sockets {
            if self.entries.len().saturating_add(additional) > limit {
                return Err(SocketError::new(
                    "EAGAIN",
                    format!("maximum socket limit {limit} reached"),
                ));
            }
        }
        Ok(())
    }

    fn check_connection_capacity(&self, additional: usize) -> SocketResult<()> {
        if let Some(limit) = self.limits.max_connections {
            if self.connection_count().saturating_add(additional) > limit {
                return Err(SocketError::new(
                    "EAGAIN",
                    format!("maximum connection limit {limit} reached"),
                ));
            }
        }
        Ok(())
    }

    fn allocate_socket_id(&mut self) -> SocketId {
        let socket_id = self.next_socket_id;
        self.next_socket_id = self.next_socket_id.saturating_add(1);
        socket_id
    }

    fn allocate_ephemeral_port(&mut self) -> SocketResult<u16> {
        let start = self.next_ephemeral_port;
        loop {
            let port = self.next_ephemeral_port;
            self.next_ephemeral_port = if self.next_ephemeral_port == u16::MAX {
                49_152
            } else {
                self.next_ephemeral_port + 1
            };
            let candidate = SocketAddress::Inet {
                host: String::from("127.0.0.1"),
                port,
            };
            if !self.listener_index.contains_key(&candidate)
                && !self.datagram_index.contains_key(&candidate)
            {
                return Ok(port);
            }
            if self.next_ephemeral_port == start {
                return Err(SocketError::new(
                    "EAGAIN",
                    "no ephemeral ports remain available",
                ));
            }
        }
    }
}
