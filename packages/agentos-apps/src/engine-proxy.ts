import { randomUUID } from "node:crypto";
import {
	createServer,
	type IncomingMessage,
	type Server,
	type ServerResponse,
} from "node:http";
import { type AddressInfo, connect as connectTcp } from "node:net";
import type { Duplex } from "node:stream";
import { connect as connectTls } from "node:tls";

const MAX_CAPABILITIES = 16_384;
const MAX_REQUESTS_PER_CAPABILITY = 256;
const MAX_ACTOR_IDS_PER_CAPABILITY = 4_096;
const MAX_WEBSOCKET_HANDSHAKE_BYTES = 64 * 1024;
const CAPABILITY_WARNING_THRESHOLD = MAX_REQUESTS_PER_CAPABILITY / 2;

interface Capability {
	owner: string;
	path: string;
	upstreamEndpoint: string;
	upstreamToken?: string;
	namespace: string;
	pool: string;
	maxRequestBytes: number;
	maxResponseBytes: number;
	activeRequests: number;
	warned: boolean;
	actorIds: Set<string>;
	sockets: Set<Duplex>;
}

export interface GuestEngineProxyRegistration {
	endpoint: string;
	port: number;
}

const capabilities = new Map<string, Capability>();
const ownerPaths = new Map<string, string>();
let serverPromise: Promise<{ server: Server; port: number }> | undefined;

function requestPathAllowed(method: string, pathname: string): boolean {
	if (method === "GET" && pathname === "/metadata") return true;
	if (method === "GET" && pathname === "/envoys/connect") return true;
	if (pathname === "/actors" && (method === "POST" || method === "PUT")) {
		return true;
	}
	if (method === "GET" && /^\/actors\/[^/]+\/kv\/keys\/[^/]+$/.test(pathname)) {
		return true;
	}
	if (method === "DELETE" && /^\/actors\/[^/]+$/.test(pathname)) {
		return true;
	}
	return pathname.startsWith("/gateway/");
}

function stripHopByHopHeaders(headers: Headers): void {
	for (const name of [
		"connection",
		"keep-alive",
		"proxy-authenticate",
		"proxy-authorization",
		"te",
		"trailer",
		"transfer-encoding",
		"upgrade",
	]) {
		headers.delete(name);
	}
}

async function readRequest(
	request: IncomingMessage,
	limit: number,
): Promise<Uint8Array | undefined> {
	const chunks: Buffer[] = [];
	let bytes = 0;
	for await (const chunk of request) {
		const buffer = Buffer.from(chunk);
		bytes += buffer.byteLength;
		if (bytes > limit) {
			throw new RangeError(
				`guest Engine proxy request exceeds maxRequestBytes ${limit}`,
			);
		}
		chunks.push(buffer);
	}
	return bytes === 0 ? undefined : Buffer.concat(chunks, bytes);
}

async function readResponse(
	response: Response,
	limit: number,
): Promise<Buffer> {
	if (!response.body) return Buffer.alloc(0);
	const chunks: Buffer[] = [];
	let bytes = 0;
	const reader = response.body.getReader();
	try {
		for (;;) {
			const { value, done } = await reader.read();
			if (done) break;
			const buffer = Buffer.from(value);
			bytes += buffer.byteLength;
			if (bytes > limit) {
				await reader.cancel("guest Engine proxy response limit exceeded");
				throw new RangeError(
					`guest Engine proxy response exceeds maxResponseBytes ${limit}`,
				);
			}
			chunks.push(buffer);
		}
	} finally {
		reader.releaseLock();
	}
	return Buffer.concat(chunks, bytes);
}

function actorIdFromPath(pathname: string): string | undefined {
	const match = pathname.match(/^\/(?:gateway|actors)\/([^/]+)/);
	if (!match?.[1]) return undefined;
	const actorId = decodeURIComponent(match[1]);
	return actorId.includes("@") ? undefined : actorId;
}

