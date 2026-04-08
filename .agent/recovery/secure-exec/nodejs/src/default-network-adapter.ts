import * as dns from "node:dns";
import * as net from "node:net";
import * as http from "node:http";
import * as https from "node:https";
import * as zlib from "node:zlib";
import type {
	NetworkAdapter,
} from "@secure-exec/core";

export interface DefaultNetworkAdapterOptions {
	/** Pre-seed loopback ports that should bypass SSRF checks (e.g. host-managed servers). */
	initialExemptPorts?: Iterable<number>;
}

interface LoopbackAwareNetworkAdapter extends NetworkAdapter {
	__setLoopbackPortChecker?(checker: (hostname: string, port: number) => boolean): void;
}

/** Check whether an IP address falls in a private/reserved range (SSRF protection). */
export function isPrivateIp(ip: string): boolean {
	// Normalize IPv4-mapped IPv6 (::ffff:a.b.c.d → a.b.c.d)
	const normalized = ip.startsWith("::ffff:") ? ip.slice(7) : ip;

	if (net.isIPv4(normalized)) {
		const parts = normalized.split(".").map(Number);
		const [a, b] = parts;
		return (
			a === 10 ||
			(a === 172 && b >= 16 && b <= 31) ||
			(a === 192 && b === 168) ||
			a === 127 ||
			(a === 169 && b === 254) ||
			a === 0 ||
			(a >= 224 && a <= 239) ||
			(a >= 240)
		);
	}

	if (net.isIPv6(normalized)) {
		const lower = normalized.toLowerCase();
		return (
			lower === "::1" ||
			lower === "::" ||
			lower.startsWith("fc") ||
			lower.startsWith("fd") ||
			lower.startsWith("fe80") ||
			lower.startsWith("ff")
		);
	}

	return false;
}

/** Check whether a hostname is a loopback address (127.x.x.x, ::1, localhost). */
function isLoopbackHost(hostname: string): boolean {
	const bare = hostname.startsWith("[") && hostname.endsWith("]")
		? hostname.slice(1, -1)
		: hostname;
	if (bare === "localhost" || bare === "::1") return true;
	if (net.isIPv4(bare) && bare.startsWith("127.")) return true;
	return false;
}

function getUrlPort(parsed: URL): number {
	return parsed.port
		? Number(parsed.port)
		: parsed.protocol === "https:" ? 443 : 80;
}

/**
 * Resolve hostname to IP and block private/reserved ranges (SSRF protection).
 *
 * Loopback requests are allowed only when an explicit exemption or the
 * runtime-provided kernel listener checker claims the requested port.
 */
async function assertNotPrivateHost(
	url: string,
	allowLoopbackPort?: (hostname: string, port: number) => boolean,
): Promise<void> {
	const parsed = new URL(url);
	if (parsed.protocol === "data:" || parsed.protocol === "blob:") return;

	const hostname = parsed.hostname;
	const bare = hostname.startsWith("[") && hostname.endsWith("]")
		? hostname.slice(1, -1)
		: hostname;

	if (isLoopbackHost(hostname)) {
		const port = getUrlPort(parsed);
		if (allowLoopbackPort?.(hostname, port)) {
			return;
		}
	}

	if (net.isIP(bare)) {
		if (isPrivateIp(bare)) {
			throw new Error(`SSRF blocked: ${hostname} resolves to private IP`);
		}
		return;
	}

	const address = await new Promise<string>((resolve, reject) => {
		dns.lookup(bare, (err, addr) => {
			if (err) reject(err);
			else resolve(addr);
		});
	});

	if (isPrivateIp(address)) {
		throw new Error(`SSRF blocked: ${hostname} resolves to private IP ${address}`);
	}
}

const MAX_REDIRECTS = 20;

/**
 * Create a Node.js network adapter that provides real fetch, DNS, and HTTP
 * client support. Binary responses are base64-encoded with an
 * `x-body-encoding` header so the bridge can decode them.
 */
