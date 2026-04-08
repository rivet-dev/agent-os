/**
 * Concrete HostNetworkAdapter for Node.js, delegating to node:net,
 * node:dgram, and node:dns for real external I/O.
 */

import * as dgram from "node:dgram";
import * as dns from "node:dns";
import * as net from "node:net";
import type {
	DnsResult,
	HostListener,
	HostNetworkAdapter,
	HostSocket,
	HostUdpSocket,
} from "@secure-exec/core";
import {
	IPPROTO_TCP,
	SOL_SOCKET,
	SO_KEEPALIVE,
	TCP_NODELAY,
} from "@secure-exec/core/internal/kernel";

/**
 * Queued-read adapter: incoming data/EOF/errors are buffered so that
 * each read() call returns the next chunk or null for EOF.
 */
class NodeHostSocket implements HostSocket {
	private socket: net.Socket;
	private readQueue: (Uint8Array | null)[] = [];
	private waiters: ((value: Uint8Array | null) => void)[] = [];
	private ended = false;

	constructor(socket: net.Socket) {
		this.socket = socket;

		socket.on("data", (chunk: Buffer) => {
			const data = new Uint8Array(chunk);
			const waiter = this.waiters.shift();
			if (waiter) {
				waiter(data);
			} else {
				this.readQueue.push(data);
			}
		});

		socket.on("end", () => {
			this.ended = true;
			const waiter = this.waiters.shift();
			if (waiter) {
				waiter(null);
			} else {
				this.readQueue.push(null);
			}
		});

		socket.on("error", (err: Error) => {
			// Wake all pending readers with EOF
			for (const waiter of this.waiters.splice(0)) {
				waiter(null);
			}
			if (!this.ended) {
				this.ended = true;
				this.readQueue.push(null);
			}
		});
	}

	async write(data: Uint8Array): Promise<void> {
		return new Promise((resolve, reject) => {
			this.socket.write(data, (err) => {
				if (err) reject(err);
				else resolve();
			});
		});
	}

	async read(): Promise<Uint8Array | null> {
		const queued = this.readQueue.shift();
		if (queued !== undefined) return queued;
		if (this.ended) return null;
		return new Promise<Uint8Array | null>((resolve) => {
			this.waiters.push(resolve);
		});
	}

	async close(): Promise<void> {
		return new Promise((resolve) => {
			if (this.socket.destroyed) {
				resolve();
				return;
			}
			this.socket.once("close", () => resolve());
			this.socket.destroy();
		});
	}

	setOption(level: number, optname: number, optval: number): void {
		if (level === IPPROTO_TCP && optname === TCP_NODELAY) {
			this.socket.setNoDelay(optval !== 0);
			return;
		}
		if (level === SOL_SOCKET && optname === SO_KEEPALIVE) {
			this.socket.setKeepAlive(optval !== 0);
		}
	}

	shutdown(how: "read" | "write" | "both"): void {
		if (how === "write" || how === "both") {
			this.socket.end();
		}
		if (how === "read" || how === "both") {
			this.socket.pause();
			this.socket.removeAllListeners("data");
			if (!this.ended) {
				this.ended = true;
				const waiter = this.waiters.shift();
				if (waiter) waiter(null);
				else this.readQueue.push(null);
			}
		}
	}
}

/**
 * TCP listener backed by node:net.Server. Incoming connections are
 * queued so each accept() call returns the next one.
 */
class NodeHostListener implements HostListener {
	private server: net.Server;
	private _port: number;
	private connQueue: net.Socket[] = [];
	private waiters: ((socket: net.Socket) => void)[] = [];
	private closed = false;

	constructor(server: net.Server, port: number) {
		this.server = server;
		this._port = port;

		server.on("connection", (socket: net.Socket) => {
			const waiter = this.waiters.shift();
			if (waiter) {
				waiter(socket);
			} else {
				this.connQueue.push(socket);
			}
		});
	}

	get port(): number {
		return this._port;
	}

	async accept(): Promise<HostSocket> {
		const queued = this.connQueue.shift();
		if (queued) return new NodeHostSocket(queued);
		if (this.closed) throw new Error("Listener closed");
		return new Promise<HostSocket>((resolve, reject) => {
			if (this.closed) {
				reject(new Error("Listener closed"));
				return;
			}
			this.waiters.push((socket) => {
				resolve(new NodeHostSocket(socket));
			});
		});
	}