async function actorBelongsToCapability(
	capability: Capability,
	actorId: string,
): Promise<boolean> {
	if (capability.actorIds.has(actorId)) return true;
	const url = new URL("/actors", capability.upstreamEndpoint);
	url.searchParams.set("namespace", capability.namespace);
	url.searchParams.set("actor_ids", actorId);
	const response = await fetch(url, {
		headers: capability.upstreamToken
			? { authorization: `Bearer ${capability.upstreamToken}` }
			: undefined,
	});
	if (!response.ok) return false;
	const body = (await response.json()) as {
		actors?: Array<{
			actor_id?: unknown;
			runner_name_selector?: unknown;
		}>;
	};
	const belongs =
		body.actors?.some(
			(actor) =>
				actor.actor_id === actorId &&
				actor.runner_name_selector === capability.pool,
		) ?? false;
	if (belongs) {
		if (capability.actorIds.size >= MAX_ACTOR_IDS_PER_CAPABILITY) {
			throw new RangeError(
				`guest Engine proxy actor cache reached ${MAX_ACTOR_IDS_PER_CAPABILITY}; deploy fewer actors or raise the host limit`,
			);
		}
		capability.actorIds.add(actorId);
	}
	return belongs;
}

async function resolveQueryGatewayActor(
	capability: Capability,
	pathname: string,
	upstreamUrl: URL,
): Promise<boolean> {
	const match = pathname.match(/^\/gateway\/([^/]+)(\/.*)?$/);
	if (!match?.[1]) return false;
	const url = new URL("/actors", capability.upstreamEndpoint);
	url.searchParams.set("namespace", capability.namespace);
	url.searchParams.set("name", decodeURIComponent(match[1]));
	const rawKey = upstreamUrl.searchParams.get("rvt-key");
	const keyParts = rawKey === null || rawKey === "" ? [] : rawKey.split(",");
	url.searchParams.set(
		"key",
		keyParts.length === 0
			? "/"
			: keyParts
					.map((part) =>
						part === ""
							? "\\0"
							: part.replaceAll("\\", "\\\\").replaceAll("/", "\\/"),
					)
					.join("/"),
	);
	const response = await fetch(url, {
		headers: capability.upstreamToken
			? { authorization: `Bearer ${capability.upstreamToken}` }
			: undefined,
	});
	if (!response.ok) return false;
	const body = (await response.json()) as {
		actors?: Array<{
			actor_id?: unknown;
			runner_name_selector?: unknown;
		}>;
	};
	const actor = body.actors?.find(
		(candidate) =>
			typeof candidate.actor_id === "string" &&
			candidate.runner_name_selector === capability.pool,
	);
	if (!actor || typeof actor.actor_id !== "string") return false;
	upstreamUrl.pathname = `/gateway/${encodeURIComponent(actor.actor_id)}${match[2] ?? ""}`;
	for (const name of [
		...new Set(
			Array.from(upstreamUrl.searchParams.keys()).filter((candidate) =>
				candidate.startsWith("rvt-"),
			),
		),
	]) {
		upstreamUrl.searchParams.delete(name);
	}
	if (capability.actorIds.size < MAX_ACTOR_IDS_PER_CAPABILITY) {
		capability.actorIds.add(actor.actor_id);
	}
	return true;
}

async function scopedUpstreamUrl(
	capability: Capability,
	pathname: string,
	incomingUrl: URL,
	method: string,
): Promise<URL | undefined> {
	const upstreamUrl = new URL(pathname, capability.upstreamEndpoint);
	for (const [name, value] of incomingUrl.searchParams) {
		upstreamUrl.searchParams.append(name, value);
	}
	upstreamUrl.searchParams.delete("rvt-token");
	if (pathname === "/envoys/connect") {
		upstreamUrl.searchParams.set("namespace", capability.namespace);
		upstreamUrl.searchParams.set("pool_name", capability.pool);
	} else if (pathname.startsWith("/gateway/")) {
		const queryMethod = upstreamUrl.searchParams.get("rvt-method");
		if (queryMethod === "get") {
			if (
				!(await resolveQueryGatewayActor(capability, pathname, upstreamUrl))
			) {
				return undefined;
			}
		} else if (queryMethod === "getOrCreate") {
			upstreamUrl.searchParams.set("rvt-namespace", capability.namespace);
			upstreamUrl.searchParams.set("rvt-pool", capability.pool);
			upstreamUrl.searchParams.set("rvt-runner", capability.pool);
		} else if (queryMethod) {
			return undefined;
		} else {
			const actorId = actorIdFromPath(pathname);
			if (!actorId || !(await actorBelongsToCapability(capability, actorId))) {
				return undefined;
			}
			for (const name of [
				...new Set(
					Array.from(upstreamUrl.searchParams.keys()).filter((candidate) =>
						candidate.startsWith("rvt-"),
					),
				),
			]) {
				upstreamUrl.searchParams.delete(name);
			}
		}
	} else {
		upstreamUrl.searchParams.set("namespace", capability.namespace);
	}
	if (
		(method === "DELETE" || pathname.includes("/kv/keys/")) &&
		actorIdFromPath(pathname)
	) {
		const actorId = actorIdFromPath(pathname);
		if (actorId && !(await actorBelongsToCapability(capability, actorId))) {
			return undefined;
		}
	}
	return upstreamUrl;
}