export function createDefaultNetworkAdapter(
	options?: DefaultNetworkAdapterOptions,
): NetworkAdapter {
	const upgradeSockets = new Map<number, import("stream").Duplex>();
	const initialExemptPorts = new Set<number>(options?.initialExemptPorts);
	let nextUpgradeSocketId = 1;
	let onUpgradeSocketData: ((socketId: number, dataBase64: string) => void) | null = null;
	let onUpgradeSocketEnd: ((socketId: number) => void) | null = null;
	let dynamicLoopbackPortChecker:
		| ((hostname: string, port: number) => boolean)
		| undefined;

	const allowLoopbackPort = (hostname: string, port: number): boolean => {
		if (initialExemptPorts.has(port)) return true;
		if (dynamicLoopbackPortChecker?.(hostname, port)) return true;
		return false;
	};

	const adapter: LoopbackAwareNetworkAdapter = {
		__setLoopbackPortChecker(checker) {
			dynamicLoopbackPortChecker = checker;
		},

		upgradeSocketWrite(socketId, dataBase64) {
			const socket = upgradeSockets.get(socketId);
			if (socket && !socket.destroyed) {
				socket.write(Buffer.from(dataBase64, "base64"));
			}
		},

		upgradeSocketEnd(socketId) {
			const socket = upgradeSockets.get(socketId);
			if (socket && !socket.destroyed) {
				socket.end();
			}
		},

		upgradeSocketDestroy(socketId) {
			const socket = upgradeSockets.get(socketId);
			if (socket) {
				socket.destroy();
				upgradeSockets.delete(socketId);
			}
		},

		setUpgradeSocketCallbacks(callbacks) {
			onUpgradeSocketData = callbacks.onData;
			onUpgradeSocketEnd = callbacks.onEnd;
		},

		async fetch(url, requestOptions) {
			let currentUrl = url;
			let redirected = false;

			for (let i = 0; i <= MAX_REDIRECTS; i++) {
				await assertNotPrivateHost(currentUrl, allowLoopbackPort);

				const response = await fetch(currentUrl, {
					method: requestOptions?.method || "GET",
					headers: requestOptions?.headers,
					body: requestOptions?.body,
					redirect: "manual",
				});

				const status = response.status;
				if (status === 301 || status === 302 || status === 303 || status === 307 || status === 308) {
					const location = response.headers.get("location");
					if (!location) break;
					currentUrl = new URL(location, currentUrl).href;
					redirected = true;
					if (status === 301 || status === 302 || status === 303) {
						requestOptions = { ...requestOptions, method: "GET", body: undefined };
					}
					continue;
				}

				const headers: Record<string, string> = {};
				response.headers.forEach((value, key) => {
					headers[key] = value;
				});

				delete headers["content-encoding"];

				const contentType = response.headers.get("content-type") || "";
				const isBinary =
					contentType.includes("octet-stream") ||
					contentType.includes("gzip") ||
					currentUrl.endsWith(".tgz");

				let body: string;
				if (isBinary) {
					const buffer = await response.arrayBuffer();
					body = Buffer.from(buffer).toString("base64");
					headers["x-body-encoding"] = "base64";
				} else {
					body = await response.text();
				}

				return {
					ok: response.ok,
					status: response.status,
					statusText: response.statusText,
					headers,
					body,
					url: currentUrl,
					redirected,
				};
			}

			throw new Error("Too many redirects");
		},

		async dnsLookup(hostname) {
			return new Promise((resolve) => {
				dns.lookup(hostname, (err, address, family) => {
					if (err) {
						resolve({ error: err.message, code: err.code || "ENOTFOUND" });
					} else {
						resolve({ address, family });
					}
				});
			});
		},

		async httpRequest(url, requestOptions) {
			await assertNotPrivateHost(url, allowLoopbackPort);
			type HttpRequestResult = Awaited<ReturnType<NetworkAdapter["httpRequest"]>> & {
				rawHeaders?: string[];
			};
			return new Promise<HttpRequestResult>((resolve, reject) => {
				const urlObj = new URL(url);
				const isHttps = urlObj.protocol === "https:";
				const transport = isHttps ? https : http;
				const reqOptions: https.RequestOptions = {
					hostname: urlObj.hostname,
					port: urlObj.port || (isHttps ? 443 : 80),
					path: urlObj.pathname + urlObj.search,
					method: requestOptions?.method || "GET",
					headers: requestOptions?.headers || {},
					// Keep host-side pooling disabled so sandbox http.Agent semantics
					// are controlled entirely by the bridge layer.
					agent: false,
					...(isHttps && requestOptions?.rejectUnauthorized !== undefined && {
						rejectUnauthorized: requestOptions.rejectUnauthorized,
					}),
				};

				const req = transport.request(reqOptions, (res) => {
					const chunks: Buffer[] = [];
					res.on("data", (chunk: Buffer) => chunks.push(chunk));
					res.on("end", async () => {
						let buffer: Buffer = Buffer.concat(chunks);

						const contentEncoding = res.headers["content-encoding"];
						if (contentEncoding === "gzip" || contentEncoding === "deflate") {
							try {
								buffer = await new Promise((responseResolve, responseReject) => {
									const decompress =
										contentEncoding === "gzip" ? zlib.gunzip : zlib.inflate;
									decompress(buffer, (err, result) => {
										if (err) responseReject(err);
										else responseResolve(result);
									});
								});
							} catch {
								// Preserve the original buffer when decompression fails.
							}
						}

						const contentType = res.headers["content-type"] || "";
						const isBinary =
							contentType.includes("octet-stream") ||
							contentType.includes("gzip") ||
							url.endsWith(".tgz");

						const headers: Record<string, string> = {};
						const rawHeaders = [...res.rawHeaders];
						Object.entries(res.headers).forEach(([key, value]) => {
							if (typeof value === "string") headers[key] = value;
							else if (Array.isArray(value)) headers[key] = value.join(", ");
						});

						delete headers["content-encoding"];

						const trailers: Record<string, string> = {};
						if (res.trailers) {
							Object.entries(res.trailers).forEach(([key, value]) => {
								if (typeof value === "string") trailers[key] = value;
							});
						}
						const hasTrailers = Object.keys(trailers).length > 0;

						const base = {
							status: res.statusCode || 200,
							statusText: res.statusMessage || "OK",
							headers,
							rawHeaders,
							url,
							...(hasTrailers ? { trailers } : {}),
						};

						if (isBinary) {
							headers["x-body-encoding"] = "base64";
							resolve({ ...base, body: buffer.toString("base64") });
						} else {
							resolve({ ...base, body: buffer.toString("utf-8") });
						}
					});
					res.on("error", reject);
				});

				req.on("upgrade", (res, socket, head) => {
					const headers: Record<string, string> = {};
					const rawHeaders = [...res.rawHeaders];
					Object.entries(res.headers).forEach(([key, value]) => {
						if (typeof value === "string") headers[key] = value;
						else if (Array.isArray(value)) headers[key] = value.join(", ");
					});

					const socketId = nextUpgradeSocketId++;
					upgradeSockets.set(socketId, socket);

					socket.on("data", (chunk) => {
						if (onUpgradeSocketData) {
							onUpgradeSocketData(socketId, chunk.toString("base64"));
						}
					});
					socket.on("close", () => {
						if (onUpgradeSocketEnd) {
							onUpgradeSocketEnd(socketId);
						}
						upgradeSockets.delete(socketId);
					});

					resolve({
						status: res.statusCode || 101,
						statusText: res.statusMessage || "Switching Protocols",
						headers,
						rawHeaders,
						body: head.toString("base64"),
						url,
						upgradeSocketId: socketId,
					});
				});

				req.on("connect", (res, socket, head) => {
					const headers: Record<string, string> = {};
					const rawHeaders = [...res.rawHeaders];
					Object.entries(res.headers).forEach(([key, value]) => {
						if (typeof value === "string") headers[key] = value;
						else if (Array.isArray(value)) headers[key] = value.join(", ");
					});

					const socketId = nextUpgradeSocketId++;
					upgradeSockets.set(socketId, socket);

					socket.on("data", (chunk) => {
						if (onUpgradeSocketData) {
							onUpgradeSocketData(socketId, chunk.toString("base64"));
						}
					});
					socket.on("close", () => {
						if (onUpgradeSocketEnd) {
							onUpgradeSocketEnd(socketId);
						}
						upgradeSockets.delete(socketId);
					});

					resolve({
						status: res.statusCode || 200,
						statusText: res.statusMessage || "Connection established",
						headers,
						rawHeaders,
						body: head.toString("base64"),
						url,
						upgradeSocketId: socketId,
					});
				});

				req.on("error", reject);
				if (requestOptions?.body) req.write(requestOptions.body);
				req.end();
			});
		},
	};

	return adapter;
}
