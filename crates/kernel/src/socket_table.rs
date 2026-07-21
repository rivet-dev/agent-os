#[cfg(not(target_arch = "wasm32"))]
use crate::admission::{Reservation, ResourceClass, ResourceLedger};
use crate::fd_table::TransferredFd;
use crate::poll::{PollEvents, POLLERR, POLLHUP, POLLIN, POLLOUT};
use crate::vfs::normalize_path;
use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

pub type SocketId = u64;
pub type SocketResult<T> = Result<T, SocketTableError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketReadinessKind {
    Data,
    Accept,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketReadiness {
    pub socket_id: SocketId,
    pub kind: SocketReadinessKind,
}

type SocketReadinessSink = Arc<dyn Fn(SocketReadiness) + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SocketReadTraceSnapshot {
    pub socket_record_clone_calls: u64,
    pub socket_record_clone_us: u64,
    pub read_recv_calls: u64,
    pub read_recv_bytes: u64,
    pub read_recv_chunks: u64,
    pub read_recv_copy_us: u64,
}

struct SocketReadTraceCounters {
    socket_record_clone_calls: AtomicU64,
    socket_record_clone_us: AtomicU64,
    read_recv_calls: AtomicU64,
    read_recv_bytes: AtomicU64,
    read_recv_chunks: AtomicU64,
    read_recv_copy_us: AtomicU64,
}

impl SocketReadTraceCounters {
    const fn new() -> Self {
        Self {
            socket_record_clone_calls: AtomicU64::new(0),
            socket_record_clone_us: AtomicU64::new(0),
            read_recv_calls: AtomicU64::new(0),
            read_recv_bytes: AtomicU64::new(0),
            read_recv_chunks: AtomicU64::new(0),
            read_recv_copy_us: AtomicU64::new(0),
        }
    }
}

static SOCKET_READ_TRACE_ENABLED: AtomicBool = AtomicBool::new(false);
static SOCKET_READ_TRACE_COUNTERS: SocketReadTraceCounters = SocketReadTraceCounters::new();

pub fn set_socket_read_trace_enabled(enabled: bool) {
    SOCKET_READ_TRACE_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn reset_socket_read_trace() {
    for counter in [
        &SOCKET_READ_TRACE_COUNTERS.socket_record_clone_calls,
        &SOCKET_READ_TRACE_COUNTERS.socket_record_clone_us,
        &SOCKET_READ_TRACE_COUNTERS.read_recv_calls,
        &SOCKET_READ_TRACE_COUNTERS.read_recv_bytes,
        &SOCKET_READ_TRACE_COUNTERS.read_recv_chunks,
        &SOCKET_READ_TRACE_COUNTERS.read_recv_copy_us,
    ] {
        counter.store(0, Ordering::Relaxed);
    }
}

pub fn socket_read_trace_snapshot() -> SocketReadTraceSnapshot {
    SocketReadTraceSnapshot {
        socket_record_clone_calls: SOCKET_READ_TRACE_COUNTERS
            .socket_record_clone_calls
            .load(Ordering::Relaxed),
        socket_record_clone_us: SOCKET_READ_TRACE_COUNTERS
            .socket_record_clone_us
            .load(Ordering::Relaxed),
        read_recv_calls: SOCKET_READ_TRACE_COUNTERS
            .read_recv_calls
            .load(Ordering::Relaxed),
        read_recv_bytes: SOCKET_READ_TRACE_COUNTERS
            .read_recv_bytes
            .load(Ordering::Relaxed),
        read_recv_chunks: SOCKET_READ_TRACE_COUNTERS
            .read_recv_chunks
            .load(Ordering::Relaxed),
        read_recv_copy_us: SOCKET_READ_TRACE_COUNTERS
            .read_recv_copy_us
            .load(Ordering::Relaxed),
    }
}

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
    SeqPacket,
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

    pub const fn unix_seqpacket() -> Self {
        Self::new(SocketDomain::Unix, SocketType::SeqPacket)
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
            .map(ConnectionState::buffered_len)
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
            .unwrap_or_else(|| {
                self.connection_state
                    .as_ref()
                    .filter(|_| self.spec.socket_type != SocketType::Stream)
                    .map(|state| state.recv_buffer.len())
                    .unwrap_or(0)
            })
    }

    pub fn queued_datagram_bytes(&self) -> usize {
        self.datagram_state
            .as_ref()
            .map(|state| datagram_queue_bytes(&state.recv_queue))
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

pub type OpaqueTransferredRight = Arc<dyn Any + Send + Sync + 'static>;

#[derive(Clone)]
pub enum TransferredSocketRight {
    Fd(TransferredFd),
    Opaque(OpaqueTransferredRight),
}

impl fmt::Debug for TransferredSocketRight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fd(fd) => f.debug_tuple("Fd").field(&fd.description_id()).finish(),
            Self::Opaque(resource) => f
                .debug_tuple("Opaque")
                .field(&(Arc::as_ptr(resource) as *const ()))
                .finish(),
        }
    }
}

