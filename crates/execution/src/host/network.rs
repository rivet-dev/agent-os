use super::{BoundedBytes, BoundedString, BoundedUsize, BoundedVec, SignalSetValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketDomain {
    Inet4,
    Inet6,
    Unix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketKind {
    Stream,
    Datagram,
    SeqPacket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsAddressFamily {
    Any,
    Inet4,
    Inet6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedUdpFamily {
    Inet4,
    Inet6,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedUnixAddress {
    Path(BoundedString),
    AbstractHex(BoundedString),
    Autobind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedTcpEndpoint {
    pub host: Option<BoundedString>,
    pub port: Option<u16>,
    pub unix: Option<ManagedUnixAddress>,
    pub bound_server_id: Option<BoundedString>,
    pub local_address: Option<BoundedString>,
    pub local_port: Option<u16>,
    pub local_reservation: Option<BoundedString>,
    pub backlog: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketAddress {
    Inet { host: BoundedString, port: u16 },
    UnixPath(BoundedString),
    UnixAbstract(BoundedBytes),
    UnixAutobind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketShutdown {
    Read,
    Write,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketOptionName {
    Error,
    ReuseAddress,
    ReusePort,
    KeepAlive,
    NoDelay,
    Broadcast,
    ReceiveBuffer,
    SendBuffer,
    Linger,
    ReceiveTimeout,
    SendTimeout,
    Ipv6Only,
    MulticastTtl,
    MulticastLoop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketOptionValue {
    Bool(bool),
    Integer(i64),
    DurationMs(Option<u64>),
    Linger { enabled: bool, seconds: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollInterest {
    pub fd: u32,
    pub readable: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpHeader {
    pub name: BoundedString,
    pub value: BoundedString,
}

/// Raw Linux-compatible poll interest used by the AgentOS kernel ABI.
///
/// The bitset is deliberately preserved instead of projecting it to a smaller
/// readable/writable pair: the guest ABI also relies on error, hangup, and
/// invalid-descriptor reporting remaining stable across executor engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelPollInterest {
    pub fd: u32,
    pub events: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketValidationRequirement {
    Socket,
    Listening,
}

/// Canonical receive result shared by executor adapters.
///
/// `message_len` records the original datagram length before truncation;
/// `bytes` is the bounded payload copied to the guest. Stream EOF is explicit
/// so adapters do not infer it from engine-specific JSON shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkReceive {
    pub bytes: BoundedBytes,
    pub source: Option<SocketAddress>,
    pub message_len: u32,
    pub truncated: bool,
    pub eof: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum NetworkOperation {
    HttpRequest {
        url: BoundedString,
        method: BoundedString,
        headers: BoundedVec<HttpHeader>,
        body: BoundedBytes,
        max_response_bytes: BoundedUsize,
        max_header_bytes: BoundedUsize,
        max_body_bytes: BoundedUsize,
    },
    ManagedBindUnix {
        address: ManagedUnixAddress,
    },
    ManagedBindConnectedUnix {
        socket_id: BoundedString,
        address: ManagedUnixAddress,
    },
    ManagedReserveTcpPort {
        host: Option<BoundedString>,
        port: Option<u16>,
    },
    ManagedReleaseTcpPort {
        reservation_id: BoundedString,
    },
    ManagedConnect {
        endpoint: ManagedTcpEndpoint,
    },
    ManagedListen {
        endpoint: ManagedTcpEndpoint,
    },
    ManagedPoll {
        socket_id: BoundedString,
        wait_ms: u64,
    },
    ManagedWaitConnect {
        socket_id: BoundedString,
    },
    ManagedRead {
        socket_id: BoundedString,
        max_bytes: u64,
        peek: bool,
        wait_ms: u64,
    },
    ManagedWrite {
        socket_id: BoundedString,
        bytes: BoundedBytes,
    },
    ManagedDestroy {
        socket_id: BoundedString,
    },
    ManagedAccept {
        listener_id: BoundedString,
    },
    ManagedCloseListener {
        listener_id: BoundedString,
    },
    ManagedTlsUpgrade {
        socket_id: BoundedString,
        options_json: BoundedString,
    },
    ManagedUdpCreate {
        family: ManagedUdpFamily,
    },
    ManagedUdpBind {
        socket_id: BoundedString,
        host: Option<BoundedString>,
        port: u16,
    },
    ManagedUdpSend {
        socket_id: BoundedString,
        bytes: BoundedBytes,
        host: Option<BoundedString>,
        port: Option<u16>,
    },
    ManagedUdpPoll {
        socket_id: BoundedString,
        wait_ms: u64,
        /// Observe the next datagram without consuming it from the shared
        /// sidecar-owned open description.
        peek: bool,
        /// Maximum datagram bytes exposed to this receive operation. `None`
        /// preserves the full-datagram compatibility contract for adapters
        /// whose own reply lane already admits the complete payload.
        max_bytes: Option<BoundedUsize>,
    },
    ManagedUdpClose {
        socket_id: BoundedString,
    },
    ResolveDns {
        host: BoundedString,
        port: Option<u16>,
        family: DnsAddressFamily,
        max_results: BoundedUsize,
    },
    ResolveDnsRecord {
        host: BoundedString,
        record_type: BoundedString,
        raw: bool,
        max_results: BoundedUsize,
    },
    Socket {
        domain: SocketDomain,
        kind: SocketKind,
        nonblocking: bool,
        close_on_exec: bool,
    },
    SocketPair {
        kind: SocketKind,
        nonblocking: bool,
        close_on_exec: bool,
    },
    SendDescriptorRights {
        fd: u32,
        bytes: BoundedBytes,
        rights: BoundedVec<u32>,
        flags: u32,
    },
    ReceiveDescriptorRights {
        fd: u32,
        max_bytes: BoundedUsize,
        max_rights: BoundedUsize,
        close_on_exec: bool,
        peek: bool,
        dontwait: bool,
        waitall: bool,
    },
    Bind {
        fd: u32,
        address: SocketAddress,
    },
    Connect {
        fd: u32,
        address: SocketAddress,
        deadline_ms: Option<u64>,
    },
    Listen {
        fd: u32,
        backlog: u32,
    },
    Accept {
        fd: u32,
        nonblocking: bool,
        close_on_exec: bool,
        deadline_ms: Option<u64>,
    },
    Receive {
        fd: u32,
        max_bytes: BoundedUsize,
        flags: u32,
        deadline_ms: Option<u64>,
    },
    Validate {
        fd: u32,
        requirement: SocketValidationRequirement,
    },
    Send {
        fd: u32,
        bytes: BoundedBytes,
        flags: u32,
        address: Option<SocketAddress>,
        deadline_ms: Option<u64>,
    },
    Shutdown {
        fd: u32,
        how: SocketShutdown,
    },
    LocalAddress {
        fd: u32,
    },
    PeerAddress {
        fd: u32,
    },
    GetOption {
        fd: u32,
        name: SocketOptionName,
    },
    SetOption {
        fd: u32,
        name: SocketOptionName,
        value: SocketOptionValue,
    },
    Poll {
        interests: BoundedVec<PollInterest>,
        deadline_ms: Option<u64>,
    },
    KernelPoll {
        interests: BoundedVec<KernelPollInterest>,
        /// `None` is poll(2)'s indefinite wait; `Some(0)` is a probe.
        timeout_ms: Option<u64>,
    },
    /// One sidecar-owned POSIX poll over both kernel and managed socket fds.
    ///
    /// The optional signal mask is installed and restored by the process
    /// owner while the guest is parked. It is never represented by a guest
    /// token, which keeps ppoll's mask swap atomic with admission and signal
    /// interruption.
    PosixPoll {
        interests: BoundedVec<KernelPollInterest>,
        /// `None` is poll(2)'s indefinite wait; `Some(0)` is a probe.
        timeout_ms: Option<u64>,
        signal_mask: Option<SignalSetValue>,
    },
    TlsConnect {
        fd: u32,
        server_name: BoundedString,
        alpn: BoundedVec<BoundedBytes>,
        deadline_ms: Option<u64>,
    },
    TlsRead {
        session_id: u64,
        max_bytes: BoundedUsize,
        deadline_ms: Option<u64>,
    },
    TlsWrite {
        session_id: u64,
        bytes: BoundedBytes,
        deadline_ms: Option<u64>,
    },
    TlsClose {
        session_id: u64,
    },
}
