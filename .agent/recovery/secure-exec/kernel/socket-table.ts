/**
 * Virtual socket table.
 *
 * Manages kernel-level sockets: create, bind, listen, accept, connect,
 * send, recv, close, poll, per-process isolation, and resource limits.
 * Loopback connections are routed entirely in-kernel without touching
 * the host network stack.
 */

import { WaitQueue } from "./wait.js";
import { KernelError, SA_RESTART } from "./types.js";
import type { NetworkAccessRequest, PermissionCheck } from "./types.js";
import type { ProcessSignalState } from "./types.js";
import type {
	HostNetworkAdapter,
	HostSocket,
	HostListener,
	HostUdpSocket,
} from "./host-adapter.js";
import type { VirtualFileSystem } from "./vfs.js";

// ---------------------------------------------------------------------------
// Socket constants
// ---------------------------------------------------------------------------

export const AF_INET = 2;
export const AF_INET6 = 10;
export const AF_UNIX = 1;

export const SOCK_STREAM = 1;
export const SOCK_DGRAM = 2;

// Socket option levels
export const SOL_SOCKET = 1;
export const IPPROTO_TCP = 6;

// Socket options (SOL_SOCKET level)
export const SO_REUSEADDR = 2;
export const SO_KEEPALIVE = 9;
export const SO_RCVBUF = 8;
export const SO_SNDBUF = 7;

// TCP options (IPPROTO_TCP level)
export const TCP_NODELAY = 1;

// Send/recv flags
export const MSG_PEEK = 0x2;
export const MSG_DONTWAIT = 0x40;
export const MSG_NOSIGNAL = 0x4000;

// UDP limits
export const MAX_DATAGRAM_SIZE = 65535;
export const MAX_UDP_QUEUE_DEPTH = 128;
const EPHEMERAL_PORT_MIN = 49152;
const EPHEMERAL_PORT_MAX = 65535;

// File type for socket files in VFS
export const S_IFSOCK = 0o140000;

// ---------------------------------------------------------------------------
// Address types
// ---------------------------------------------------------------------------

export type InetAddr = { host: string; port: number };
export type UnixAddr = { path: string };
export type SockAddr = InetAddr | UnixAddr;

export function isInetAddr(addr: SockAddr): addr is InetAddr {
	return "host" in addr;
}

export function isUnixAddr(addr: SockAddr): addr is UnixAddr {
	return "path" in addr;
}

// ---------------------------------------------------------------------------
// UDP datagram (preserves message boundaries with source address)
// ---------------------------------------------------------------------------

export interface UdpDatagram {
	data: Uint8Array;
	srcAddr: SockAddr;
}

// ---------------------------------------------------------------------------
// Address key helper
// ---------------------------------------------------------------------------

/** Canonical string key for a socket address ("host:port" or unix path). */
export function addrKey(addr: SockAddr): string {
	if (isInetAddr(addr)) return `${addr.host}:${addr.port}`;
	return addr.path;
}

/** Canonical string key for a socket option ("level:optname"). */
export function optKey(level: number, optname: number): string {
	return `${level}:${optname}`;
}

// ---------------------------------------------------------------------------
// Socket state machine
// ---------------------------------------------------------------------------

export type SocketState =
	| "created"
	| "bound"
	| "listening"
	| "connecting"
	| "connected"
	| "read-closed"
	| "write-closed"
	| "closed";

// ---------------------------------------------------------------------------
// KernelSocket
// ---------------------------------------------------------------------------

export interface KernelSocket {
	readonly id: number;
	readonly domain: number;
	readonly type: number;
	readonly protocol: number;
	state: SocketState;
	nonBlocking: boolean;
	localAddr?: SockAddr;
	remoteAddr?: SockAddr;
	options: Map<string, number>;
	readonly pid: number;
	readBuffer: Uint8Array[];
	readWaiters: WaitQueue;
	backlog: number[];
	backlogLimit: number;
	acceptWaiters: WaitQueue;
	/** Peer socket ID for connected loopback/socketpair sockets. */
	peerId?: number;
	/** True when the peer has shut down its write side (half-close EOF). */
	peerWriteClosed?: boolean;
	/** True when connected via host adapter (external network). */
	external?: boolean;
	/** Host socket for external connections (data relay). */
	hostSocket?: HostSocket;
	/** Host listener for external-facing server sockets. */
	hostListener?: HostListener;
	/** Queued datagrams for UDP sockets (preserves message boundaries). */
	datagramQueue: UdpDatagram[];
	/** Host UDP socket for external datagram routing. */
	hostUdpSocket?: HostUdpSocket;
	/** Tracks whether bind() was originally requested with port 0. */
	requestedEphemeralPort?: boolean;
}

// ---------------------------------------------------------------------------
// SocketTable
// ---------------------------------------------------------------------------

const DEFAULT_MAX_SOCKETS = 1024;

type BlockingSocketWait = {
	block: true;
	pid: number;
};

export class SocketTable {
	private sockets: Map<number, KernelSocket> = new Map();
	private nextSocketId = 1;
	private readonly maxSockets: number;
	private readonly networkCheck?: PermissionCheck<NetworkAccessRequest>;
	private readonly hostAdapter?: HostNetworkAdapter;
	private readonly vfs?: VirtualFileSystem;
	private readonly getSignalState?: (pid: number) => ProcessSignalState;
	private readonly processExists?: (pid: number) => boolean;

	/** Bound/listening address → socket ID. Used for EADDRINUSE and TCP routing. */
	private listeners: Map<string, number> = new Map();

	/** Bound UDP address → socket ID. Separate from TCP listeners. */
	private udpBindings: Map<string, number> = new Map();

	constructor(options?: {
		maxSockets?: number;
		networkCheck?: PermissionCheck<NetworkAccessRequest>;
		hostAdapter?: HostNetworkAdapter;
		vfs?: VirtualFileSystem;
		getSignalState?: (pid: number) => ProcessSignalState;
		processExists?: (pid: number) => boolean;
	}) {
		this.maxSockets = options?.maxSockets ?? DEFAULT_MAX_SOCKETS;
		this.networkCheck = options?.networkCheck;
		this.hostAdapter = options?.hostAdapter;
		this.vfs = options?.vfs;
		this.getSignalState = options?.getSignalState;
		this.processExists = options?.processExists;
	}

