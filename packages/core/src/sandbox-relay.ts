import { randomBytes, timingSafeEqual } from "node:crypto";
import { once } from "node:events";
import {
	createServer,
	type IncomingHttpHeaders,
	type Server,
	type ServerResponse,
} from "node:http";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import type { ReadableStream as NodeReadableStream } from "node:stream/web";
import type { AgentOsSandboxClient } from "./sandbox.js";

const DEFAULT_MAX_RELAY_REQUESTS = 64;
const RELAY_WARNING_PERCENT = 80;
const HOP_BY_HOP_HEADERS = new Set([
	"connection",
	"keep-alive",
	"proxy-authenticate",
	"proxy-authorization",
	"proxy-connection",
	"te",
	"trailer",
	"transfer-encoding",
	"upgrade",
]);
const ALLOWED_RELAY_ROUTES = new Map<string, ReadonlySet<string>>([
	["/v1/fs/entries", new Set(["GET"])],
	["/v1/fs/file", new Set(["GET", "PUT"])],
	["/v1/fs/entry", new Set(["DELETE"])],
	["/v1/fs/mkdir", new Set(["POST"])],
	["/v1/fs/move", new Set(["POST"])],
	["/v1/fs/stat", new Set(["GET"])],
	["/v1/processes/run", new Set(["POST"])],
]);

export interface SandboxRelayClientController {
	withClient<T>(
		operation: (client: AgentOsSandboxClient) => Promise<T>,
	): Promise<T>;
}

export interface SandboxRelayOptions {
	controller: SandboxRelayClientController;
	maxConcurrentRequests?: number;
}

export interface SandboxRelay {
	baseUrl: string;
	token: string;
	dispose(): Promise<void>;
}

interface SerializableSandboxClient {
	baseUrl?: string;
	token?: string;
	defaultHeaders?: RequestInit["headers"];
}

type RelayRequestInit = RequestInit & { duplex?: "half" };

function problem(
	response: ServerResponse,
	status: number,
	title: string,
	detail: string,
): void {
	if (response.headersSent) {
		response.destroy(new Error(detail));
		return;
	}
	const body = Buffer.from(
		JSON.stringify({
			type: "about:blank",
			title,
			status,
			detail,
		}),
	);
	response.writeHead(status, {
		"content-length": String(body.length),
		"content-type": "application/problem+json",
	});
	response.end(body);
}

function bearerToken(headers: IncomingHttpHeaders): string | undefined {
	const authorization = headers.authorization;
	if (!authorization?.startsWith("Bearer ")) return undefined;
	return authorization.slice("Bearer ".length);
}

function tokenMatches(actual: string | undefined, expected: string): boolean {
	if (!actual) return false;
	const actualBytes = Buffer.from(actual);
	const expectedBytes = Buffer.from(expected);
	return (
		actualBytes.length === expectedBytes.length &&
		timingSafeEqual(actualBytes, expectedBytes)
	);
}

function isAllowedRelayRoute(method: string, pathname: string): boolean {
	return ALLOWED_RELAY_ROUTES.get(pathname)?.has(method) === true;
}

function copyRequestHeaders(headers: IncomingHttpHeaders): Headers {
	const copied = new Headers();
	for (const [name, value] of Object.entries(headers)) {
		const lower = name.toLowerCase();
		if (
			value === undefined ||
			lower === "accept-encoding" ||
			lower === "authorization" ||
			lower === "host" ||
			HOP_BY_HOP_HEADERS.has(lower)
		) {
			continue;
		}
		if (Array.isArray(value)) {
			for (const item of value) copied.append(name, item);
		} else {
			copied.set(name, value);
		}
	}
	copied.set("accept-encoding", "identity");
	return copied;
}

function validateUpstreamBaseUrl(raw: string): string {
	const normalized = raw.trim().replace(/\/+$/, "");
	if (!normalized) throw new Error("Sandbox client baseUrl must not be empty");
	const url = new URL(normalized);
	if (!url.hostname || url.search || url.hash) {
		throw new Error(
			"Sandbox client baseUrl must include a host without a query string or fragment",
		);
	}
	if (url.protocol !== "http:" && url.protocol !== "https:") {
		throw new Error("Sandbox client baseUrl must use http or https");
	}
	const hostname = url.hostname
		.replace(/^\[/, "")
		.replace(/\]$/, "")
		.toLowerCase();
	const loopback =
		hostname === "localhost" ||
		hostname === "::1" ||
		/^127(?:\.|$)/.test(hostname);
	if (url.protocol !== "https:" && !loopback) {
		throw new Error(
			"Sandbox client baseUrl must use https unless it targets localhost",
		);
	}
	return normalized;
}

function mergeUpstreamHeaders(
	client: AgentOsSandboxClient,
	requestHeaders: Headers,
): Headers {
	const serializable = client as AgentOsSandboxClient &
		SerializableSandboxClient;
	const headers = new Headers(serializable.defaultHeaders);
	requestHeaders.forEach((value, name) => {
		headers.set(name, value);
	});
	if (serializable.token) {
		headers.set("authorization", `Bearer ${serializable.token}`);
	}
	return headers;
}

async function requestUpstream(
	client: AgentOsSandboxClient,
	path: string,
	init: RelayRequestInit,
): Promise<Response> {
	const headers = mergeUpstreamHeaders(client, new Headers(init.headers));
	const upstreamInit: RelayRequestInit = {
		...init,
		headers,
		redirect: "manual",
	};
	if (client.request) {
		return await client.request(path, upstreamInit);
	}

	const serializable = client as AgentOsSandboxClient &
		SerializableSandboxClient;
	const rawBaseUrl = serializable.baseUrl;
	if (!rawBaseUrl) {
		throw new Error(
			"Sandbox client does not expose request() or a serializable baseUrl",
		);
	}
	const baseUrl = validateUpstreamBaseUrl(rawBaseUrl);
	return await fetch(`${baseUrl}${path}`, upstreamInit);
}