async function forward(
	request: IncomingMessage,
	response: ServerResponse,
	capability: Capability,
	pathname: string,
): Promise<void> {
	const method = request.method ?? "GET";
	if (!requestPathAllowed(method, pathname)) {
		response.writeHead(403).end("Engine management route is not available");
		return;
	}

	const incomingUrl = new URL(request.url ?? "/", "http://agentos.invalid");
	const upstreamUrl = await scopedUpstreamUrl(
		capability,
		pathname,
		incomingUrl,
		method,
	);
	if (!upstreamUrl) {
		response.writeHead(403).end("Actor does not belong to this app");
		return;
	}

	const headers = new Headers();
	for (const [name, value] of Object.entries(request.headers)) {
		if (value !== undefined) {
			headers.set(name, Array.isArray(value) ? value.join(", ") : value);
		}
	}
	stripHopByHopHeaders(headers);
	headers.delete("host");
	headers.delete("authorization");
	headers.delete("x-rivet-token");
	if (capability.upstreamToken && !pathname.startsWith("/gateway/")) {
		headers.set("authorization", `Bearer ${capability.upstreamToken}`);
	}

	let body = await readRequest(request, capability.maxRequestBytes);
	if (
		body &&
		pathname === "/actors" &&
		(method === "POST" || method === "PUT")
	) {
		const parsed = JSON.parse(new TextDecoder().decode(body)) as Record<
			string,
			unknown
		>;
		parsed.runner_name_selector = capability.pool;
		body = Buffer.from(JSON.stringify(parsed));
		headers.set("content-type", "application/json");
		headers.set("content-length", String(body.byteLength));
	}

	const upstream = await fetch(upstreamUrl, {
		method,
		headers,
		body:
			method === "GET" || method === "HEAD"
				? undefined
				: Buffer.from(body ?? []),
		redirect: "manual",
	});
	let responseBody = await readResponse(upstream, capability.maxResponseBytes);
	const responseHeaders = new Headers(upstream.headers);
	stripHopByHopHeaders(responseHeaders);
	if (
		pathname === "/metadata" &&
		upstream.ok &&
		responseHeaders.get("content-type")?.includes("application/json")
	) {
		const metadata = JSON.parse(responseBody.toString("utf8")) as Record<
			string,
			unknown
		>;
		metadata.clientEndpoint = `http://127.0.0.1:${(await ensureServer()).port}/${capability.path}`;
		metadata.clientNamespace = capability.namespace;
		delete metadata.clientToken;
		responseBody = Buffer.from(JSON.stringify(metadata));
		responseHeaders.set("content-length", String(responseBody.byteLength));
	}
	responseHeaders.forEach((value, name) => {
		response.setHeader(name, value);
	});
	response.statusCode = upstream.status;
	response.statusMessage = upstream.statusText;
	response.end(responseBody);
}