	hasHostNetworkAdapter(): boolean {
		return this.hostAdapter !== undefined;
	}

	/**
	 * Create a new socket owned by the given process.
	 * Returns the kernel socket ID.
	 */
	create(domain: number, type: number, protocol: number, pid: number): number {
		if (this.sockets.size >= this.maxSockets) {
			throw new KernelError("EMFILE", "too many open sockets");
		}
		if (this.processExists && !this.processExists(pid)) {
			throw new KernelError(
				"ESRCH",
				`cannot create socket for unknown pid ${pid}`,
			);
		}

		const id = this.nextSocketId++;
		const socket: KernelSocket = {
			id,
			domain,
			type,
			protocol,
			state: "created",
			nonBlocking: false,
			options: new Map(),
			pid,
			readBuffer: [],
			readWaiters: new WaitQueue(),
			backlog: [],
			backlogLimit: 0,
			acceptWaiters: new WaitQueue(),
			datagramQueue: [],
		};

		this.sockets.set(id, socket);
		return id;
	}

	/**
	 * Get a socket by ID. Returns null if not found.
	 */
	get(socketId: number): KernelSocket | null {
		return this.sockets.get(socketId) ?? null;
	}

	// -------------------------------------------------------------------
	// Network permission check
	// -------------------------------------------------------------------

	/**
	 * Check network permission for an operation. Throws EACCES if the
	 * configured policy denies the request or if no policy is set
	 * (deny-by-default). Loopback callers should skip this method.
	 */
	checkNetworkPermission(
		op: NetworkAccessRequest["op"],
		addr?: SockAddr,
	): void {
		const request: NetworkAccessRequest = { op };
		if (addr && isInetAddr(addr)) {
			request.hostname = addr.host;
		}

		if (!this.networkCheck) {
			throw new KernelError(
				"EACCES",
				`network ${op} denied (no permission policy)`,
			);
		}

		const decision = this.networkCheck(request);
		if (!decision?.allow) {
			const reason = decision?.reason ? `: ${decision.reason}` : "";
			throw new KernelError("EACCES", `network ${op} denied${reason}`);
		}
	}

	// -------------------------------------------------------------------
	// Bind / Listen / Accept
	// -------------------------------------------------------------------

	/**
	 * Bind a socket to an address. Transitions to 'bound' and registers
	 * the address in the listeners map for port reservation.
	 *
	 * For Unix domain sockets (UnixAddr), creates a socket file in the
	 * VFS if one is configured.
	 */
	async bind(
		socketId: number,
		addr: SockAddr,
		options?: { mode?: number },
	): Promise<void> {
		const socket = this.requireSocket(socketId);
		if (socket.state !== "created") {
			throw new KernelError(
				"EINVAL",
				"socket must be in created state to bind",
			);
		}
		const boundAddr = this.assignEphemeralPort(addr, socket);

		// Unix domain sockets: check VFS for existing path
		if (isUnixAddr(boundAddr) && this.vfs) {
			if (await this.vfs.exists(boundAddr.path)) {
				throw new KernelError(
					"EADDRINUSE",
					`address already in use: ${boundAddr.path}`,
				);
			}
		}

		// UDP uses a separate binding map from TCP
		if (socket.type === SOCK_DGRAM) {
			if (this.isUdpAddrInUse(boundAddr, socket)) {
				throw new KernelError(
					"EADDRINUSE",
					`address already in use: ${addrKey(boundAddr)}`,
				);
			}
			socket.localAddr = boundAddr;
			socket.state = "bound";
			this.udpBindings.set(addrKey(boundAddr), socketId);
			// Create socket file in VFS for Unix dgram sockets
			if (isUnixAddr(boundAddr) && this.vfs) {
				await this.createSocketFile(boundAddr.path, options?.mode);
			}
			return;
		}

		if (this.isAddrInUse(boundAddr, socket)) {
			throw new KernelError(
				"EADDRINUSE",
				`address already in use: ${addrKey(boundAddr)}`,
			);
		}

		socket.localAddr = boundAddr;
		socket.state = "bound";
		this.listeners.set(addrKey(boundAddr), socketId);

		// Create socket file in VFS for Unix stream sockets
		if (isUnixAddr(boundAddr) && this.vfs) {
			await this.createSocketFile(boundAddr.path, options?.mode);
		}
	}

	/**
	 * Mark a bound socket as listening. The socket must already be bound.
	 * Checks network permission before transitioning.
	 *
	 * When `external` is true and a host adapter is available, creates a
	 * real TCP listener via `hostAdapter.tcpListen()` and starts an accept
	 * pump that feeds incoming connections into the kernel backlog.
	 */
	async listen(
		socketId: number,
		backlogSize: number = 128,
		options?: { external?: boolean },
	): Promise<void> {
		const socket = this.requireSocket(socketId);
		if (socket.state !== "bound") {
			throw new KernelError("EINVAL", "socket must be bound before listen");
		}
		socket.backlogLimit = Math.max(0, backlogSize);

		// AF_UNIX listeners stay entirely in-kernel, so host-network policy
		// only applies to inet listeners.
		if (socket.localAddr && isInetAddr(socket.localAddr)) {
			this.checkNetworkPermission("listen", socket.localAddr);
		}

		// External listen — delegate to host adapter
		if (
			options?.external &&
			this.hostAdapter &&
			socket.localAddr &&
			isInetAddr(socket.localAddr)
		) {
			const hostListener = await this.hostAdapter.tcpListen(
				socket.localAddr.host,
				socket.requestedEphemeralPort ? 0 : socket.localAddr.port,
			);

			socket.hostListener = hostListener;
			socket.external = true;

			// Update port for ephemeral (port 0) bindings
			if (socket.requestedEphemeralPort || socket.localAddr.port === 0) {
				const oldKey = addrKey(socket.localAddr);
				socket.localAddr = {
					host: socket.localAddr.host,
					port: hostListener.port,
				};
				// Re-register in listeners map with actual port
				this.listeners.delete(oldKey);
				this.listeners.set(addrKey(socket.localAddr), socketId);
			}

			socket.state = "listening";
			this.startAcceptPump(socket);
			return;
		}

		socket.state = "listening";
	}

