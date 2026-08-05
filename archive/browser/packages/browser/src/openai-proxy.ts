// The in-sandbox OpenAI proxy logic (AGENTOS-WEB-ASYNC-AGENTS.md §6). pi (and any
// OpenAI/Anthropic HTTP client) talks to its `baseUrl` over plain loopback HTTP; a
// guest proxy listening on that port forwards the request body to on-device inference
// through the kernel-brokered `host.inference` syscall and returns the reply as an
// HTTP response. pi needs ZERO changes beyond the baseUrl.
//
// This module is the PURE framing + forwarding logic, free of any socket/syscall
// dependency (those are injected), so it is unit-testable in Node and reusable by both
// the test proxy agent and the eventual standalone proxy guest. The proxy does not
// transform the chat body — it is HTTP framing around the inference call: the request
// body IS the chat-completion JSON the chrome-llm adapter consumes, and that adapter's
// reply IS the HTTP response body.

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export interface HttpRequest {
	method: string;
	path: string;
	headers: Map<string, string>;
	body: string;
}

export interface HttpResponse {
	status: number;
	headers: Map<string, string>;
	body: string;
}

/** Build a minimal HTTP/1.1 request (the client/pi side). */
export function buildHttpRequest(method: string, path: string, body: string, host = "127.0.0.1"): Uint8Array {
	const bodyBytes = encoder.encode(body);
	const head =
		`${method} ${path} HTTP/1.1\r\n` +
		`Host: ${host}\r\n` +
		`Content-Type: application/json\r\n` +
		`Content-Length: ${bodyBytes.byteLength}\r\n` +
		`Connection: close\r\n\r\n`;
	return concat(encoder.encode(head), bodyBytes);
}

/** Build a minimal HTTP/1.1 response (the proxy side). */
export function buildHttpResponse(status: number, body: string): Uint8Array {
	const bodyBytes = encoder.encode(body);
	const reason = status === 200 ? "OK" : status === 400 ? "Bad Request" : "Error";
	const head =
		`HTTP/1.1 ${status} ${reason}\r\n` +
		`Content-Type: application/json\r\n` +
		`Content-Length: ${bodyBytes.byteLength}\r\n` +
		`Connection: close\r\n\r\n`;
	return concat(encoder.encode(head), bodyBytes);
}

/** Index of the end of the header block (after the CRLFCRLF), or -1 if not yet present. */
function headerEnd(bytes: Uint8Array): number {
	for (let i = 3; i < bytes.length; i += 1) {
		if (bytes[i - 3] === 13 && bytes[i - 2] === 10 && bytes[i - 1] === 13 && bytes[i] === 10) {
			return i + 1;
		}
	}
	return -1;
}

function parseHeaders(headerText: string): { startLine: string; headers: Map<string, string> } {
	const lines = headerText.split("\r\n").filter((l) => l.length > 0);
	const startLine = lines.shift() ?? "";
	const headers = new Map<string, string>();
	for (const line of lines) {
		const colon = line.indexOf(":");
		if (colon > 0) headers.set(line.slice(0, colon).trim().toLowerCase(), line.slice(colon + 1).trim());
	}
	return { startLine, headers };
}

function contentLength(headers: Map<string, string>): number {
	const raw = headers.get("content-length");
	const n = raw ? Number.parseInt(raw, 10) : 0;
	return Number.isFinite(n) && n >= 0 ? n : 0;
}

/** Read a full HTTP message (headers + Content-Length body) by pulling chunks from
 * `readChunk` until complete. `readChunk()` returns the next bytes (possibly empty if
 * not ready yet) or null on EOF. Returns the assembled bytes + the body offset. */
function readFullMessage(initial: Uint8Array, readChunk: () => Uint8Array | null): { bytes: Uint8Array; bodyStart: number } | null {
	let buf = initial;
	for (let guard = 0; guard < 100_000; guard += 1) {
		const bodyStart = headerEnd(buf);
		if (bodyStart >= 0) {
			const { headers } = parseHeaders(decoder.decode(buf.subarray(0, bodyStart)));
			const need = bodyStart + contentLength(headers);
			if (buf.byteLength >= need) return { bytes: buf.subarray(0, need), bodyStart };
		}
		const chunk = readChunk();
		if (chunk === null) return bodyStart >= 0 ? { bytes: buf, bodyStart } : null;
		if (chunk.byteLength > 0) buf = concat(buf, chunk);
	}
	return null;
}

/** Parse a complete HTTP request, reading more via `readChunk` until the body is in. */
export function readHttpRequest(initial: Uint8Array, readChunk: () => Uint8Array | null): HttpRequest | null {
	const message = readFullMessage(initial, readChunk);
	if (!message) return null;
	const { startLine, headers } = parseHeaders(decoder.decode(message.bytes.subarray(0, message.bodyStart)));
	const [method = "", path = ""] = startLine.split(" ");
	return { method, path, headers, body: decoder.decode(message.bytes.subarray(message.bodyStart)) };
}

/** Parse a complete HTTP response (the client/pi side). */
export function readHttpResponse(initial: Uint8Array, readChunk: () => Uint8Array | null): HttpResponse | null {
	const message = readFullMessage(initial, readChunk);
	if (!message) return null;
	const { startLine, headers } = parseHeaders(decoder.decode(message.bytes.subarray(0, message.bodyStart)));
	const status = Number.parseInt(startLine.split(" ")[1] ?? "0", 10) || 0;
	return { status, headers, body: decoder.decode(message.bytes.subarray(message.bodyStart)) };
}

/** The proxy core: turn one parsed HTTP request into an HTTP response by forwarding
 * the body to `infer` (the host.inference call). Only POSTs to a chat/completions or
 * /v1/messages path are forwarded; anything else is a 404-shaped error. */
export async function handleProxyRequest(request: HttpRequest, infer: (body: string) => Promise<string> | string): Promise<Uint8Array> {
	const isChat = request.method === "POST" && /\/(chat\/completions|messages|completions)$/.test(request.path);
	if (!isChat) {
		return buildHttpResponse(404, JSON.stringify({ error: { type: "not_found", message: `no handler for ${request.method} ${request.path}` } }));
	}
	const reply = await infer(request.body);
	return buildHttpResponse(200, reply);
}

function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
	const out = new Uint8Array(a.byteLength + b.byteLength);
	out.set(a, 0);
	out.set(b, a.byteLength);
	return out;
}
