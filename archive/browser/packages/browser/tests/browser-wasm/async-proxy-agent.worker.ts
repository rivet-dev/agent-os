// The in-sandbox OpenAI proxy, end-to-end over loopback (AGENTOS-WEB-ASYNC-AGENTS.md
// §6). On session/prompt this agent stands up BOTH sides of the pi↔proxy path in one
// turn, to prove the full shape with a single executor:
//   - a "pi" HTTP client connects to the loopback proxy port and POSTs an
//     OpenAI chat-completions request;
//   - the proxy accepts, reads the HTTP request, forwards the body to on-device
//     inference via the kernel-brokered `host.inference` syscall (the chrome-llm
//     host-callback), and writes the reply back as an HTTP 200;
//   - the client reads the HTTP response and extracts the assistant message.
// All socket I/O is synchronous net.* sync-bridge ops through the kernel socket table;
// the inference call is the one async hop (DEFERRED → completion channel). This is the
// exact path pi will drive, minus pi itself running as a separate guest (the next
// layer). The proxy framing logic is the shared, unit-tested openai-proxy module.

import { SabExecutionEndpoint, type SabRingLayout } from "@rivet-dev/agentos-runtime-browser";
import {
	buildHttpRequest,
	handleProxyRequest,
	readHttpRequest,
	readHttpResponse,
} from "../../src/openai-proxy.js";
import { encodeSyscall } from "./syscall-codec.js";

interface InitMessage {
	type: "init";
	upSab: SharedArrayBuffer;
	downSab: SharedArrayBuffer;
	controlSab: SharedArrayBuffer;
	layout: SabRingLayout;
}
interface StdinMessage {
	type: "stdin";
	chunk: Uint8Array;
}

let endpoint: SabExecutionEndpoint | null = null;
let buffer = "";
const decoder = new TextDecoder();
const encoder = new TextEncoder();
const PORT = 8088;
const EMPTY = new Uint8Array(0);

/** Net sync-bridge call → ConvergedSyncResponse value. Throws on a bridge error. */
function net(operation: string, arg: unknown): Record<string, unknown> {
	const raw = endpoint!.syscall(encodeSyscall(operation, [arg]));
	const response = JSON.parse(decoder.decode(raw)) as { value?: Record<string, unknown>; error?: string };
	if (response.error) throw new Error(`${operation}: ${response.error}`);
	return response.value ?? {};
}

function decodeBase64(base64: string): Uint8Array {
	const binary = atob(base64);
	const out = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
	return out;
}

/** A blocking read of the next chunk from a stream socket: returns bytes, or null on
 * EOF. Spins through net.poll while the socket would-block (the kernel clamps each
 * poll wait, so this loop yields the turn back to other work via the reactor). */
function readChunk(socketId: number): Uint8Array | null {
	for (let i = 0; i < 10_000; i += 1) {
		const r = net("net.read", { socketId });
		if (typeof r.data === "string") return decodeBase64(r.data);
		if (r.closed === true) return null;
		net("net.poll", { socketId, timeoutMs: 1000 });
	}
	return null;
}

/** The on-device inference host-callback (the one async hop): returns the OpenAI chat
 * completion JSON the chrome-llm adapter produced. */
function hostInference(body: string): string {
	return decoder.decode(endpoint!.syscall(encodeSyscall("host.inference", [body])));
}

/** Run the whole pi↔proxy↔inference loopback round-trip, returning the assistant text. */
async function runProxyRoundTrip(userText: string): Promise<string> {
	const listener = net("net.listen", { host: "127.0.0.1", port: PORT });
	const client = net("net.connect", { host: "127.0.0.1", port: PORT });
	const clientId = client.socketId as number;
	const listenerId = listener.socketId as number;

	// "pi" side: POST an OpenAI chat-completions request to the proxy.
	const chatBody = JSON.stringify({ model: "chrome-local", messages: [{ role: "user", content: userText }] });
	net("net.write", { socketId: clientId, data: buildHttpRequest("POST", "/v1/chat/completions", chatBody) });

	// Proxy side: accept, read the request, forward to inference, write the response.
	const accepted = net("net.accept", { socketId: listenerId });
	const acceptedId = accepted.socketId as number;
	const request = readHttpRequest(EMPTY, () => readChunk(acceptedId));
	if (!request) throw new Error("proxy: incomplete HTTP request");
	const responseBytes = await handleProxyRequest(request, hostInference);
	net("net.write", { socketId: acceptedId, data: responseBytes });
	net("net.shutdown", { socketId: acceptedId, how: "write" });

	// "pi" side: read the HTTP response, extract the assistant message.
	const response = readHttpResponse(EMPTY, () => readChunk(clientId));
	if (!response) throw new Error("proxy: incomplete HTTP response");
	const completion = JSON.parse(response.body) as {
		choices?: { message?: { content?: string } }[];
		error?: { message?: string };
	};
	net("net.close", { socketId: clientId });
	net("net.close", { socketId: acceptedId });
	net("net.close", { socketId: listenerId });
	if (completion.error) return `ERR:${completion.error.message}`;
	return completion.choices?.[0]?.message?.content ?? "ERR:no-content";
}

async function handleLine(line: string): Promise<void> {
	const request = JSON.parse(line) as {
		id: number;
		method: string;
		params?: { protocolVersion?: number; prompt?: { text?: string }[] };
	};
	await Promise.resolve();
	const { id, method, params } = request;
	let body: Record<string, unknown>;
	switch (method) {
		case "initialize":
			body = {
				result: {
					protocolVersion: params?.protocolVersion ?? 1,
					agentInfo: { name: "async-proxy", version: "0.0.0" },
					agentCapabilities: {},
				},
			};
			break;
		case "session/new":
			body = { result: { sessionId: "async-proxy-session" } };
			break;
		case "session/prompt": {
			const userText = params?.prompt?.[0]?.text ?? "ping";
			let content: string;
			try {
				content = await runProxyRoundTrip(userText);
			} catch (error) {
				content = `ERR:${error instanceof Error ? error.message : String(error)}`;
			}
			body = { result: { stopReason: "end_turn", content } };
			break;
		}
		default:
			body = { error: { code: -32601, message: `method not found: ${method}` } };
	}
	endpoint!.writeStdout(encoder.encode(`${JSON.stringify({ jsonrpc: "2.0", id, ...body })}\n`));
}

self.onmessage = (event: MessageEvent<InitMessage | StdinMessage>) => {
	const message = event.data;
	if (message.type === "init") {
		endpoint = new SabExecutionEndpoint({
			upSab: message.upSab,
			downSab: message.downSab,
			controlSab: message.controlSab,
			layout: message.layout,
		});
		return;
	}
	if (message.type === "stdin" && endpoint) {
		buffer += decoder.decode(message.chunk);
		let newline = buffer.indexOf("\n");
		while (newline >= 0) {
			const line = buffer.slice(0, newline).trim();
			buffer = buffer.slice(newline + 1);
			if (line) void handleLine(line);
			newline = buffer.indexOf("\n");
		}
	}
};