	/**
	 * Accept a pending connection from a listening socket's backlog.
	 * Returns the connected socket ID, or null if backlog is empty (EAGAIN).
	 */
	accept(socketId: number): number | null;
	accept(socketId: number, options: BlockingSocketWait): Promise<number | null>;
	accept(
		socketId: number,
		options?: BlockingSocketWait,
	): number | null | Promise<number | null> {
		const socket = this.requireSocket(socketId);
		if (socket.state !== "listening") {
			throw new KernelError("EINVAL", "socket is not listening");
		}
		if (socket.backlog.length === 0 && socket.nonBlocking) {
			throw new KernelError(
				"EAGAIN",
				"no pending connections on non-blocking socket",
			);
		}
		if (!options?.block) {
			const connId = socket.backlog.shift();
			return connId ?? null;
		}
		return this.acceptBlocking(socket, options.pid);
	}

	/**
	 * Find a listening socket that matches the given address.
	 * Checks exact match first, then wildcard (0.0.0.0 / ::).
	 */
	findListener(addr: SockAddr): KernelSocket | null {
		if (isInetAddr(addr)) {
			// Exact match
			const sock = this.getListeningSocket(`${addr.host}:${addr.port}`);
			if (sock) return sock;
			// Wildcard IPv4
			const wild4 = this.getListeningSocket(`0.0.0.0:${addr.port}`);
			if (wild4) return wild4;
			// Wildcard IPv6
			const wild6 = this.getListeningSocket(`:::${addr.port}`);
			if (wild6) return wild6;
			return null;
		}
		return this.getListeningSocket(addr.path) ?? null;
	}

	// -------------------------------------------------------------------
	// Shutdown (half-close)
	// -------------------------------------------------------------------

	/**
	 * Shut down part of a full-duplex connection.
	 * - 'write': peer recv() gets EOF, local send() returns EPIPE
	 * - 'read': local recv() returns EOF immediately
	 * - 'both': equivalent to shutdown('read') + shutdown('write')
	 */
	shutdown(socketId: number, how: "read" | "write" | "both"): void {
		const socket = this.requireSocket(socketId);
		if (
			socket.state !== "connected" &&
			socket.state !== "write-closed" &&
			socket.state !== "read-closed"
		) {
			throw new KernelError("ENOTCONN", "socket is not connected");
		}

		// Propagate half-close/full-close semantics to real host sockets so
		// external TCP clients observe EOF instead of hanging on response reads.
		socket.hostSocket?.shutdown(how);

		if (how === "both") {
			this.shutdownWrite(socket);
			this.shutdownRead(socket);
			socket.state = "closed";
			return;
		}

		if (how === "write") {
			this.shutdownWrite(socket);
			if (socket.state === "read-closed") {
				socket.state = "closed";
			} else {
				socket.state = "write-closed";
			}
			return;
		}

		// how === 'read'
		this.shutdownRead(socket);
		if (socket.state === "write-closed") {
			socket.state = "closed";
		} else {
			socket.state = "read-closed";
		}
	}

	/** Signal EOF to the peer by waking their readWaiters. */
	private shutdownWrite(socket: KernelSocket): void {
		if (socket.peerId !== undefined) {
			const peer = this.sockets.get(socket.peerId);
			if (peer) {
				peer.peerWriteClosed = true;
				peer.readWaiters.wakeAll();
			}
		}
	}

	/** Discard unread data and mark the read side as closed. */
	private shutdownRead(socket: KernelSocket): void {
		socket.readBuffer.length = 0;
		socket.readWaiters.wakeAll();
	}

	// -------------------------------------------------------------------
	// Socketpair
	// -------------------------------------------------------------------

	/**
	 * Create a pair of connected sockets atomically (for IPC).
	 * Returns [socketId1, socketId2]. Both are pre-connected with
	 * peerId linking, so data written to one appears in the other's
	 * readBuffer via send/recv.
	 */
	socketpair(
		domain: number,
		type: number,
		protocol: number,
		pid: number,
	): [number, number] {
		const id1 = this.create(domain, type, protocol, pid);
		const id2 = this.create(domain, type, protocol, pid);

		const sock1 = this.get(id1)!;
		const sock2 = this.get(id2)!;

		sock1.peerId = id2;
		sock2.peerId = id1;
		sock1.state = "connected";
		sock2.state = "connected";

		return [id1, id2];
	}

	// -------------------------------------------------------------------
	// Socket options
	// -------------------------------------------------------------------

	/**
	 * Set a socket option. Stores the value keyed by "level:optname".
	 */
	setsockopt(
		socketId: number,
		level: number,
		optname: number,
		optval: number,
	): void {
		const socket = this.requireSocket(socketId);
		socket.options.set(optKey(level, optname), optval);
		this.applySocketOptionToHostSocket(socket, level, optname, optval);
	}

	/** Toggle non-blocking behavior for an existing socket. */
	setNonBlocking(socketId: number, nonBlocking: boolean): void {
		const socket = this.requireSocket(socketId);
		socket.nonBlocking = nonBlocking;
	}

	/**
	 * Get a socket option. Returns the value, or undefined if not set.
	 */
	getsockopt(
		socketId: number,
		level: number,
		optname: number,
	): number | undefined {
		const socket = this.requireSocket(socketId);
		return socket.options.get(optKey(level, optname));
	}

	/** Get the bound/local address for a socket. */
	getLocalAddr(socketId: number): SockAddr {
		const socket = this.requireSocket(socketId);
		if (!socket.localAddr) {
			throw new KernelError("EINVAL", "socket has no local address");
		}
		return socket.localAddr;
	}

	/** Get the connected peer address for a socket. */
	getRemoteAddr(socketId: number): SockAddr {
		const socket = this.requireSocket(socketId);
		if (!socket.remoteAddr) {
			throw new KernelError("ENOTCONN", "socket is not connected");
		}
		return socket.remoteAddr;
	}

	// -------------------------------------------------------------------
	// Connect (loopback routing)
	// -------------------------------------------------------------------

