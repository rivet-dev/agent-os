/**
 * Host adapter interfaces for kernel network delegation.
 *
 * The kernel uses these interfaces to delegate external I/O to the host
 * without knowing the host implementation. Node.js driver implements
 * using node:net / node:dgram; browser driver may use WebSocket proxy.
 */

/** A connected TCP socket on the host. */
export interface HostSocket {
	write(data: Uint8Array): Promise<void>;
	/** Returns data or null for EOF. */
	read(): Promise<Uint8Array | null>;
	close(): Promise<void>;
	/** Forward kernel socket options to host socket. */
	setOption(level: number, optname: number, optval: number): void;
	/** TCP half-close / full shutdown. */
	shutdown(how: "read" | "write" | "both"): void;
}

/** A TCP listener on the host. */
export interface HostListener {
	/** Accept the next incoming connection. */
	accept(): Promise<HostSocket>;
	close(): Promise<void>;
	/** Actual bound port (useful when binding port 0 for ephemeral ports). */
	readonly port: number;
}

/** A UDP socket on the host. */
export interface HostUdpSocket {
	recv(): Promise<{ data: Uint8Array; remoteAddr: { host: string; port: number } }>;
	close(): Promise<void>;
}

/** DNS lookup result. */
export interface DnsResult {
	address: string;
	family: 4 | 6;
}

/** Host adapter that the kernel delegates external network I/O to. */
export interface HostNetworkAdapter {
	// TCP
	tcpConnect(host: string, port: number): Promise<HostSocket>;
	tcpListen(host: string, port: number): Promise<HostListener>;

	// UDP
	udpBind(host: string, port: number): Promise<HostUdpSocket>;
	udpSend(socket: HostUdpSocket, data: Uint8Array, host: string, port: number): Promise<void>;

	// DNS
	dnsLookup(hostname: string, rrtype: string): Promise<DnsResult>;
}