async function handleRequest(
	request: IncomingMessage,
	response: ServerResponse,
): Promise<void> {
	const url = new URL(request.url ?? "/", "http://agentos.invalid");
	const parts = url.pathname.split("/").filter(Boolean);
	const capabilityPath = parts.shift();
	const capability = capabilityPath
		? capabilities.get(capabilityPath)
		: undefined;
	if (!capability) {
		response.writeHead(401).end("Unknown guest Engine capability");
		return;
	}
	if (capability.activeRequests >= MAX_REQUESTS_PER_CAPABILITY) {
		response
			.writeHead(503)
			.end(
				`guest Engine proxy reached ${MAX_REQUESTS_PER_CAPABILITY} concurrent requests; raise the host limit`,
			);
		return;
	}
	capability.activeRequests += 1;
	if (
		!capability.warned &&
		capability.activeRequests >= CAPABILITY_WARNING_THRESHOLD
	) {
		capability.warned = true;
		console.warn(
			`agentOS Apps guest Engine proxy passed 50% of its ${MAX_REQUESTS_PER_CAPABILITY}-request limit`,
		);
	}
	try {
		await forward(request, response, capability, `/${parts.join("/")}`);
	} catch (error) {
		console.error("agentOS Apps guest Engine proxy failed", error);
		if (!response.headersSent) response.writeHead(502);
		if (!response.writableEnded) response.end("Guest Engine proxy failed");
	} finally {
		capability.activeRequests -= 1;
		if (capability.activeRequests < CAPABILITY_WARNING_THRESHOLD) {
			capability.warned = false;
		}
	}
}

async function handleUpgrade(
	request: IncomingMessage,
	socket: Duplex,
	head: Buffer,
): Promise<void> {
	const incomingUrl = new URL(request.url ?? "/", "http://agentos.invalid");
	const parts = incomingUrl.pathname.split("/").filter(Boolean);
	const capabilityPath = parts.shift();
	const capability = capabilityPath
		? capabilities.get(capabilityPath)
		: undefined;
	const pathname = `/${parts.join("/")}`;
	if (
		!capability ||
		!requestPathAllowed("GET", pathname) ||
		capability.activeRequests >= MAX_REQUESTS_PER_CAPABILITY
	) {
		socket.end("HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n");
		return;
	}
	capability.activeRequests += 1;
	capability.sockets.add(socket);
	if (
		!capability.warned &&
		capability.activeRequests >= CAPABILITY_WARNING_THRESHOLD
	) {
		capability.warned = true;
		console.warn(
			`agentOS Apps guest Engine proxy passed 50% of its ${MAX_REQUESTS_PER_CAPABILITY}-request limit`,
		);
	}
	let released = false;
	const release = () => {
		if (released) return;
		released = true;
		capability.activeRequests -= 1;
		capability.sockets.delete(socket);
		if (capability.activeRequests < CAPABILITY_WARNING_THRESHOLD) {
			capability.warned = false;
		}
	};
	try {
		const upstreamUrl = await scopedUpstreamUrl(
			capability,
			pathname,
			incomingUrl,
			"GET",
		);
		if (!upstreamUrl) {
			socket.end("HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n");
			release();
			return;
		}
		const secure = upstreamUrl.protocol === "https:";
		if (!secure && upstreamUrl.protocol !== "http:") {
			throw new TypeError(
				`unsupported Engine proxy protocol ${upstreamUrl.protocol}`,
			);
		}
		const port = Number(upstreamUrl.port || (secure ? 443 : 80));
		const upstream = secure
			? connectTls({
					host: upstreamUrl.hostname,
					port,
					servername: upstreamUrl.hostname,
				})
			: connectTcp({ host: upstreamUrl.hostname, port });
		const close = () => {
			release();
			if (!socket.destroyed) socket.destroy();
			if (!upstream.destroyed) upstream.destroy();
		};
		socket.once("close", close);
		upstream.once("close", close);
		upstream.once("error", (error) => {
			console.error("agentOS Apps guest Engine WebSocket proxy failed", error);
			if (!socket.destroyed) {
				socket.end("HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n");
			}
		});
		const onConnected = () => {
			const headers = new Map<string, string>();
			for (const [name, value] of Object.entries(request.headers)) {
				if (value !== undefined) {
					headers.set(name, Array.isArray(value) ? value.join(", ") : value);
				}
			}
			headers.set("host", upstreamUrl.host);
			headers.delete("authorization");
			headers.delete("x-rivet-token");
			const protocols = (headers.get("sec-websocket-protocol") ?? "")
				.split(",")
				.map((protocol) => protocol.trim())
				.filter(
					(protocol) =>
						protocol.length > 0 && !protocol.startsWith("rivet_token."),
				);
			if (pathname === "/envoys/connect" && capability.upstreamToken) {
				protocols.push(`rivet_token.${capability.upstreamToken}`);
			}
			if (protocols.length > 0) {
				headers.set("sec-websocket-protocol", protocols.join(", "));
			} else {
				headers.delete("sec-websocket-protocol");
			}
			const requestHead = [
				`GET ${upstreamUrl.pathname}${upstreamUrl.search} HTTP/1.1`,
				...Array.from(headers, ([name, value]) => `${name}: ${value}`),
				"",
				"",
			].join("\r\n");
			upstream.write(requestHead);
			if (head.byteLength > 0) upstream.write(head);
			socket.pipe(upstream);
			let handshake = Buffer.alloc(0);
			const receiveHandshake = (chunk: Buffer) => {
				handshake = Buffer.concat([handshake, chunk]);
				if (handshake.byteLength > MAX_WEBSOCKET_HANDSHAKE_BYTES) {
					close();
					return;
				}
				const boundary = handshake.indexOf("\r\n\r\n");
				if (boundary < 0) return;
				upstream.pause();
				upstream.off("data", receiveHandshake);
				const header = handshake
					.subarray(0, boundary)
					.toString("latin1")
					.split("\r\n")
					.filter(
						(line) =>
							!(
								line.toLowerCase().startsWith("sec-websocket-protocol:") &&
								line.includes("rivet_token.")
							),
					)
					.join("\r\n");
				socket.write(`${header}\r\n\r\n`, "latin1");
				const remainder = handshake.subarray(boundary + 4);
				if (remainder.byteLength > 0) socket.write(remainder);
				upstream.pipe(socket);
				upstream.resume();
			};
			upstream.on("data", receiveHandshake);
		};
		if (secure) upstream.once("secureConnect", onConnected);
		else upstream.once("connect", onConnected);
	} catch (error) {
		console.error("agentOS Apps guest Engine WebSocket proxy failed", error);
		socket.end("HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n");
		release();
	}
}