	/**
	 * Connect a socket to a remote address. For loopback (addr matches a
	 * kernel listener), creates a paired server-side socket and queues it
	 * in the listener's backlog — loopback is always allowed regardless of
	 * permission policy. External addresses are checked against the network
	 * permission policy and routed through the host adapter.
	 */
	async connect(socketId: number, addr: SockAddr): Promise<void> {
		const socket = this.requireSocket(socketId);
		if (socket.state !== "created" && socket.state !== "bound") {
			throw new KernelError(
				"EINVAL",
				"socket must be in created or bound state to connect",
			);
		}

		// Mirror POSIX auto-bind behavior so connected client sockets always
		// expose a concrete local address/port to both peers.
		if (!socket.localAddr && isInetAddr(addr)) {
			socket.localAddr = this.assignEphemeralPort(
				{
					host: addr.host.includes(":") ? "::1" : "127.0.0.1",
					port: 0,
				},
				socket,
			);
		}

		// Unix domain sockets: check VFS for socket file existence
		if (isUnixAddr(addr) && this.vfs) {
			if (!(await this.vfs.exists(addr.path))) {
				throw new KernelError(
					"ECONNREFUSED",
					`connection refused: ${addr.path}`,
				);
			}
		}

		const listener = this.findListener(addr);

		if (!listener) {
			if (isUnixAddr(addr)) {
				throw new KernelError(
					"ECONNREFUSED",
					`connection refused: ${addr.path}`,
				);
			}

			// Check external connections through the deny-by-default network policy.
			this.checkNetworkPermission("connect", addr);

			// Route through host adapter if available
			if (this.hostAdapter && isInetAddr(addr)) {
				if (socket.nonBlocking) {
					socket.state = "connecting";
					socket.remoteAddr = addr;
					this.startExternalConnect(socket, addr);
					throw new KernelError(
						"EINPROGRESS",
						`connection in progress: ${addrKey(addr)}`,
					);
				}

				const hostSocket = await this.hostAdapter.tcpConnect(
					addr.host,
					addr.port,
				);
				socket.state = "connected";
				socket.external = true;
				socket.remoteAddr = addr;
				socket.hostSocket = hostSocket;
				this.applySocketOptionsToHostSocket(socket);
				this.startReadPump(socket);
				return;
			}

			throw new KernelError(
				"ECONNREFUSED",
				`connection refused: ${addrKey(addr)}`,
			);
		}

		// Loopback — always allowed, no permission check
		if (listener.backlog.length >= listener.backlogLimit) {
			throw new KernelError(
				"ECONNREFUSED",
				`connection refused: backlog full for ${addrKey(addr)}`,
			);
		}

		// Create server-side socket paired with the client
		const serverSockId = this.create(
			listener.domain,
			listener.type,
			listener.protocol,
			listener.pid,
		);
		const serverSock = this.get(serverSockId)!;

		// Set addresses
		socket.remoteAddr = addr;
		serverSock.localAddr = listener.localAddr;
		serverSock.remoteAddr = socket.localAddr;

		// Link peers
		socket.peerId = serverSockId;
		serverSock.peerId = socketId;

		// Transition both to connected
		socket.state = "connected";
		serverSock.state = "connected";

		// Queue server socket in listener's backlog
		listener.backlog.push(serverSockId);
		listener.acceptWaiters.wakeOne();
	}

	// -------------------------------------------------------------------
	// Send / Recv
	// -------------------------------------------------------------------

	/**
	 * Send data to the connected peer. Writes to the peer's readBuffer
	 * and wakes one pending reader. Returns bytes written.
	 *
	 * Flags: MSG_NOSIGNAL suppresses SIGPIPE — returns EPIPE error
	 * instead of raising SIGPIPE on a broken connection.
	 *
	 * For external sockets, checks network permission before sending.
	 */
	send(socketId: number, data: Uint8Array, flags: number = 0): number {
		const socket = this.requireSocket(socketId);
		const nosignal = (flags & MSG_NOSIGNAL) !== 0;

		if (socket.state === "write-closed" || socket.state === "closed") {
			throw new KernelError(
				"EPIPE",
				nosignal
					? "broken pipe (MSG_NOSIGNAL)"
					: "broken pipe: write side shut down",
			);
		}
		if (socket.state !== "connected" && socket.state !== "read-closed") {
			throw new KernelError("ENOTCONN", "socket is not connected");
		}

		// Re-check outbound external writes so pre-existing host sockets still
		// honor deny-by-default network policy.
		if (socket.external) {
			this.checkNetworkPermission("connect", socket.remoteAddr);
		}

		// External socket: write to host socket
		if (socket.external && socket.hostSocket) {
			socket.hostSocket.write(new Uint8Array(data)).catch(() => {
				socket.state = "closed";
				socket.readWaiters.wakeAll();
			});
			return data.length;
		}

		if (socket.peerId === undefined) {
			throw new KernelError(
				"EPIPE",
				nosignal ? "broken pipe (MSG_NOSIGNAL)" : "broken pipe: peer closed",
			);
		}

		const peer = this.sockets.get(socket.peerId);
		if (!peer) {
			socket.peerId = undefined;
			throw new KernelError(
				"EPIPE",
				nosignal ? "broken pipe (MSG_NOSIGNAL)" : "broken pipe: peer closed",
			);
		}

		// Enforce SO_RCVBUF on the peer's receive buffer
		const rcvBuf = peer.options.get(optKey(SOL_SOCKET, SO_RCVBUF));
		if (rcvBuf !== undefined) {
			let currentSize = 0;
			for (const chunk of peer.readBuffer) currentSize += chunk.length;
			if (currentSize >= rcvBuf) {
				throw new KernelError("EAGAIN", "peer receive buffer full");
			}
		}

		// Copy data into peer's read buffer
		peer.readBuffer.push(new Uint8Array(data));
		peer.readWaiters.wakeOne();

		return data.length;
	}