function copyResponseHeaders(headers: Headers): Record<string, string> {
	const copied: Record<string, string> = {};
	headers.forEach((value, name) => {
		if (!HOP_BY_HOP_HEADERS.has(name.toLowerCase())) copied[name] = value;
	});
	return copied;
}

async function writeUpstreamResponse(
	upstream: Response,
	response: ServerResponse,
): Promise<void> {
	response.writeHead(upstream.status, copyResponseHeaders(upstream.headers));
	if (!upstream.body) {
		response.end();
		return;
	}
	const body = Readable.fromWeb(
		upstream.body as unknown as NodeReadableStream<Uint8Array>,
	);
	await pipeline(body, response);
}

function closeServer(server: Server): Promise<void> {
	return new Promise((resolve, reject) => {
		server.close((error) => {
			if (error) reject(error);
			else resolve();
		});
		server.closeIdleConnections();
		server.closeAllConnections();
	});
}

export async function createSandboxRelay(
	options: SandboxRelayOptions,
): Promise<SandboxRelay> {
	const token = randomBytes(32).toString("base64url");
	const maxConcurrentRequests =
		options.maxConcurrentRequests ?? DEFAULT_MAX_RELAY_REQUESTS;
	if (
		!Number.isSafeInteger(maxConcurrentRequests) ||
		maxConcurrentRequests <= 0
	) {
		throw new Error("sandbox.maxRelayRequests must be a positive safe integer");
	}

	let activeRequests = 0;
	let warnedNearCapacity = false;
	let disposed = false;
	const server = createServer((request, response) => {
		void (async () => {
			if (disposed) {
				problem(
					response,
					503,
					"Sandbox relay unavailable",
					"agentOS VM sandbox relay is shutting down",
				);
				return;
			}
			if (!tokenMatches(bearerToken(request.headers), token)) {
				problem(response, 401, "Unauthorized", "Invalid sandbox relay token");
				return;
			}

			const url = new URL(request.url ?? "/", "http://127.0.0.1");
			const method = request.method ?? "GET";
			if (!isAllowedRelayRoute(method, url.pathname)) {
				problem(
					response,
					404,
					"Not Found",
					`Sandbox relay does not expose ${method} ${url.pathname}`,
				);
				return;
			}
			if (activeRequests >= maxConcurrentRequests) {
				problem(
					response,
					429,
					"Sandbox relay capacity exceeded",
					`Sandbox relay reached sandbox.maxRelayRequests=${maxConcurrentRequests}; raise sandbox.maxRelayRequests to allow more concurrent requests`,
				);
				return;
			}

			activeRequests += 1;
			if (
				!warnedNearCapacity &&
				activeRequests * 100 >= maxConcurrentRequests * RELAY_WARNING_PERCENT
			) {
				warnedNearCapacity = true;
				console.warn(
					`agentOS sandbox relay near sandbox.maxRelayRequests: ${activeRequests}/${maxConcurrentRequests}`,
				);
			}
			try {
				await options.controller.withClient(async (client) => {
					const abortController = new AbortController();
					const abort = () => {
						if (!response.writableEnded) abortController.abort();
					};
					request.once("aborted", abort);
					response.once("close", abort);
					try {
						const hasBody = method !== "GET" && method !== "HEAD";
						const init: RelayRequestInit = {
							method,
							headers: copyRequestHeaders(request.headers),
							redirect: "manual",
							signal: abortController.signal,
							...(hasBody
								? {
										body: Readable.toWeb(request) as never,
										duplex: "half" as const,
									}
								: {}),
						};
						const upstream = await requestUpstream(
							client,
							`${url.pathname}${url.search}`,
							init,
						);
						await writeUpstreamResponse(upstream, response);
					} finally {
						request.off("aborted", abort);
						response.off("close", abort);
					}
				});
			} catch (error) {
				const detail = error instanceof Error ? error.message : String(error);
				problem(response, 503, "Sandbox unavailable", detail);
			} finally {
				activeRequests -= 1;
				if (
					activeRequests * 100 <
					maxConcurrentRequests * RELAY_WARNING_PERCENT
				) {
					warnedNearCapacity = false;
				}
			}
		})().catch((error) => {
			console.error("agentOS sandbox relay request failed", error);
			problem(
				response,
				500,
				"Sandbox relay failure",
				error instanceof Error ? error.message : String(error),
			);
		});
	});
	server.maxConnections = Math.min(
		Number.MAX_SAFE_INTEGER,
		maxConcurrentRequests + 1,
	);
	server.listen(0, "127.0.0.1");
	try {
		await Promise.race([
			once(server, "listening"),
			once(server, "error").then(([error]) => Promise.reject(error)),
		]);
	} catch (error) {
		await closeServer(server).catch((closeError) => {
			console.error("agentOS sandbox relay cleanup failed", closeError);
		});
		throw error;
	}
	server.unref();

	const address = server.address();
	if (!address || typeof address === "string") {
		await closeServer(server);
		throw new Error("Sandbox relay failed to bind to a TCP port");
	}

	return {
		baseUrl: `http://127.0.0.1:${address.port}`,
		token,
		async dispose() {
			if (disposed) return;
			disposed = true;
			await closeServer(server);
		},
	};
}