impl PartialEq for TransferredSocketRight {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Fd(left), Self::Fd(right)) => left == right,
            (Self::Opaque(left), Self::Opaque(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

impl Eq for TransferredSocketRight {}

#[derive(Debug)]
pub struct ReceivedSocketMessage {
    pub payload: Vec<u8>,
    pub rights: Vec<TransferredSocketRight>,
    pub truncated: bool,
    pub full_length: usize,
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

/// Native sidecar handoff that transfers the kernel queue's exact resource
/// ownership with the datagram. Standalone callers may keep using
/// `recv_datagram`, whose boundary releases queue ownership immediately.
#[cfg(not(target_arch = "wasm32"))]
pub type DatagramReservations = (Reservation, Reservation, Reservation, Reservation);

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct ChargedReceivedDatagram {
    datagram: ReceivedDatagram,
    reservations: Option<DatagramReservations>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ChargedReceivedDatagram {
    pub fn into_parts(
        self,
    ) -> (
        Option<InetSocketAddress>,
        Vec<u8>,
        Option<DatagramReservations>,
    ) {
        let (source_address, payload) = self.datagram.into_parts();
        (source_address, payload, self.reservations)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SocketTableSnapshot {
    pub sockets: usize,
    pub listeners: usize,
    pub connections: usize,
    pub buffered_bytes: usize,
    pub datagram_queue_len: usize,
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

    #[cfg(not(target_arch = "wasm32"))]
    fn resource_limit(error: crate::admission::LimitError) -> Self {
        Self {
            code: "EAGAIN",
            message: error.to_string(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn accounting_invariant(message: impl Into<String>) -> Self {
        Self {
            code: "EIO",
            message: format!(
                "ERR_AGENTOS_RESOURCE_ACCOUNTING_INVARIANT: {}",
                message.into()
            ),
        }
    }

    fn id_exhausted() -> Self {
        Self {
            code: "EMFILE",
            message: String::from(
                "ERR_AGENTOS_SOCKET_ID_EXHAUSTED: VM kernel socket id space exhausted",
            ),
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
    #[cfg(not(target_arch = "wasm32"))]
    retained_resources: BTreeMap<SocketId, RetainedSocketResources>,
}

/// Exact ownership for bytes and datagrams retained by one kernel socket.
///
/// Socket and connection count reservations intentionally live in the shared
/// capability registry. Keeping only queue storage here avoids charging the
/// same kernel-backed capability twice while still making queue admission and
/// mutation one operation.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
struct RetainedSocketResources {
    buffered_bytes: VecDeque<Reservation>,
    datagrams: VecDeque<Reservation>,
    udp_bytes: VecDeque<Reservation>,
    udp_datagrams: VecDeque<Reservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListenerState {
    backlog: usize,
    pending_accepts: VecDeque<PendingConnection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ConnectionState {
    peer_socket_id: Option<SocketId>,
    recv_buffer: VecDeque<RecvChunk>,
    recv_buffer_len: usize,
    read_shutdown: bool,
    write_shutdown: bool,
    peer_write_shutdown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecvChunk {
    data: Vec<u8>,
    rights: Vec<TransferredSocketRight>,
}

impl ConnectionState {
    fn buffered_len(&self) -> usize {
        self.recv_buffer_len
    }

    fn has_buffered_data(&self) -> bool {
        !self.recv_buffer.is_empty()
    }

    fn push_recv(&mut self, data: &[u8], rights: Vec<TransferredSocketRight>) {
        if data.is_empty() && rights.is_empty() {
            return;
        }
        self.recv_buffer.push_back(RecvChunk {
            data: data.to_vec(),
            rights,
        });
        self.recv_buffer_len = self.recv_buffer_len.saturating_add(data.len());
    }

    fn read_recv(&mut self, max_bytes: usize, message_oriented: bool) -> Option<Vec<u8>> {
        self.read_recv_message(max_bytes, message_oriented)
            .map(|message| message.payload)
    }

    fn read_recv_message(
        &mut self,
        max_bytes: usize,
        message_oriented: bool,
    ) -> Option<ReceivedSocketMessage> {
        if self.recv_buffer.is_empty() {
            return None;
        }

        if message_oriented {
            let chunk = self.recv_buffer.pop_front()?;
            self.recv_buffer_len = self.recv_buffer_len.saturating_sub(chunk.data.len());
            let truncated = chunk.data.len() > max_bytes;
            let full_length = chunk.data.len();
            return Some(ReceivedSocketMessage {
                payload: chunk.data[..chunk.data.len().min(max_bytes)].to_vec(),
                rights: chunk.rights,
                truncated,
                full_length,
            });
        }

        if max_bytes == 0 {
            let chunk = self.recv_buffer.front_mut()?;
            return Some(ReceivedSocketMessage {
                payload: Vec::new(),
                rights: std::mem::take(&mut chunk.rights),
                truncated: false,
                full_length: 0,
            });
        }

        let read_len = self.recv_buffer_len.min(max_bytes);
        self.recv_buffer_len -= read_len;

        let mut remaining = read_len;
        let mut chunks = 0usize;
        let trace_started = SOCKET_READ_TRACE_ENABLED
            .load(Ordering::Relaxed)
            .then(Instant::now);
        let mut out = Vec::with_capacity(read_len);
        let mut rights = Vec::new();
        while remaining > 0 {
            let mut chunk = self.recv_buffer.pop_front()?;
            chunks += 1;
            rights.append(&mut chunk.rights);
            if chunk.data.len() <= remaining {
                remaining -= chunk.data.len();
                out.extend_from_slice(&chunk.data);
                continue;
            }

            let tail = chunk.data.split_off(remaining);
            out.extend_from_slice(&chunk.data);
            self.recv_buffer.push_front(RecvChunk {
                data: tail,
                rights: Vec::new(),
            });
            remaining = 0;
        }
        if let Some(started) = trace_started {
            SOCKET_READ_TRACE_COUNTERS
                .read_recv_calls
                .fetch_add(1, Ordering::Relaxed);
            SOCKET_READ_TRACE_COUNTERS.read_recv_bytes.fetch_add(
                u64::try_from(read_len).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            SOCKET_READ_TRACE_COUNTERS
                .read_recv_chunks
                .fetch_add(u64::try_from(chunks).unwrap_or(u64::MAX), Ordering::Relaxed);
            SOCKET_READ_TRACE_COUNTERS.read_recv_copy_us.fetch_add(
                u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        Some(ReceivedSocketMessage {
            payload: out,
            rights,
            truncated: false,
            full_length: read_len,
        })
    }

    fn peek_recv_message(
        &self,
        max_bytes: usize,
        message_oriented: bool,
    ) -> Option<ReceivedSocketMessage> {
        let first = self.recv_buffer.front()?;
        if message_oriented {
            return Some(ReceivedSocketMessage {
                payload: first.data[..first.data.len().min(max_bytes)].to_vec(),
                rights: first.rights.clone(),
                truncated: first.data.len() > max_bytes,
                full_length: first.data.len(),
            });
        }

        let read_len = self.recv_buffer_len.min(max_bytes);
        let mut payload = Vec::with_capacity(read_len);
        let mut rights = Vec::new();
        let mut remaining = read_len;
        for (index, chunk) in self.recv_buffer.iter().enumerate() {
            if index > 0 && remaining == 0 {
                break;
            }
            rights.extend(chunk.rights.iter().cloned());
            let take = remaining.min(chunk.data.len());
            payload.extend_from_slice(&chunk.data[..take]);
            remaining -= take;
        }
        Some(ReceivedSocketMessage {
            payload,
            rights,
            truncated: false,
            full_length: read_len,
        })
    }

    fn clear_recv(&mut self) {
        self.recv_buffer.clear();
        self.recv_buffer_len = 0;
    }
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

struct SocketTableInner {
    state: Mutex<SocketTableState>,
    readiness_sink: Mutex<Option<SocketReadinessSink>>,
    #[cfg(not(target_arch = "wasm32"))]
    resource_ledger: Mutex<Option<Arc<ResourceLedger>>>,
}

impl Default for SocketTableInner {
    fn default() -> Self {
        Self {
            state: Mutex::new(SocketTableState::default()),
            readiness_sink: Mutex::new(None),
            #[cfg(not(target_arch = "wasm32"))]
            resource_ledger: Mutex::new(None),
        }
    }
}

impl fmt::Debug for SocketTableInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketTableInner")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct SocketTable {
    inner: Arc<SocketTableInner>,
}

impl SocketTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the VM ledger before any socket is created. The table owns
    /// reservations only for data physically retained in kernel queues.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_resource_ledger(&self, ledger: Arc<ResourceLedger>) -> SocketResult<()> {
        let table = lock_or_recover(&self.inner.state);
        if !table.sockets.is_empty() {
            return Err(SocketTableError::invalid_argument(
                "socket resource ledger must be installed before socket creation",
            ));
        }
        let mut target = lock_or_recover(&self.inner.resource_ledger);
        if let Some(current) = target.as_ref() {
            if Arc::ptr_eq(current, &ledger) {
                return Ok(());
            }
            return Err(SocketTableError::invalid_argument(
                "socket resource ledger is already installed",
            ));
        }
        *target = Some(ledger);
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn has_resource_ledger(&self) -> bool {
        lock_or_recover(&self.inner.resource_ledger).is_some()
    }

    #[cfg(target_arch = "wasm32")]
    pub const fn has_resource_ledger(&self) -> bool {
        false
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn resource_ledger(&self) -> Option<Arc<ResourceLedger>> {
        lock_or_recover(&self.inner.resource_ledger).clone()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn buffered_byte_capacity_available(&self) -> bool {
        self.resource_ledger()
            .is_none_or(|ledger| ledger.capacity_available(ResourceClass::BufferedBytes, 1))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn datagram_capacity_available(&self) -> bool {
        self.resource_ledger().is_none_or(|ledger| {
            ledger.capacity_available(ResourceClass::Datagrams, 1)
                && ledger.capacity_available(ResourceClass::UdpDatagrams, 1)
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn reserve_buffered_bytes(&self, amount: usize) -> SocketResult<Option<Reservation>> {
        if amount == 0 {
            return Ok(None);
        }
        self.resource_ledger()
            .map(|ledger| {
                ledger
                    .reserve(ResourceClass::BufferedBytes, amount)
                    .map_err(SocketTableError::resource_limit)
            })
            .transpose()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn reserve_datagram(
        &self,
        amount: usize,
    ) -> SocketResult<Option<(Reservation, Reservation, Reservation, Reservation)>> {
        let Some(ledger) = self.resource_ledger() else {
            return Ok(None);
        };
        let bytes = ledger
            .reserve(ResourceClass::BufferedBytes, amount)
            .map_err(SocketTableError::resource_limit)?;
        let datagram = ledger
            .reserve(ResourceClass::Datagrams, 1)
            .map_err(SocketTableError::resource_limit)?;
        let udp_bytes = ledger
            .reserve(ResourceClass::UdpBytes, amount)
            .map_err(SocketTableError::resource_limit)?;
        let udp_datagram = ledger
            .reserve(ResourceClass::UdpDatagrams, 1)
            .map_err(SocketTableError::resource_limit)?;
        Ok(Some((bytes, datagram, udp_bytes, udp_datagram)))
    }

    pub fn set_readiness_sink<F>(&self, sink: Option<F>)
    where
        F: Fn(SocketReadiness) + Send + Sync + 'static,
    {
        let mut target = lock_or_recover(&self.inner.readiness_sink);
        *target = sink.map(|sink| Arc::new(sink) as SocketReadinessSink);
    }

    fn emit_readiness(&self, readiness: Option<SocketReadiness>) {
        let Some(readiness) = readiness else {
            return;
        };
        let sink = lock_or_recover(&self.inner.readiness_sink).clone();
        if let Some(sink) = sink {
            sink(readiness);
        }
    }

    pub fn allocate(&self, owner_pid: u32, spec: SocketSpec) -> SocketResult<SocketRecord> {
        self.allocate_with_state(owner_pid, spec, SocketState::Created)
    }

    pub fn allocate_with_state(
        &self,
        owner_pid: u32,
        spec: SocketSpec,
        state: SocketState,
    ) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let socket_id = next_socket_id(&mut table)?;
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
        Ok(record)
    }

    pub fn get(&self, socket_id: SocketId) -> Option<SocketRecord> {
        lock_or_recover(&self.inner.state)
            .sockets
            .get(&socket_id)
            .cloned()
    }

    pub fn reassign_owner(&self, socket_id: SocketId, owner_pid: u32) -> SocketResult<()> {
        let mut table = lock_or_recover(&self.inner.state);
        let previous_owner = table
            .sockets
            .get(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?
            .owner_pid;
        if previous_owner == owner_pid {
            return Ok(());
        }
        if let Some(ids) = table.by_owner.get_mut(&previous_owner) {
            ids.remove(&socket_id);
            if ids.is_empty() {
                table.by_owner.remove(&previous_owner);
            }
        }
        table
            .by_owner
            .entry(owner_pid)
            .or_default()
            .insert(socket_id);
        table
            .sockets
            .get_mut(&socket_id)
            .expect("socket checked before owner reassignment")
            .owner_pid = owner_pid;
        Ok(())
    }

    pub fn records_for_owner(&self, owner_pid: u32) -> Vec<SocketRecord> {
        let table = lock_or_recover(&self.inner.state);
        let Some(socket_ids) = table.by_owner.get(&owner_pid) else {
            return Vec::new();
        };
        socket_ids
            .iter()
            .filter_map(|socket_id| table.sockets.get(socket_id).cloned())
            .collect()
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
        let mut address = normalize_inet_address(address);
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
        // POSIX bind(port=0) assigns a free ephemeral port (kernel-owned, so the
        // converged browser path does not need a host port allocator).
        if address.port() == 0 {
            let port = assign_ephemeral_inet_port(&table, existing.spec, address.host())?;
            address = normalize_inet_address(InetSocketAddress::new(address.host(), port));
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
        let readiness = {
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

            let was_empty = listener_state.pending_accepts.is_empty();
            listener_state.pending_accepts.push_back(PendingConnection {
                peer_address: Some(peer_address),
                peer_unix_path: None,
                accepted_socket_id: None,
            });
            was_empty.then_some(SocketReadiness {
                socket_id: listener_socket_id,
                kind: SocketReadinessKind::Accept,
            })
        };
        self.emit_readiness(readiness);
        Ok(())
    }

    pub fn accept(&self, listener_socket_id: SocketId) -> SocketResult<SocketRecord> {
        let mut table = lock_or_recover(&self.inner.state);
        let (owner_pid, spec, local_address, local_unix_path, needs_socket_id) = {
            let record = table
                .sockets
                .get(&listener_socket_id)
                .ok_or_else(|| SocketTableError::not_found(listener_socket_id))?;

            if record.state != SocketState::Listening {
                return Err(SocketTableError::invalid_argument(format!(
                    "socket {listener_socket_id} is not listening"
                )));
            }

            let listener_state = record.listener_state.as_ref().ok_or_else(|| {
                SocketTableError::invalid_argument(format!(
                    "socket {listener_socket_id} has no listener state"
                ))
            })?;
            let pending = listener_state.pending_accepts.front().ok_or_else(|| {
                SocketTableError::would_block(format!(
                    "listener {listener_socket_id} has no pending connections"
                ))
            })?;

            (
                record.owner_pid,
                record.spec,
                record.local_address.clone(),
                record.local_unix_path.clone(),
                pending.accepted_socket_id.is_none(),
            )
        };
        // External pending connections need a new kernel identity. Reserve it
        // before removing the backlog entry so exhaustion is a retryable,
        // non-destructive accept failure.
        let new_socket_id = needs_socket_id
            .then(|| next_socket_id(&mut table))
            .transpose()?;
        let pending = table
            .sockets
            .get_mut(&listener_socket_id)
            .and_then(|record| record.listener_state.as_mut())
            .and_then(|listener| listener.pending_accepts.pop_front())
            .ok_or_else(|| {
                SocketTableError::accounting_invariant(format!(
                    "listener {listener_socket_id} lost a pending accept during allocation"
                ))
            })?;

        if let Some(accepted_socket_id) = pending.accepted_socket_id {
            return table
                .sockets
                .get(&accepted_socket_id)
                .cloned()
                .ok_or_else(|| SocketTableError::not_found(accepted_socket_id));
        }

        let socket_id = new_socket_id.ok_or_else(|| {
            SocketTableError::accounting_invariant(format!(
                "listener {listener_socket_id} accepted an external connection without an id"
            ))
        })?;
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
        let (result, readiness) = {
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
            let mut accept_was_empty = false;
            let result = (|| {
                // Validate the listener and confirm backlog capacity BEFORE consuming a
                // socket id. The id counter is monotonic and never
                // reclaims, so allocating an id before this check leaks one on every
                // rejected connect (for example when the backlog is full).
                {
                    let listener = table
                        .sockets
                        .get(&listener_socket_id)
                        .ok_or_else(|| SocketTableError::not_found(listener_socket_id))?;
                    validate_connect_to_listener(&client, listener)?;

                    let listener_state = listener.listener_state.as_ref().ok_or_else(|| {
                        SocketTableError::invalid_argument(format!(
                            "socket {listener_socket_id} has no listener state"
                        ))
                    })?;
                    if listener_state.pending_accepts.len() >= listener_state.backlog {
                        return Err(SocketTableError::would_block(format!(
                            "listener {listener_socket_id} backlog is full"
                        )));
                    }
                }

                // Capacity confirmed: only now is it safe to consume a socket id.
                let accepted_socket_id = next_socket_id(&mut table)?;
                let listener = table
                    .sockets
                    .get_mut(&listener_socket_id)
                    .ok_or_else(|| SocketTableError::not_found(listener_socket_id))?;
                let listener_state = listener.listener_state.as_mut().ok_or_else(|| {
                    SocketTableError::invalid_argument(format!(
                        "socket {listener_socket_id} has no listener state"
                    ))
                })?;

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

                accept_was_empty = listener_state.pending_accepts.is_empty();
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

            let result = match result {
                Ok(accepted) => {
                    let accepted_socket_id = accepted.id;
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
            };
            let readiness = result.is_ok().then_some(()).and_then(|()| {
                accept_was_empty.then_some(SocketReadiness {
                    socket_id: listener_socket_id,
                    kind: SocketReadinessKind::Accept,
                })
            });
            (result, readiness)
        };
        self.emit_readiness(readiness);
        result
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
        let (result, readiness) = {
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
            let mut accept_was_empty = false;
            let result = (|| {
                // Validate the listener and confirm backlog capacity BEFORE consuming a
                // socket id. The id counter is monotonic and never
                // reclaims, so allocating an id before this check leaks one on every
                // rejected connect (for example when the backlog is full).
                {
                    let listener = table
                        .sockets
                        .get(&listener_socket_id)
                        .ok_or_else(|| SocketTableError::not_found(listener_socket_id))?;
                    validate_connect_to_listener(&client, listener)?;

                    let listener_state = listener.listener_state.as_ref().ok_or_else(|| {
                        SocketTableError::invalid_argument(format!(
                            "socket {listener_socket_id} has no listener state"
                        ))
                    })?;
                    if listener_state.pending_accepts.len() >= listener_state.backlog {
                        return Err(SocketTableError::would_block(format!(
                            "listener {listener_socket_id} backlog is full"
                        )));
                    }
                }

                // Capacity confirmed: only now is it safe to consume a socket id.
                let accepted_socket_id = next_socket_id(&mut table)?;
                let listener = table
                    .sockets
                    .get_mut(&listener_socket_id)
                    .ok_or_else(|| SocketTableError::not_found(listener_socket_id))?;
                let listener_state = listener.listener_state.as_mut().ok_or_else(|| {
                    SocketTableError::invalid_argument(format!(
                        "socket {listener_socket_id} has no listener state"
                    ))
                })?;

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

                accept_was_empty = listener_state.pending_accepts.is_empty();
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

            let result = match result {
                Ok(accepted) => {
                    let accepted_socket_id = accepted.id;
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
            };
            let readiness = result.is_ok().then_some(()).and_then(|()| {
                accept_was_empty.then_some(SocketReadiness {
                    socket_id: listener_socket_id,
                    kind: SocketReadinessKind::Accept,
                })
            });
            (result, readiness)
        };
        self.emit_readiness(readiness);
        result
    }

    pub fn send_to_bound_udp_socket(
        &self,
        socket_id: SocketId,
        target_address: InetSocketAddress,
        data: &[u8],
    ) -> SocketResult<usize> {
        let target_address = normalize_inet_address(target_address);
        let readiness = {
            let mut table = lock_or_recover(&self.inner.state);
            let sender = table
                .sockets
                .get(&socket_id)
                .cloned()
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;
            validate_bound_udp_sender(&sender)?;
            let source_address = sender.local_address.as_ref().map(|source| {
                if source.host() == "0.0.0.0" || source.host() == "::" {
                    InetSocketAddress::new(target_address.host(), source.port())
                } else {
                    source.clone()
                }
            });

            let receiver_socket_id = lookup_bound_inet_datagram_socket_in_table(
                &table.bound_inet_datagrams,
                &target_address,
            )
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

            // A connected UDP socket only admits datagrams from its selected
            // peer. The sender still observes a successful datagram send, as
            // it would when the network drops a packet before delivery.
            if receiver.peer_address.is_some()
                && receiver.peer_address.as_ref() != source_address.as_ref()
            {
                return Ok(data.len());
            }

            let datagram_state = receiver.datagram_state.as_mut().ok_or_else(|| {
                SocketTableError::invalid_argument(format!(
                    "socket {receiver_socket_id} does not support datagrams"
                ))
            })?;
            #[cfg(not(target_arch = "wasm32"))]
            let retained = self.reserve_datagram(data.len())?;
            let was_empty = datagram_state.recv_queue.is_empty();
            datagram_state.recv_queue.push_back(QueuedDatagram {
                source_address,
                payload: data.to_vec(),
            });
            #[cfg(not(target_arch = "wasm32"))]
            if let Some((bytes, datagram, udp_bytes, udp_datagram)) = retained {
                let resources = table
                    .retained_resources
                    .entry(receiver_socket_id)
                    .or_default();
                resources.buffered_bytes.push_back(bytes);
                resources.datagrams.push_back(datagram);
                resources.udp_bytes.push_back(udp_bytes);
                resources.udp_datagrams.push_back(udp_datagram);
            }
            was_empty.then_some(SocketReadiness {
                socket_id: receiver_socket_id,
                kind: SocketReadinessKind::Data,
            })
        };
        self.emit_readiness(readiness);
        Ok(data.len())
    }

    pub fn connect_bound_udp_socket(
        &self,
        socket_id: SocketId,
        peer_address: InetSocketAddress,
    ) -> SocketResult<()> {
        let mut table = lock_or_recover(&self.inner.state);
        let socket = table
            .sockets
            .get_mut(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        validate_bound_udp_sender(socket)?;
        if socket.peer_address.is_some() {
            return Err(SocketTableError::invalid_argument(format!(
                "UDP socket {socket_id} is already connected"
            )));
        }
        socket.peer_address = Some(normalize_inet_address(peer_address));
        Ok(())
    }

    pub fn disconnect_bound_udp_socket(&self, socket_id: SocketId) -> SocketResult<()> {
        let mut table = lock_or_recover(&self.inner.state);
        let socket = table
            .sockets
            .get_mut(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        validate_bound_udp_sender(socket)?;
        if socket.peer_address.take().is_none() {
            return Err(SocketTableError::not_connected(format!(
                "UDP socket {socket_id} is not connected"
            )));
        }
        Ok(())
    }

    pub fn send_connected_udp_socket(
        &self,
        socket_id: SocketId,
        data: &[u8],
    ) -> SocketResult<usize> {
        let peer_address = {
            let table = lock_or_recover(&self.inner.state);
            let socket = table
                .sockets
                .get(&socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;
            validate_bound_udp_sender(socket)?;
            socket.peer_address.clone().ok_or_else(|| {
                SocketTableError::not_connected(format!("UDP socket {socket_id} is not connected"))
            })?
        };
        self.send_to_bound_udp_socket(socket_id, peer_address, data)
    }

    pub fn check_send_to_bound_udp_socket(
        &self,
        socket_id: SocketId,
        target_address: InetSocketAddress,
    ) -> SocketResult<()> {
        let target_address = normalize_inet_address(target_address);
        let table = lock_or_recover(&self.inner.state);
        let sender = table
            .sockets
            .get(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        validate_bound_udp_sender(sender)?;

        let receiver_socket_id = lookup_bound_inet_datagram_socket_in_table(
            &table.bound_inet_datagrams,
            &target_address,
        )
        .ok_or_else(|| {
            SocketTableError::not_found_address(format!(
                "no UDP socket bound at {}:{}",
                target_address.host(),
                target_address.port()
            ))
        })?;
        let receiver = table
            .sockets
            .get(&receiver_socket_id)
            .ok_or_else(|| SocketTableError::not_found(receiver_socket_id))?;
        validate_bound_udp_receiver(receiver)?;
        Ok(())
    }

    pub fn recv_datagram(
        &self,
        socket_id: SocketId,
        max_bytes: usize,
    ) -> SocketResult<Option<ReceivedDatagram>> {
        let (datagram, reservations) = self.recv_datagram_inner(socket_id, max_bytes)?;
        drop(reservations);
        Ok(datagram)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn recv_datagram_charged(
        &self,
        socket_id: SocketId,
        max_bytes: usize,
    ) -> SocketResult<Option<ChargedReceivedDatagram>> {
        let (datagram, reservations) = self.recv_datagram_inner(socket_id, max_bytes)?;
        Ok(datagram.map(|datagram| ChargedReceivedDatagram {
            datagram,
            reservations,
        }))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn recv_datagram_inner(
        &self,
        socket_id: SocketId,
        max_bytes: usize,
    ) -> SocketResult<(Option<ReceivedDatagram>, Option<DatagramReservations>)> {
        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        validate_bound_udp_receiver(record)?;
        let datagram_state = record.datagram_state.as_ref().ok_or_else(|| {
            SocketTableError::invalid_argument(format!(
                "socket {socket_id} does not support datagrams"
            ))
        })?;
        if datagram_state.recv_queue.is_empty() {
            return Err(SocketTableError::would_block(format!(
                "socket {socket_id} has no queued datagrams"
            )));
        }
        // Validate and transfer accounting ownership before removing the
        // payload. An internal ownership mismatch must not silently discard a
        // guest datagram and leave the queue/accountant out of sync.
        let reservations = if self.has_resource_ledger() {
            Some(take_retained_datagram(&mut table, socket_id)?)
        } else {
            None
        };
        let datagram = table
            .sockets
            .get_mut(&socket_id)
            .and_then(|record| record.datagram_state.as_mut())
            .and_then(|state| state.recv_queue.pop_front())
            .ok_or_else(|| {
                SocketTableError::accounting_invariant(format!(
                    "socket {socket_id} lost a datagram during charged transfer"
                ))
            })?;
        let mut payload = datagram.payload;
        // Truncate in place so the original backing allocation and its full
        // byte reservation move together. Copying a prefix here would require
        // a second reservation until the source allocation was released.
        payload.truncate(max_bytes);
        Ok((
            Some(ReceivedDatagram {
                source_address: datagram.source_address,
                payload,
            }),
            reservations,
        ))
    }

    #[cfg(target_arch = "wasm32")]
    fn recv_datagram_inner(
        &self,
        socket_id: SocketId,
        max_bytes: usize,
    ) -> SocketResult<(Option<ReceivedDatagram>, Option<()>)> {
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
        let mut payload = datagram.payload;
        payload.truncate(max_bytes);
        Ok((
            Some(ReceivedDatagram {
                source_address: datagram.source_address,
                payload,
            }),
            None,
        ))
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

                if requested.intersects(POLLIN) && connection.has_buffered_data() {
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
        let readiness = {
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

            #[cfg(not(target_arch = "wasm32"))]
            let retained = self.reserve_buffered_bytes(data.len())?;
            let was_empty = !peer_connection.has_buffered_data();
            peer_connection.push_recv(data, Vec::new());
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(retained) = retained {
                table
                    .retained_resources
                    .entry(peer_socket_id)
                    .or_default()
                    .buffered_bytes
                    .push_back(retained);
            }
            (was_empty && !data.is_empty()).then_some(SocketReadiness {
                socket_id: peer_socket_id,
                kind: SocketReadinessKind::Data,
            })
        };
        self.emit_readiness(readiness);
        Ok(data.len())
    }

    pub fn send_message(
        &self,
        socket_id: SocketId,
        data: &[u8],
        rights: Vec<TransferredSocketRight>,
    ) -> SocketResult<usize> {
        let readiness = {
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
            if data.is_empty() && record.spec.socket_type == SocketType::Stream {
                return Ok(0);
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
            let was_empty = !peer_connection.has_buffered_data();
            peer_connection.push_recv(data, rights);
            was_empty.then_some(SocketReadiness {
                socket_id: peer_socket_id,
                kind: SocketReadinessKind::Data,
            })
        };
        self.emit_readiness(readiness);
        Ok(data.len())
    }

    pub fn check_write(&self, socket_id: SocketId) -> SocketResult<()> {
        let table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get(&socket_id)
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
        let peer = table.sockets.get(&peer_socket_id).ok_or_else(|| {
            SocketTableError::broken_pipe(format!("socket {socket_id} peer is closed"))
        })?;
        let peer_connection = peer.connection_state.as_ref().ok_or_else(|| {
            SocketTableError::broken_pipe(format!("socket {socket_id} peer is closed"))
        })?;
        if peer_connection.read_shutdown {
            return Err(SocketTableError::broken_pipe(format!(
                "socket {peer_socket_id} read side is shut down"
            )));
        }

        Ok(())
    }

    pub fn read(&self, socket_id: SocketId, max_bytes: usize) -> SocketResult<Option<Vec<u8>>> {
        if max_bytes == 0 {
            return Ok(Some(Vec::new()));
        }

        let mut table = lock_or_recover(&self.inner.state);
        let record_ref = table
            .sockets
            .get(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        let clone_started = SOCKET_READ_TRACE_ENABLED
            .load(Ordering::Relaxed)
            .then(Instant::now);
        let record = record_ref.clone();
        if let Some(started) = clone_started {
            SOCKET_READ_TRACE_COUNTERS
                .socket_record_clone_calls
                .fetch_add(1, Ordering::Relaxed);
            SOCKET_READ_TRACE_COUNTERS.socket_record_clone_us.fetch_add(
                u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
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
        if connection.has_buffered_data() {
            let record = table
                .sockets
                .get_mut(&socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;
            let connection = record.connection_state.as_mut().ok_or_else(|| {
                SocketTableError::not_connected(format!("socket {socket_id} is not connected"))
            })?;
            let result =
                connection.read_recv(max_bytes, record.spec.socket_type != SocketType::Stream);
            #[cfg(not(target_arch = "wasm32"))]
            if self.has_resource_ledger() {
                if let Some(read) = result.as_ref() {
                    release_retained_bytes(&mut table, socket_id, read.len())?;
                }
            }
            return Ok(result);
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

    pub fn recv_message(
        &self,
        socket_id: SocketId,
        max_bytes: usize,
        peek: bool,
    ) -> SocketResult<Option<ReceivedSocketMessage>> {
        let mut table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get_mut(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        if record.state != SocketState::Connected {
            return Err(SocketTableError::not_connected(format!(
                "socket {socket_id} is not connected"
            )));
        }
        let message_oriented = record.spec.socket_type != SocketType::Stream;
        let connection = record.connection_state.as_mut().ok_or_else(|| {
            SocketTableError::not_connected(format!("socket {socket_id} is not connected"))
        })?;
        if connection.read_shutdown {
            return Ok(None);
        }
        if connection.has_buffered_data() {
            return Ok(if peek {
                connection.peek_recv_message(max_bytes, message_oriented)
            } else {
                connection.read_recv_message(max_bytes, message_oriented)
            });
        }
        if connection.peer_write_shutdown {
            return Ok(None);
        }
        Err(SocketTableError::would_block(format!(
            "socket {socket_id} has no readable data"
        )))
    }

    pub fn next_message_rights_count(&self, socket_id: SocketId) -> SocketResult<usize> {
        let table = lock_or_recover(&self.inner.state);
        let record = table
            .sockets
            .get(&socket_id)
            .ok_or_else(|| SocketTableError::not_found(socket_id))?;
        let connection = record.connection_state.as_ref().ok_or_else(|| {
            SocketTableError::not_connected(format!("socket {socket_id} is not connected"))
        })?;
        Ok(connection
            .recv_buffer
            .iter()
            .take(1)
            .map(|chunk| chunk.rights.len())
            .sum())
    }

    pub fn shutdown(&self, socket_id: SocketId, how: SocketShutdown) -> SocketResult<SocketRecord> {
        let (record, readiness) = {
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
                connection.clear_recv();
                #[cfg(not(target_arch = "wasm32"))]
                release_all_retained_bytes(&mut table, socket_id);
                connection.read_shutdown = true;
            }
            let mut readiness = None;
            if matches!(how, SocketShutdown::Write | SocketShutdown::Both) {
                connection.write_shutdown = true;
                if let Some(peer_socket_id) = connection.peer_socket_id {
                    if let Some(peer) = table.sockets.get_mut(&peer_socket_id) {
                        if let Some(peer_connection) = peer.connection_state.as_mut() {
                            let became_eof_ready = !peer_connection.peer_write_shutdown;
                            peer_connection.peer_write_shutdown = true;
                            readiness = became_eof_ready.then_some(SocketReadiness {
                                socket_id: peer_socket_id,
                                kind: SocketReadinessKind::Data,
                            });
                        }
                    }
                }
            }

            let mut record = record;
            record.connection_state = Some(connection);
            let cloned = record.clone();
            table.sockets.insert(socket_id, record);
            (cloned, readiness)
        };
        self.emit_readiness(readiness);
        Ok(record)
    }

    pub fn remove(&self, socket_id: SocketId) -> SocketResult<SocketRecord> {
        let (record, readiness) = {
            let mut table = lock_or_recover(&self.inner.state);
            let readiness = peer_eof_readiness(&table, socket_id);
            let record = remove_socket(&mut table, socket_id)
                .ok_or_else(|| SocketTableError::not_found(socket_id))?;
            (record, readiness)
        };
        self.emit_readiness(readiness);
        Ok(record)
    }

    pub fn remove_all_for_pid(&self, owner_pid: u32) -> Vec<SocketRecord> {
        let (records, readiness) = {
            let mut table = lock_or_recover(&self.inner.state);
            let Some(socket_ids) = table.by_owner.remove(&owner_pid) else {
                return Vec::new();
            };
            let mut readiness = Vec::new();
            let records = socket_ids
                .into_iter()
                .filter_map(|socket_id| {
                    if let Some(event) = peer_eof_readiness(&table, socket_id) {
                        readiness.push(event);
                    }
                    remove_socket(&mut table, socket_id)
                })
                .collect();
            readiness.retain(|event| table.sockets.contains_key(&event.socket_id));
            (records, readiness)
        };
        for event in readiness {
            self.emit_readiness(Some(event));
        }
        records
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
            if let Some(connection) = &record.connection_state {
                snapshot.buffered_bytes = snapshot
                    .buffered_bytes
                    .saturating_add(connection.buffered_len());
                if record.spec.socket_type != SocketType::Stream {
                    snapshot.datagram_queue_len = snapshot
                        .datagram_queue_len
                        .saturating_add(connection.recv_buffer.len());
                }
            }
            if let Some(datagram_state) = &record.datagram_state {
                snapshot.datagram_queue_len = snapshot
                    .datagram_queue_len
                    .saturating_add(datagram_state.recv_queue.len());
                snapshot.buffered_bytes = snapshot
                    .buffered_bytes
                    .saturating_add(datagram_queue_bytes(&datagram_state.recv_queue));
            }
        }
        snapshot
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn release_retained_bytes(
    table: &mut SocketTableState,
    socket_id: SocketId,
    mut amount: usize,
) -> SocketResult<()> {
    if amount == 0 {
        return Ok(());
    }
    let resources = table
        .retained_resources
        .get_mut(&socket_id)
        .ok_or_else(|| {
            SocketTableError::accounting_invariant(format!(
                "socket {socket_id} released {amount} unowned buffered bytes"
            ))
        })?;
    let owned = resources
        .buffered_bytes
        .iter()
        .try_fold(0usize, |total, reservation| {
            total.checked_add(reservation.amount())
        })
        .ok_or_else(|| {
            SocketTableError::accounting_invariant(format!(
                "socket {socket_id} buffered-byte ownership overflowed"
            ))
        })?;
    if owned < amount {
        return Err(SocketTableError::accounting_invariant(format!(
            "socket {socket_id} released {amount} buffered bytes but owns only {owned}"
        )));
    }
    while amount > 0 {
        let reservation = resources.buffered_bytes.front_mut().ok_or_else(|| {
            SocketTableError::accounting_invariant(format!(
                "socket {socket_id} released more buffered bytes than it owns"
            ))
        })?;
        if reservation.amount() <= amount {
            amount -= reservation.amount();
            resources.buffered_bytes.pop_front();
        } else {
            let released = reservation.split(amount).ok_or_else(|| {
                SocketTableError::accounting_invariant(format!(
                    "socket {socket_id} could not split its buffered-byte reservation"
                ))
            })?;
            drop(released);
            amount = 0;
        }
    }
    if resources.buffered_bytes.is_empty()
        && resources.datagrams.is_empty()
        && resources.udp_bytes.is_empty()
        && resources.udp_datagrams.is_empty()
    {
        table.retained_resources.remove(&socket_id);
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn take_retained_datagram(
    table: &mut SocketTableState,
    socket_id: SocketId,
) -> SocketResult<(Reservation, Reservation, Reservation, Reservation)> {
    let resources = table
        .retained_resources
        .get_mut(&socket_id)
        .ok_or_else(|| {
            SocketTableError::accounting_invariant(format!(
                "socket {socket_id} transferred an unowned datagram"
            ))
        })?;
    if resources.buffered_bytes.is_empty()
        || resources.datagrams.is_empty()
        || resources.udp_bytes.is_empty()
        || resources.udp_datagrams.is_empty()
    {
        return Err(SocketTableError::accounting_invariant(format!(
            "socket {socket_id} has incomplete datagram ownership"
        )));
    }
    let bytes = resources.buffered_bytes.pop_front().ok_or_else(|| {
        SocketTableError::accounting_invariant(format!(
            "socket {socket_id} transferred a datagram without buffered-byte ownership"
        ))
    })?;
    let datagram = resources.datagrams.pop_front().ok_or_else(|| {
        SocketTableError::accounting_invariant(format!(
            "socket {socket_id} transferred more datagrams than it owns"
        ))
    })?;
    let udp_bytes = resources.udp_bytes.pop_front().ok_or_else(|| {
        SocketTableError::accounting_invariant(format!(
            "socket {socket_id} transferred a datagram without UDP-byte ownership"
        ))
    })?;
    let udp_datagram = resources.udp_datagrams.pop_front().ok_or_else(|| {
        SocketTableError::accounting_invariant(format!(
            "socket {socket_id} transferred more UDP datagrams than it owns"
        ))
    })?;
    if resources.buffered_bytes.is_empty()
        && resources.datagrams.is_empty()
        && resources.udp_bytes.is_empty()
        && resources.udp_datagrams.is_empty()
    {
        table.retained_resources.remove(&socket_id);
    }
    Ok((bytes, datagram, udp_bytes, udp_datagram))
}

#[cfg(not(target_arch = "wasm32"))]
fn release_all_retained_bytes(table: &mut SocketTableState, socket_id: SocketId) {
    let remove_entry = if let Some(resources) = table.retained_resources.get_mut(&socket_id) {
        resources.buffered_bytes.clear();
        resources.datagrams.is_empty()
            && resources.udp_bytes.is_empty()
            && resources.udp_datagrams.is_empty()
    } else {
        false
    };
    if remove_entry {
        table.retained_resources.remove(&socket_id);
    }
}

fn datagram_queue_bytes(queue: &VecDeque<QueuedDatagram>) -> usize {
    queue
        .iter()
        .map(|datagram| datagram.payload.len())
        .sum::<usize>()
}

fn next_socket_id(table: &mut SocketTableState) -> SocketResult<SocketId> {
    if table.next_socket_id == 0 {
        table.next_socket_id = 1;
    }
    let socket_id = table.next_socket_id;
    table.next_socket_id = table
        .next_socket_id
        .checked_add(1)
        .ok_or_else(SocketTableError::id_exhausted)?;
    Ok(socket_id)
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
    if socket.spec != peer.spec {
        return Err(SocketTableError::invalid_argument(format!(
            "socket {} and peer {} have incompatible types",
            socket.id, peer.id
        )));
    }
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
        || (spec.domain == SocketDomain::Unix
            && matches!(
                spec.socket_type,
                SocketType::Datagram | SocketType::SeqPacket
            ))
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

/// Pick a free ephemeral port for `bind(port=0)`, scanning the IANA dynamic
/// range and skipping any already in conflict for `spec`/`host`.
fn assign_ephemeral_inet_port(
    table: &SocketTableState,
    spec: SocketSpec,
    host: &str,
) -> SocketResult<u16> {
    const EPHEMERAL_START: u16 = 49152;
    const EPHEMERAL_END: u16 = 65535;
    for port in EPHEMERAL_START..=EPHEMERAL_END {
        let candidate = normalize_inet_address(InetSocketAddress::new(host, port));
        if lookup_conflicting_bound_inet_socket_ids(table, spec, &candidate).is_empty() {
            return Ok(port);
        }
    }
    Err(SocketTableError::address_in_use(
        "no free ephemeral port available",
    ))
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

fn peer_eof_readiness(table: &SocketTableState, socket_id: SocketId) -> Option<SocketReadiness> {
    let peer_socket_id = table
        .sockets
        .get(&socket_id)?
        .connection_state
        .as_ref()?
        .peer_socket_id?;
    let peer_connection = table
        .sockets
        .get(&peer_socket_id)?
        .connection_state
        .as_ref()?;
    (!peer_connection.peer_write_shutdown).then_some(SocketReadiness {
        socket_id: peer_socket_id,
        kind: SocketReadinessKind::Data,
    })
}

fn remove_socket(table: &mut SocketTableState, socket_id: SocketId) -> Option<SocketRecord> {
    let record = table.sockets.remove(&socket_id)?;
    #[cfg(not(target_arch = "wasm32"))]
    table.retained_resources.remove(&socket_id);
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(target_arch = "wasm32"))]
    use crate::admission::{ResourceLimit, ResourceUsage};

    /// Reads the monotonic socket-id counter without advancing it, so a test can
    /// observe whether a code path consumed an id.
    fn peek_next_socket_id(table: &SocketTable) -> SocketId {
        lock_or_recover(&table.inner.state).next_socket_id
    }

    #[test]
    fn exhausted_socket_ids_fail_without_reusing_a_live_identity() {
        let table = SocketTable::new();
        lock_or_recover(&table.inner.state).next_socket_id = u64::MAX;

        let error = table
            .allocate(1, SocketSpec::tcp())
            .expect_err("exhausted id space must reject allocation");

        assert_eq!(error.code(), "EMFILE");
        assert!(error
            .to_string()
            .contains("ERR_AGENTOS_SOCKET_ID_EXHAUSTED"));
        assert_eq!(table.snapshot().sockets, 0);
    }

    #[test]
    fn exhausted_accept_preserves_the_pending_connection() {
        let table = SocketTable::new();
        let listener = table
            .allocate(1, SocketSpec::tcp())
            .expect("allocate listener");
        let address = InetSocketAddress::new("127.0.0.1", 43001);
        table
            .bind_inet(listener.id(), address)
            .expect("bind listener");
        table.listen(listener.id(), 1).expect("listen");
        table
            .enqueue_incoming_tcp_connection(
                listener.id(),
                InetSocketAddress::new("127.0.0.1", 43002),
            )
            .expect("queue incoming connection");
        lock_or_recover(&table.inner.state).next_socket_id = u64::MAX;

        let error = table
            .accept(listener.id())
            .expect_err("exhausted id space must reject accept");

        assert_eq!(error.code(), "EMFILE");
        assert_eq!(
            table
                .get(listener.id())
                .expect("listener remains live")
                .pending_accept_count(),
            1
        );
    }

    #[test]
    fn full_backlog_unix_connect_does_not_consume_socket_id() {
        let table = SocketTable::new();
        let path = "/tmp/leak-test/server.sock";

        let listener = table
            .allocate(1, SocketSpec::unix_stream())
            .expect("allocate listener");
        table
            .bind_unix(listener.id, path)
            .expect("bind unix listener");
        table.listen(listener.id, 1).expect("listen with backlog 1");

        // Fill the only backlog slot with one pending connection.
        let first = table
            .allocate(2, SocketSpec::unix_stream())
            .expect("allocate first client");
        table
            .connect_to_bound_unix_stream(first.id, path)
            .expect("first connect fills the backlog");

        // A second connect must be rejected because the backlog is full, and it
        // must NOT consume a socket id (the counter is monotonic and never reclaims).
        let second = table
            .allocate(2, SocketSpec::unix_stream())
            .expect("allocate second client");
        let before = peek_next_socket_id(&table);
        let error = table
            .connect_to_bound_unix_stream(second.id, path)
            .expect_err("full-backlog connect must fail");
        assert_eq!(error.code(), "EAGAIN");
        let after = peek_next_socket_id(&table);

        assert_eq!(
            before, after,
            "full-backlog unix connect leaked a socket id (counter advanced from {before} to {after})"
        );
    }

    #[test]
    fn full_backlog_inet_connect_does_not_consume_socket_id() {
        let table = SocketTable::new();
        let target = InetSocketAddress::new("127.0.0.1", 49222);

        let listener = table
            .allocate(1, SocketSpec::tcp())
            .expect("allocate listener");
        table
            .bind_inet(listener.id, target.clone())
            .expect("bind inet listener");
        table.listen(listener.id, 1).expect("listen with backlog 1");

        // Fill the only backlog slot with one pending connection.
        let first = table
            .allocate(2, SocketSpec::tcp())
            .expect("allocate first client");
        table
            .connect_to_bound_inet_stream(first.id, target.clone())
            .expect("first connect fills the backlog");

        // A second connect must be rejected because the backlog is full, and it
        // must NOT consume a socket id (the counter is monotonic and never reclaims).
        let second = table
            .allocate(2, SocketSpec::tcp())
            .expect("allocate second client");
        let before = peek_next_socket_id(&table);
        let error = table
            .connect_to_bound_inet_stream(second.id, target)
            .expect_err("full-backlog connect must fail");
        assert_eq!(error.code(), "EAGAIN");
        let after = peek_next_socket_id(&table);

        assert_eq!(
            before, after,
            "full-backlog inet connect leaked a socket id (counter advanced from {before} to {after})"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn test_resource_ledger(buffered_bytes: usize, datagrams: usize) -> Arc<ResourceLedger> {
        Arc::new(ResourceLedger::root(
            "test-vm",
            [
                (
                    ResourceClass::BufferedBytes,
                    ResourceLimit::new(buffered_bytes, "test.maxBufferedBytes"),
                ),
                (
                    ResourceClass::Datagrams,
                    ResourceLimit::new(datagrams, "test.maxDatagrams"),
                ),
                (
                    ResourceClass::UdpBytes,
                    ResourceLimit::new(buffered_bytes, "limits.udp.maxBufferedBytes"),
                ),
                (
                    ResourceClass::UdpDatagrams,
                    ResourceLimit::new(datagrams, "limits.udp.maxBufferedDatagrams"),
                ),
            ],
        ))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn usage(ledger: &ResourceLedger, class: ResourceClass) -> ResourceUsage {
        ledger.usage(class)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn stream_queue_reservations_follow_write_read_shutdown_and_close() {
        let table = SocketTable::new();
        let ledger = test_resource_ledger(5, 1);
        table
            .set_resource_ledger(Arc::clone(&ledger))
            .expect("install resource ledger");

        let writer = table
            .allocate(1, SocketSpec::tcp())
            .expect("allocate writer");
        let reader = table
            .allocate(2, SocketSpec::tcp())
            .expect("allocate reader");
        table
            .connect_pair(writer.id(), reader.id())
            .expect("connect stream pair");

        // Socket and connection counts belong to CapabilityRegistry, not the
        // kernel queue owner.
        assert_eq!(usage(&ledger, ResourceClass::Sockets).used, 0);
        assert_eq!(usage(&ledger, ResourceClass::Connections).used, 0);

        table.write(writer.id(), b"12345").expect("fill queue");
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 5);
        assert!(!table.buffered_byte_capacity_available());
        let error = table
            .write(writer.id(), b"6")
            .expect_err("write beyond retained-byte limit must fail");
        assert_eq!(error.code(), "EAGAIN");
        assert_eq!(table.snapshot().buffered_bytes, 5);
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 5);

        assert_eq!(
            table.read(reader.id(), 2).expect("partial read"),
            Some(b"12".to_vec())
        );
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 3);
        assert!(table.buffered_byte_capacity_available());

        table
            .shutdown(reader.id(), SocketShutdown::Read)
            .expect("discard unread queue");
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 0);

        table.remove(writer.id()).expect("close writer");
        table.remove(reader.id()).expect("close reader");
        assert!(ledger.is_zero());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn retained_byte_mismatch_fails_before_releasing_owned_capacity() {
        let table = SocketTable::new();
        let ledger = test_resource_ledger(8, 1);
        table
            .set_resource_ledger(Arc::clone(&ledger))
            .expect("install resource ledger");
        let writer = table
            .allocate(1, SocketSpec::tcp())
            .expect("allocate writer");
        let reader = table
            .allocate(2, SocketSpec::tcp())
            .expect("allocate reader");
        table
            .connect_pair(writer.id(), reader.id())
            .expect("connect stream pair");
        table.write(writer.id(), b"abc").expect("queue bytes");

        let mut state = lock_or_recover(&table.inner.state);
        let error = release_retained_bytes(&mut state, reader.id(), 4)
            .expect_err("over-release must fail atomically");
        assert_eq!(error.code(), "EIO");
        assert_eq!(
            state
                .retained_resources
                .get(&reader.id())
                .expect("retained ownership")
                .buffered_bytes
                .front()
                .expect("byte reservation")
                .amount(),
            3
        );
        drop(state);
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 3);
        assert_eq!(table.snapshot().buffered_bytes, 3);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn datagram_reservations_are_atomic_and_release_full_truncated_payload() {
        let table = SocketTable::new();
        let ledger = test_resource_ledger(8, 1);
        table
            .set_resource_ledger(Arc::clone(&ledger))
            .expect("install resource ledger");

        let sender = table
            .allocate(1, SocketSpec::udp())
            .expect("allocate sender");
        let receiver = table
            .allocate(2, SocketSpec::udp())
            .expect("allocate receiver");
        let sender_address = InetSocketAddress::new("127.0.0.1", 41001);
        let receiver_address = InetSocketAddress::new("127.0.0.1", 41002);
        table
            .bind_inet(sender.id(), sender_address)
            .expect("bind sender");
        table
            .bind_inet(receiver.id(), receiver_address.clone())
            .expect("bind receiver");

        table
            .send_to_bound_udp_socket(sender.id(), receiver_address.clone(), b"abc")
            .expect("enqueue datagram");
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 3);
        assert_eq!(usage(&ledger, ResourceClass::Datagrams).used, 1);
        assert_eq!(usage(&ledger, ResourceClass::UdpBytes).used, 3);
        assert_eq!(usage(&ledger, ResourceClass::UdpDatagrams).used, 1);
        assert!(!table.datagram_capacity_available());

        let error = table
            .send_to_bound_udp_socket(sender.id(), receiver_address.clone(), b"def")
            .expect_err("second datagram must fail atomically");
        assert_eq!(error.code(), "EAGAIN");
        assert_eq!(table.snapshot().datagram_queue_len, 1);
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 3);
        assert_eq!(usage(&ledger, ResourceClass::Datagrams).used, 1);
        assert_eq!(usage(&ledger, ResourceClass::UdpBytes).used, 3);
        assert_eq!(usage(&ledger, ResourceClass::UdpDatagrams).used, 1);

        let received = table
            .recv_datagram(receiver.id(), 1)
            .expect("receive datagram")
            .expect("queued datagram");
        assert_eq!(received.payload(), b"a");
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 0);
        assert_eq!(usage(&ledger, ResourceClass::Datagrams).used, 0);
        assert_eq!(usage(&ledger, ResourceClass::UdpBytes).used, 0);
        assert_eq!(usage(&ledger, ResourceClass::UdpDatagrams).used, 0);

        table
            .send_to_bound_udp_socket(sender.id(), receiver_address.clone(), b"charged")
            .expect("enqueue charged handoff");
        let charged = table
            .recv_datagram_charged(receiver.id(), 1)
            .expect("charged receive")
            .expect("charged datagram");
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 7);
        assert_eq!(usage(&ledger, ResourceClass::Datagrams).used, 1);
        assert_eq!(usage(&ledger, ResourceClass::UdpBytes).used, 7);
        assert_eq!(usage(&ledger, ResourceClass::UdpDatagrams).used, 1);
        let error = table
            .send_to_bound_udp_socket(sender.id(), receiver_address.clone(), b"blocked")
            .expect_err("guest-bound datagram ownership must retain the count permit");
        assert_eq!(error.code(), "EAGAIN");
        let (_, payload, reservations) = charged.into_parts();
        assert_eq!(payload, b"c");
        let (bytes, datagram, udp_bytes, udp_datagram) = reservations.expect("charged ownership");
        assert_eq!(bytes.amount(), 7);
        assert_eq!(datagram.amount(), 1);
        assert_eq!(udp_bytes.amount(), 7);
        assert_eq!(udp_datagram.amount(), 1);
        drop((bytes, datagram, udp_bytes, udp_datagram));
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 0);
        assert_eq!(usage(&ledger, ResourceClass::Datagrams).used, 0);
        assert_eq!(usage(&ledger, ResourceClass::UdpBytes).used, 0);
        assert_eq!(usage(&ledger, ResourceClass::UdpDatagrams).used, 0);

        table
            .send_to_bound_udp_socket(sender.id(), receiver_address, b"queued")
            .expect("enqueue replacement datagram");
        table.remove(receiver.id()).expect("close queued receiver");
        assert_eq!(usage(&ledger, ResourceClass::BufferedBytes).used, 0);
        assert_eq!(usage(&ledger, ResourceClass::Datagrams).used, 0);
        assert_eq!(usage(&ledger, ResourceClass::UdpBytes).used, 0);
        assert_eq!(usage(&ledger, ResourceClass::UdpDatagrams).used, 0);
        table.remove(sender.id()).expect("close sender");
        assert!(ledger.is_zero());
    }

    #[test]
    fn connected_udp_filters_non_peer_datagrams_and_disconnect_restores_delivery() {
        let table = SocketTable::new();
        let allowed_sender = table
            .allocate(1, SocketSpec::udp())
            .expect("allocate allowed sender");
        let other_sender = table
            .allocate(2, SocketSpec::udp())
            .expect("allocate other sender");
        let receiver = table
            .allocate(3, SocketSpec::udp())
            .expect("allocate receiver");
        let allowed_address = InetSocketAddress::new("127.0.0.1", 41101);
        let other_address = InetSocketAddress::new("127.0.0.1", 41102);
        let receiver_address = InetSocketAddress::new("127.0.0.1", 41103);
        table
            .bind_inet(allowed_sender.id(), allowed_address.clone())
            .expect("bind allowed sender");
        table
            .bind_inet(other_sender.id(), other_address)
            .expect("bind other sender");
        table
            .bind_inet(receiver.id(), receiver_address.clone())
            .expect("bind receiver");

        table
            .connect_bound_udp_socket(receiver.id(), allowed_address.clone())
            .expect("connect receiver to allowed peer");
        assert_eq!(
            table
                .get(receiver.id())
                .expect("connected receiver")
                .peer_address(),
            Some(&allowed_address)
        );

        assert_eq!(
            table
                .send_to_bound_udp_socket(other_sender.id(), receiver_address.clone(), b"drop")
                .expect("non-peer send still succeeds"),
            4
        );
        assert_eq!(
            table
                .recv_datagram(receiver.id(), usize::MAX)
                .expect_err("connected receiver must filter another peer")
                .code(),
            "EAGAIN"
        );

        table
            .send_to_bound_udp_socket(allowed_sender.id(), receiver_address.clone(), b"allowed")
            .expect("send from connected peer");
        let received = table
            .recv_datagram(receiver.id(), usize::MAX)
            .expect("receive from connected peer")
            .expect("connected datagram");
        assert_eq!(received.payload(), b"allowed");
        assert_eq!(received.source_address(), Some(&allowed_address));

        table
            .send_connected_udp_socket(receiver.id(), b"reply")
            .expect("send through connected peer state");
        let reply = table
            .recv_datagram(allowed_sender.id(), usize::MAX)
            .expect("receive connected reply")
            .expect("connected reply datagram");
        assert_eq!(reply.payload(), b"reply");
        assert_eq!(reply.source_address(), Some(&receiver_address));

        table
            .disconnect_bound_udp_socket(receiver.id())
            .expect("disconnect receiver");
        assert!(table
            .get(receiver.id())
            .expect("disconnected receiver")
            .peer_address()
            .is_none());
        table
            .send_to_bound_udp_socket(other_sender.id(), receiver_address, b"accepted")
            .expect("send after disconnect");
        assert_eq!(
            table
                .recv_datagram(receiver.id(), usize::MAX)
                .expect("receive after disconnect")
                .expect("disconnected datagram")
                .payload(),
            b"accepted"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn incomplete_datagram_ownership_does_not_discard_payload() {
        let table = SocketTable::new();
        let ledger = test_resource_ledger(8, 1);
        table
            .set_resource_ledger(Arc::clone(&ledger))
            .expect("install resource ledger");
        let sender = table
            .allocate(1, SocketSpec::udp())
            .expect("allocate sender");
        let receiver = table
            .allocate(2, SocketSpec::udp())
            .expect("allocate receiver");
        let sender_address = InetSocketAddress::new("127.0.0.1", 42001);
        let receiver_address = InetSocketAddress::new("127.0.0.1", 42002);
        table
            .bind_inet(sender.id(), sender_address)
            .expect("bind sender");
        table
            .bind_inet(receiver.id(), receiver_address.clone())
            .expect("bind receiver");
        table
            .send_to_bound_udp_socket(sender.id(), receiver_address, b"abc")
            .expect("queue datagram");

        // Simulate an internal ownership defect. Receiving must report it
        // without losing the guest-visible payload.
        let missing = lock_or_recover(&table.inner.state)
            .retained_resources
            .get_mut(&receiver.id())
            .expect("retained ownership")
            .udp_datagrams
            .pop_front()
            .expect("UDP datagram reservation");
        drop(missing);
        let error = table
            .recv_datagram(receiver.id(), usize::MAX)
            .expect_err("incomplete ownership must fail");
        assert_eq!(error.code(), "EIO");
        assert_eq!(table.snapshot().datagram_queue_len, 1);
        assert_eq!(table.snapshot().buffered_bytes, 3);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn retained_queue_admission_charges_the_process_parent_atomically() {
        let process = Arc::new(ResourceLedger::root(
            "process",
            [(
                ResourceClass::BufferedBytes,
                ResourceLimit::new(3, "runtime.resources.maxBufferedBytes"),
            )],
        ));
        let vm = Arc::new(ResourceLedger::child(
            "vm",
            [(
                ResourceClass::BufferedBytes,
                ResourceLimit::new(5, "limits.resources.maxSocketBufferedBytes"),
            )],
            Arc::clone(&process),
        ));
        let table = SocketTable::new();
        table
            .set_resource_ledger(Arc::clone(&vm))
            .expect("install child ledger");
        let writer = table
            .allocate(1, SocketSpec::tcp())
            .expect("allocate writer");
        let reader = table
            .allocate(2, SocketSpec::tcp())
            .expect("allocate reader");
        table
            .connect_pair(writer.id(), reader.id())
            .expect("connect stream pair");

        let error = table
            .write(writer.id(), b"1234")
            .expect_err("process aggregate must reject before queue mutation");
        assert_eq!(error.code(), "EAGAIN");
        assert_eq!(table.snapshot().buffered_bytes, 0);
        assert_eq!(usage(&process, ResourceClass::BufferedBytes).used, 0);
        assert_eq!(usage(&vm, ResourceClass::BufferedBytes).used, 0);

        table
            .write(writer.id(), b"123")
            .expect("write within aggregate limit");
        assert_eq!(usage(&process, ResourceClass::BufferedBytes).used, 3);
        assert_eq!(usage(&vm, ResourceClass::BufferedBytes).used, 3);
        table.read(reader.id(), 3).expect("drain retained bytes");
        assert_eq!(usage(&process, ResourceClass::BufferedBytes).used, 0);
        assert_eq!(usage(&vm, ResourceClass::BufferedBytes).used, 0);
    }
}