	/**
	 * Receive data from the socket's readBuffer. Returns null if no data
	 * is available and the socket is non-blocking, or if the peer has
	 * closed (EOF).
	 *
	 * Flags:
	 * - MSG_PEEK: read data without consuming it from the buffer
	 * - MSG_DONTWAIT: return EAGAIN if no data (even on blocking socket)
	 */
	recv(socketId: number, maxBytes: number, flags?: number): Uint8Array | null;
	recv(
		socketId: number,
		maxBytes: number,
		flags: number,
		options: BlockingSocketWait,
	): Promise<Uint8Array | null>;
	recv(
		socketId: number,
		maxBytes: number,
		flags: number = 0,
		options?: BlockingSocketWait,
	): Uint8Array | null | Promise<Uint8Array | null> {
		const socket = this.requireSocket(socketId);
		const peek = (flags & MSG_PEEK) !== 0;
		const dontwait = (flags & MSG_DONTWAIT) !== 0;

		// read-closed or closed → immediate EOF
		if (socket.state === "read-closed" || socket.state === "closed") {
			return null;
		}
		if (socket.state !== "connected" && socket.state !== "write-closed") {
			throw new KernelError("ENOTCONN", "socket is not connected");
		}

		if (socket.readBuffer.length > 0) {
			if (peek) {
				return this.peekFromBuffer(socket, maxBytes);
			}
			return this.consumeFromBuffer(socket, maxBytes);
		}

		// Buffer empty — check for EOF (peer gone or peer shut down write)
		if (
			socket.peerId === undefined ||
			!this.sockets.has(socket.peerId) ||
			socket.peerWriteClosed
		) {
			return null;
		}

		// No data available
		if (socket.nonBlocking || dontwait) {
			throw new KernelError(
				"EAGAIN",
				socket.nonBlocking
					? "no data available on non-blocking socket"
					: "no data available (MSG_DONTWAIT)",
			);
		}
		if (options?.block) {
			return this.recvBlocking(socket, maxBytes, flags, options.pid);
		}
		return null;
	}

	// -------------------------------------------------------------------
	// UDP: sendTo / recvFrom
	// -------------------------------------------------------------------

	/**
	 * Send a datagram to a specific address (UDP only).
	 * For loopback, delivers to the kernel-bound UDP socket. For external
	 * addresses, routes through the host adapter (fire-and-forget). Sends
	 * to unbound ports are silently dropped (UDP semantics).
	 *
	 * Returns bytes "sent" (always data.length for UDP — drops are silent).
	 */
	sendTo(
		socketId: number,
		data: Uint8Array,
		flags: number,
		destAddr: SockAddr,
	): number {
		const socket = this.requireSocket(socketId);
		if (socket.type !== SOCK_DGRAM) {
			throw new KernelError("EINVAL", "sendTo requires a datagram socket");
		}
		if (data.length > MAX_DATAGRAM_SIZE) {
			throw new KernelError("EMSGSIZE", "datagram too large (max 65535 bytes)");
		}

		// Loopback routing — find a kernel-bound UDP socket at destAddr
		const target = this.findBoundUdp(destAddr);
		if (target) {
			if (target.datagramQueue.length >= MAX_UDP_QUEUE_DEPTH) {
				return data.length; // Silently drop
			}
			const srcAddr: SockAddr = this.getUdpSourceAddr(socket, destAddr);
			target.datagramQueue.push({ data: new Uint8Array(data), srcAddr });
			target.readWaiters.wakeOne();
			return data.length;
		}

		// External routing via host adapter
		if (socket.hostUdpSocket && this.hostAdapter && isInetAddr(destAddr)) {
			this.checkNetworkPermission("connect", destAddr);
			this.hostAdapter
				.udpSend(
					socket.hostUdpSocket,
					new Uint8Array(data),
					destAddr.host,
					destAddr.port,
				)
				.catch(() => {});
			return data.length;
		}

		// No loopback target, no host adapter — silently drop (UDP semantics)
		return data.length;
	}

	private getUdpSourceAddr(socket: KernelSocket, destAddr: SockAddr): SockAddr {
		if (!socket.localAddr) {
			return isInetAddr(destAddr)
				? {
						host: destAddr.host.includes(":") ? "::1" : "127.0.0.1",
						port: 0,
					}
				: { path: destAddr.path };
		}
		if (
			isInetAddr(socket.localAddr) &&
			isInetAddr(destAddr) &&
			(socket.localAddr.host === "0.0.0.0" || socket.localAddr.host === "::")
		) {
			return {
				host: destAddr.host,
				port: socket.localAddr.port,
			};
		}
		return socket.localAddr;
	}

	/**
	 * Receive a datagram from a UDP socket. Returns the datagram and the
	 * source address, or null if no datagram is queued.
	 *
	 * Message boundaries are preserved: each sendTo produces exactly one
	 * recvFrom result. If the datagram exceeds maxBytes, excess is
	 * discarded (UDP truncation semantics).
	 *
	 * Flags: MSG_PEEK reads without consuming, MSG_DONTWAIT throws EAGAIN.
	 */
	recvFrom(
		socketId: number,
		maxBytes: number,
		flags: number = 0,
	): { data: Uint8Array; srcAddr: SockAddr } | null {
		const socket = this.requireSocket(socketId);
		if (socket.type !== SOCK_DGRAM) {
			throw new KernelError("EINVAL", "recvFrom requires a datagram socket");
		}

		const peek = (flags & MSG_PEEK) !== 0;
		const dontwait = (flags & MSG_DONTWAIT) !== 0;

		if (socket.datagramQueue.length > 0) {
			if (peek) {
				const dgram = socket.datagramQueue[0];
				const data =
					dgram.data.length <= maxBytes
						? new Uint8Array(dgram.data)
						: new Uint8Array(dgram.data.subarray(0, maxBytes));
				return { data, srcAddr: dgram.srcAddr };
			}
			const dgram = socket.datagramQueue.shift()!;
			const data =
				dgram.data.length <= maxBytes
					? dgram.data
					: dgram.data.subarray(0, maxBytes);
			return { data, srcAddr: dgram.srcAddr };
		}

		if (dontwait) {
			throw new KernelError("EAGAIN", "no datagram available (MSG_DONTWAIT)");
		}

		return null;
	}