	async close(): Promise<void> {
		this.closed = true;
		// Reject pending accept waiters
		for (const _waiter of this.waiters.splice(0)) {
			// Resolve with a destroyed socket to signal closure — caller handles
			// the error via the socket's error/close events
		}
		return new Promise<void>((resolve, reject) => {
			this.server.close((err) => {
				if (err) reject(err);
				else resolve();
			});
		});
	}
}

/**
 * UDP socket backed by node:dgram.Socket. Messages are queued
 * so each recv() call returns the next datagram.
 */
class NodeHostUdpSocket implements HostUdpSocket {
	private socket: dgram.Socket;
	private msgQueue: { data: Uint8Array; remoteAddr: { host: string; port: number } }[] = [];
	private waiters: ((msg: { data: Uint8Array; remoteAddr: { host: string; port: number } }) => void)[] = [];
	private closed = false;

	constructor(socket: dgram.Socket) {
		this.socket = socket;

		socket.on("message", (msg: Buffer, rinfo: dgram.RemoteInfo) => {
			const entry = {
				data: new Uint8Array(msg),
				remoteAddr: { host: rinfo.address, port: rinfo.port },
			};
			const waiter = this.waiters.shift();
			if (waiter) {
				waiter(entry);
			} else {
				this.msgQueue.push(entry);
			}
		});
	}

	async recv(): Promise<{ data: Uint8Array; remoteAddr: { host: string; port: number } }> {
		const queued = this.msgQueue.shift();
		if (queued) return queued;
		if (this.closed) throw new Error("UDP socket closed");
		return new Promise((resolve, reject) => {
			if (this.closed) {
				reject(new Error("UDP socket closed"));
				return;
			}
			this.waiters.push(resolve);
		});
	}

	async close(): Promise<void> {
		this.closed = true;
		return new Promise((resolve) => {
			this.socket.close(() => resolve());
		});
	}
}

/** Create a Node.js HostNetworkAdapter that uses real OS networking. */
export function createNodeHostNetworkAdapter(): HostNetworkAdapter {
	return {
		async tcpConnect(host: string, port: number): Promise<HostSocket> {
			return new Promise<HostSocket>((resolve, reject) => {
				const socket = net.connect({ host, port });
				socket.once("connect", () => {
					socket.removeListener("error", reject);
					resolve(new NodeHostSocket(socket));
				});
				socket.once("error", (err) => {
					socket.removeListener("connect", resolve as () => void);
					reject(err);
				});
			});
		},

		async tcpListen(host: string, port: number): Promise<HostListener> {
			return new Promise<HostListener>((resolve, reject) => {
				const server = net.createServer();
				server.once("listening", () => {
					server.removeListener("error", reject);
					const addr = server.address() as net.AddressInfo;
					resolve(new NodeHostListener(server, addr.port));
				});
				server.once("error", (err) => {
					server.removeListener("listening", resolve as () => void);
					reject(err);
				});
				server.listen(port, host);
			});
		},

		async udpBind(host: string, port: number): Promise<HostUdpSocket> {
			return new Promise<HostUdpSocket>((resolve, reject) => {
				const socket = dgram.createSocket("udp4");
				socket.once("listening", () => {
					socket.removeListener("error", reject);
					resolve(new NodeHostUdpSocket(socket));
				});
				socket.once("error", (err) => {
					socket.removeListener("listening", resolve as () => void);
					reject(err);
				});
				socket.bind(port, host);
			});
		},

		async udpSend(
			socket: HostUdpSocket,
			data: Uint8Array,
			host: string,
			port: number,
		): Promise<void> {
			// Access the underlying dgram socket via the wrapper
			const udp = socket as NodeHostUdpSocket;
			const inner = (udp as unknown as { socket: dgram.Socket }).socket;
			return new Promise<void>((resolve, reject) => {
				inner.send(data, 0, data.length, port, host, (err) => {
					if (err) reject(err);
					else resolve();
				});
			});
		},

		async dnsLookup(hostname: string, rrtype: string): Promise<DnsResult> {
			const family = rrtype === "AAAA" ? 6 : 4;
			return new Promise<DnsResult>((resolve, reject) => {
				dns.lookup(hostname, { family }, (err, address, resultFamily) => {
					if (err) reject(err);
					else resolve({ address, family: resultFamily as 4 | 6 });
				});
			});
		},
	};
}