async function ensureServer(): Promise<{ server: Server; port: number }> {
	serverPromise ??= new Promise((resolve, reject) => {
		const server = createServer((request, response) => {
			void handleRequest(request, response);
		});
		server.on("upgrade", (request, socket, head) => {
			void handleUpgrade(request, socket, head);
		});
		server.once("error", reject);
		server.listen(0, "127.0.0.1", () => {
			server.off("error", reject);
			resolve({
				server,
				port: (server.address() as AddressInfo).port,
			});
		});
	});
	return serverPromise;
}

export async function registerGuestEngineProxy(input: {
	owner: string;
	upstreamEndpoint: string;
	upstreamToken?: string;
	namespace: string;
	pool: string;
	maxRequestBytes: number;
	maxResponseBytes: number;
}): Promise<GuestEngineProxyRegistration> {
	const existing = ownerPaths.get(input.owner);
	if (existing) capabilities.delete(existing);
	if (capabilities.size >= MAX_CAPABILITIES) {
		throw new RangeError(
			`agentOS Apps guest Engine proxy reached ${MAX_CAPABILITIES} capabilities; raise the host limit`,
		);
	}
	const { port } = await ensureServer();
	const path =
		randomUUID().replaceAll("-", "") + randomUUID().replaceAll("-", "");
	capabilities.set(path, {
		...input,
		path,
		activeRequests: 0,
		warned: false,
		actorIds: new Set(),
		sockets: new Set(),
	});
	ownerPaths.set(input.owner, path);
	return { endpoint: `http://127.0.0.1:${port}/${path}`, port };
}

export function unregisterGuestEngineProxy(owner: string): void {
	const path = ownerPaths.get(owner);
	if (!path) return;
	ownerPaths.delete(owner);
	for (const socket of capabilities.get(path)?.sockets ?? []) socket.destroy();
	capabilities.delete(path);
}