	/**
	 * Set up external UDP routing for a bound datagram socket.
	 * Creates a host UDP socket via the host adapter and starts a recv
	 * pump that feeds incoming datagrams into the kernel datagramQueue.
	 */
	async bindExternalUdp(socketId: number): Promise<void> {
		const socket = this.requireSocket(socketId);
		if (socket.type !== SOCK_DGRAM) {
			throw new KernelError(
				"EINVAL",
				"bindExternalUdp requires a datagram socket",
			);
		}
		if (socket.state !== "bound") {
			throw new KernelError(
				"EINVAL",
				"socket must be bound before external UDP bind",
			);
		}
		if (
			!this.hostAdapter ||
			!socket.localAddr ||
			!isInetAddr(socket.localAddr)
		) {
			throw new KernelError("EINVAL", "host adapter and inet address required");
		}

		this.checkNetworkPermission("listen", socket.localAddr);

		const hostUdpSocket = await this.hostAdapter.udpBind(
			socket.localAddr.host,
			socket.localAddr.port,
		);
		socket.hostUdpSocket = hostUdpSocket;
		socket.external = true;
		this.startUdpRecvPump(socket);
	}

	// -------------------------------------------------------------------
	// Close / Cleanup
	// -------------------------------------------------------------------

	/**
	 * Close a socket. The caller must own the socket (per-process isolation).
	 * Wakes all pending waiters and frees resources.
	 */
	close(socketId: number, pid: number): void {
		const socket = this.requireSocket(socketId);
		if (socket.pid !== pid) {
			throw new KernelError(
				"EBADF",
				`socket ${socketId} not owned by pid ${pid}`,
			);
		}
		this.destroySocket(socket);
	}

	/**
	 * Poll a socket for readability, writability, and hangup.
	 */
	poll(socketId: number): {
		readable: boolean;
		writable: boolean;
		hangup: boolean;
	} {
		const socket = this.requireSocket(socketId);

		const closed = socket.state === "closed";
		const readClosed = socket.state === "read-closed";
		const writeClosed = socket.state === "write-closed";
		const pendingAccept =
			socket.state === "listening" && socket.backlog.length > 0;

		// UDP: readable when datagramQueue has data
		const readable =
			socket.type === SOCK_DGRAM
				? socket.datagramQueue.length > 0 || closed
				: socket.readBuffer.length > 0 || pendingAccept || closed || readClosed;

		const writable =
			socket.state === "connected" ||
			socket.state === "created" ||
			socket.state === "read-closed" ||
			(socket.type === SOCK_DGRAM && socket.state === "bound");
		const hangup = closed || readClosed || writeClosed;

		return { readable, writable, hangup };
	}

	/**
	 * Clean up all sockets owned by a process (called on process exit).
	 */
	closeAllForProcess(pid: number): void {
		for (const socket of this.sockets.values()) {
			if (socket.pid === pid) {
				this.destroySocket(socket);
			}
		}
	}

	/**
	 * Clean up all sockets (called on kernel dispose).
	 */
	disposeAll(): void {
		for (const socket of this.sockets.values()) {
			socket.readWaiters.wakeAll();
			socket.acceptWaiters.wakeAll();
			if (socket.hostSocket) {
				socket.hostSocket.close().catch(() => {});
			}
			if (socket.hostListener) {
				socket.hostListener.close().catch(() => {});
			}
			if (socket.hostUdpSocket) {
				socket.hostUdpSocket.close().catch(() => {});
			}
		}
		this.sockets.clear();
		this.listeners.clear();
		this.udpBindings.clear();
	}

	/** Number of open sockets. */
	get size(): number {
		return this.sockets.size;
	}

	// -----------------------------------------------------------------------
	// Internal helpers
	// -----------------------------------------------------------------------

	/** Create a socket file in the VFS with S_IFSOCK mode. */
	private async createSocketFile(
		path: string,
		mode: number = 0o755,
	): Promise<void> {
		if (!this.vfs) return;
		await this.vfs.writeFile(path, new Uint8Array(0));
		await this.vfs.chmod(path, S_IFSOCK | (mode & 0o777));
	}

	private requireSocket(socketId: number): KernelSocket {
		const socket = this.sockets.get(socketId);
		if (!socket) {
			throw new KernelError("EBADF", `socket ${socketId} not found`);
		}
		return socket;
	}

	/** Wait for an inbound connection, restarting when SA_RESTART applies. */
	private async acceptBlocking(
		socket: KernelSocket,
		pid: number,
	): Promise<number | null> {
		while (true) {
			const connId = socket.backlog.shift();
			if (connId !== undefined) return connId;
			await this.waitForSocketWake(socket.acceptWaiters, pid, "accept");
			if (socket.state !== "listening") {
				throw new KernelError("EINVAL", "socket is not listening");
			}
		}
	}

	private destroySocket(socket: KernelSocket): void {
		// Tear down queued-but-unaccepted connections with the listener so they
		// cannot leak detached server-side sockets after the listening endpoint closes.
		for (const pendingId of [...socket.backlog]) {
			const pending = this.sockets.get(pendingId);
			if (pending) {
				this.destroySocket(pending);
			}
		}
		socket.backlog.length = 0;

		// Propagate EOF to peer: clear peer link and wake readers
		if (socket.peerId !== undefined) {
			const peer = this.sockets.get(socket.peerId);
			if (peer) {
				peer.peerId = undefined;
				peer.readWaiters.wakeAll();
			}
		}

		// Close host socket for external connections
		if (socket.hostSocket) {
			socket.hostSocket.close().catch(() => {});
			socket.hostSocket = undefined;
		}

		// Close host listener for external-facing server sockets
		if (socket.hostListener) {
			socket.hostListener.close().catch(() => {});
			socket.hostListener = undefined;
		}

		// Close host UDP socket for external datagram sockets
		if (socket.hostUdpSocket) {
			socket.hostUdpSocket.close().catch(() => {});
			socket.hostUdpSocket = undefined;
		}

		// Free listener/binding registration if this socket was bound
		if (socket.localAddr) {
			const key = addrKey(socket.localAddr);
			if (this.listeners.get(key) === socket.id) {
				this.listeners.delete(key);
			}
			if (this.udpBindings.get(key) === socket.id) {
				this.udpBindings.delete(key);
			}
		}
		socket.state = "closed";
		socket.readBuffer.length = 0;
		socket.datagramQueue.length = 0;
		socket.readWaiters.wakeAll();
		socket.acceptWaiters.wakeAll();
		this.sockets.delete(socket.id);
	}

	/** Background pump: reads from host socket and feeds kernel readBuffer. */
	private startReadPump(socket: KernelSocket): void {
		if (!socket.hostSocket) return;
		const hostSocket = socket.hostSocket;
		const pump = async () => {
			try {
				while (socket.state !== "closed" && socket.hostSocket === hostSocket) {
					const data = await hostSocket.read();
					if (data === null) {
						// EOF from host
						socket.peerWriteClosed = true;
						socket.readWaiters.wakeAll();
						break;
					}
					socket.readBuffer.push(data);
					socket.readWaiters.wakeOne();
				}
			} catch {
				// Connection error — mark as closed
				if (socket.state !== "closed") {
					socket.peerWriteClosed = true;
					socket.readWaiters.wakeAll();
				}
			}
		};
		pump();
	}

	/** Complete a non-blocking external connect in the background. */
	private startExternalConnect(socket: KernelSocket, addr: InetAddr): void {
		if (!this.hostAdapter) return;

		this.hostAdapter
			.tcpConnect(addr.host, addr.port)
			.then((hostSocket) => {
				const current = this.sockets.get(socket.id);
				if (!current || current !== socket || current.state === "closed") {
					hostSocket.close().catch(() => {});
					return;
				}

				current.state = "connected";
				current.external = true;
				current.remoteAddr = addr;
				current.hostSocket = hostSocket;
				this.applySocketOptionsToHostSocket(current);
				this.startReadPump(current);
			})
			.catch(() => {
				const current = this.sockets.get(socket.id);
				if (!current || current !== socket || current.state === "closed") {
					return;
				}

				current.state = "created";
				current.remoteAddr = undefined;
				current.external = false;
				current.hostSocket = undefined;
				current.readWaiters.wakeAll();
			});
	}

	/** Background pump: accepts incoming connections from host listener and feeds kernel backlog. */
	private startAcceptPump(socket: KernelSocket): void {
		if (!socket.hostListener) return;
		const hostListener = socket.hostListener;
		const pump = async () => {
			try {
				while (
					socket.state === "listening" &&
					socket.hostListener === hostListener
				) {
					const hostSocket = await hostListener.accept();
					if (socket.backlog.length >= socket.backlogLimit) {
						hostSocket.close().catch(() => {});
						continue;
					}

					// Create a kernel socket for this incoming connection
					const connId = this.create(
						socket.domain,
						socket.type,
						socket.protocol,
						socket.pid,
					);
					const connSock = this.get(connId)!;
					connSock.state = "connected";
					connSock.external = true;
					connSock.hostSocket = hostSocket;
					connSock.localAddr = socket.localAddr;
					this.applySocketOptionsToHostSocket(connSock);

					// Start read pump for the accepted socket
					this.startReadPump(connSock);

					// Queue in listener's backlog
					socket.backlog.push(connId);
					socket.acceptWaiters.wakeOne();
				}
			} catch {
				// Listener closed or error — stop pump
			}
		};
		pump();
	}

	/** Look up a listening socket by exact address key. */
	private getListeningSocket(key: string): KernelSocket | null {
		const id = this.listeners.get(key);
		if (id === undefined) return null;
		const sock = this.sockets.get(id);
		if (!sock || sock.state !== "listening") return null;
		return sock;
	}

	/** Replay stored socket options onto a host-backed connection. */
	private applySocketOptionsToHostSocket(socket: KernelSocket): void {
		for (const [key, value] of socket.options.entries()) {
			const [level, optname] = key.split(":").map(Number);
			if (Number.isNaN(level) || Number.isNaN(optname)) continue;
			this.applySocketOptionToHostSocket(socket, level, optname, value);
		}
	}

	/** Best-effort option forwarding for host-backed sockets. */
	private applySocketOptionToHostSocket(
		socket: KernelSocket,
		level: number,
		optname: number,
		optval: number,
	): void {
		if (!socket.external || !socket.hostSocket) {
			return;
		}

		try {
			socket.hostSocket.setOption(level, optname, optval);
		} catch {
			// Host adapters may not support every kernel-tracked option.
		}
	}

	/** Peek up to maxBytes from a socket's readBuffer without consuming. */
	private peekFromBuffer(socket: KernelSocket, maxBytes: number): Uint8Array {
		const chunks: Uint8Array[] = [];
		let totalLen = 0;

		for (const chunk of socket.readBuffer) {
			if (totalLen >= maxBytes) break;
			const remaining = maxBytes - totalLen;
			if (chunk.length <= remaining) {
				chunks.push(chunk);
				totalLen += chunk.length;
			} else {
				chunks.push(chunk.subarray(0, remaining));
				totalLen += remaining;
			}
		}

		if (chunks.length === 1) return new Uint8Array(chunks[0]);
		const result = new Uint8Array(totalLen);
		let offset = 0;
		for (const c of chunks) {
			result.set(c, offset);
			offset += c.length;
		}
		return result;
	}

	/** Consume up to maxBytes from a socket's readBuffer. */
	private consumeFromBuffer(
		socket: KernelSocket,
		maxBytes: number,
	): Uint8Array {
		const chunks: Uint8Array[] = [];
		let totalLen = 0;

		while (socket.readBuffer.length > 0 && totalLen < maxBytes) {
			const chunk = socket.readBuffer[0];
			const remaining = maxBytes - totalLen;

			if (chunk.length <= remaining) {
				chunks.push(chunk);
				totalLen += chunk.length;
				socket.readBuffer.shift();
			} else {
				chunks.push(chunk.subarray(0, remaining));
				socket.readBuffer[0] = chunk.subarray(remaining);
				totalLen += remaining;
			}
		}

		if (chunks.length === 1) return chunks[0];
		const result = new Uint8Array(totalLen);
		let offset = 0;
		for (const c of chunks) {
			result.set(c, offset);
			offset += c.length;
		}
		return result;
	}

	/** Wait for readable data, restarting when SA_RESTART applies. */
	private async recvBlocking(
		socket: KernelSocket,
		maxBytes: number,
		flags: number,
		pid: number,
	): Promise<Uint8Array | null> {
		while (true) {
			const result = this.recv(socket.id, maxBytes, flags);
			if (result !== null) return result;
			if (!this.canBlockForRecv(socket)) return null;
			await this.waitForSocketWake(socket.readWaiters, pid, "recv");
		}
	}

	/** Check whether recv() could still yield data later instead of EOF. */
	private canBlockForRecv(socket: KernelSocket): boolean {
		if (socket.state === "read-closed" || socket.state === "closed") {
			return false;
		}
		if (socket.readBuffer.length > 0) {
			return false;
		}
		if (socket.external) {
			return !socket.peerWriteClosed;
		}
		return (
			socket.peerId !== undefined &&
			this.sockets.has(socket.peerId) &&
			!socket.peerWriteClosed
		);
	}

	/** Wait for socket readiness or an interrupting signal. */
	private async waitForSocketWake(
		waiters: WaitQueue,
		pid: number,
		op: "accept" | "recv",
	): Promise<void> {
		const signalState = this.getSignalState?.(pid);
		if (!signalState) {
			const handle = waiters.enqueue();
			await handle.wait();
			waiters.remove(handle);
			return;
		}

		const startSeq = signalState.deliverySeq;
		const socketHandle = waiters.enqueue();
		const signalHandle = signalState.signalWaiters.enqueue();

		if (signalState.deliverySeq !== startSeq) {
			signalHandle.wake();
		}

		try {
			const winner = await Promise.race([
				socketHandle.wait().then(() => "socket" as const),
				signalHandle.wait().then(() => "signal" as const),
			]);

			if (winner === "signal" && signalState.deliverySeq !== startSeq) {
				if ((signalState.lastDeliveredFlags & SA_RESTART) !== 0) {
					return;
				}
				throw new KernelError(
					"EINTR",
					`${op} interrupted by signal ${signalState.lastDeliveredSignal ?? "unknown"}`,
				);
			}
		} finally {
			waiters.remove(socketHandle);
			signalState.signalWaiters.remove(signalHandle);
		}
	}

	/** Find a bound UDP socket that matches the given address (exact + wildcard). */
	findBoundUdp(addr: SockAddr): KernelSocket | null {
		if (isInetAddr(addr)) {
			const sock = this.getBoundUdpSocket(`${addr.host}:${addr.port}`);
			if (sock) return sock;
			const wild4 = this.getBoundUdpSocket(`0.0.0.0:${addr.port}`);
			if (wild4) return wild4;
			const wild6 = this.getBoundUdpSocket(`:::${addr.port}`);
			if (wild6) return wild6;
			return null;
		}
		return this.getBoundUdpSocket(addr.path) ?? null;
	}

	/** Look up a bound UDP socket by exact address key. */
	private getBoundUdpSocket(key: string): KernelSocket | null {
		const id = this.udpBindings.get(key);
		if (id === undefined) return null;
		const sock = this.sockets.get(id);
		if (!sock || sock.type !== SOCK_DGRAM) return null;
		return sock;
	}

	/** Check if a UDP address conflicts with an existing UDP binding. */
	private isUdpAddrInUse(addr: SockAddr, socket: KernelSocket): boolean {
		if (!isInetAddr(addr)) {
			return this.udpBindings.has(addr.path);
		}
		if (socket.options.get(optKey(SOL_SOCKET, SO_REUSEADDR)) === 1)
			return false;
		if (this.udpBindings.has(addrKey(addr))) return true;
		const isWildcard = addr.host === "0.0.0.0" || addr.host === "::";
		for (const existingId of this.udpBindings.values()) {
			const existing = this.sockets.get(existingId);
			if (!existing?.localAddr || !isInetAddr(existing.localAddr)) continue;
			if (existing.localAddr.port !== addr.port) continue;
			const existingIsWildcard =
				existing.localAddr.host === "0.0.0.0" ||
				existing.localAddr.host === "::";
			if (isWildcard || existingIsWildcard) return true;
		}
		return false;
	}

	/** Background pump: receives datagrams from host UDP socket and feeds kernel datagramQueue. */
	private startUdpRecvPump(socket: KernelSocket): void {
		if (!socket.hostUdpSocket) return;
		const hostUdpSocket = socket.hostUdpSocket;
		const pump = async () => {
			try {
				while (
					socket.state !== "closed" &&
					socket.hostUdpSocket === hostUdpSocket
				) {
					const result = await hostUdpSocket.recv();
					if (socket.datagramQueue.length < MAX_UDP_QUEUE_DEPTH) {
						socket.datagramQueue.push({
							data: result.data,
							srcAddr: {
								host: result.remoteAddr.host,
								port: result.remoteAddr.port,
							},
						});
						socket.readWaiters.wakeOne();
					}
				}
			} catch {
				// Socket closed or error — stop pump
			}
		};
		pump();
	}

	/** Check if an address conflicts with an existing TCP binding. */
	private isAddrInUse(addr: SockAddr, socket: KernelSocket): boolean {
		if (!isInetAddr(addr)) {
			return this.listeners.has(addr.path);
		}

		// SO_REUSEADDR on the new socket skips the check
		if (socket.options.get(optKey(SOL_SOCKET, SO_REUSEADDR)) === 1)
			return false;

		// Exact match
		if (this.listeners.has(addrKey(addr))) return true;

		// Wildcard overlap: same port, either side is wildcard
		const isWildcard = addr.host === "0.0.0.0" || addr.host === "::";
		for (const existingId of this.listeners.values()) {
			const existing = this.sockets.get(existingId);
			if (!existing?.localAddr || !isInetAddr(existing.localAddr)) continue;
			if (existing.localAddr.port !== addr.port) continue;
			const existingIsWildcard =
				existing.localAddr.host === "0.0.0.0" ||
				existing.localAddr.host === "::";
			if (isWildcard || existingIsWildcard) return true;
		}

		return false;
	}

	/** Assign a kernel-managed ephemeral port for bind(port=0). */
	private assignEphemeralPort(addr: SockAddr, socket: KernelSocket): SockAddr {
		if (!isInetAddr(addr) || addr.port !== 0) {
			socket.requestedEphemeralPort = false;
			return addr;
		}

		socket.requestedEphemeralPort = true;
		for (let port = EPHEMERAL_PORT_MIN; port <= EPHEMERAL_PORT_MAX; port++) {
			const candidate: InetAddr = { host: addr.host, port };
			const inUse =
				socket.type === SOCK_DGRAM
					? this.isUdpAddrInUse(candidate, socket)
					: this.isAddrInUse(candidate, socket);
			if (!inUse) {
				return candidate;
			}
		}

		throw new KernelError("EADDRINUSE", "no ephemeral ports available");
	}
}
