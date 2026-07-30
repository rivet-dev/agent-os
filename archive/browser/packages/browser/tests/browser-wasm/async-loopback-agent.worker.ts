// A net-loopback probe agent (AGENTOS-WEB-ASYNC-AGENTS.md §6 groundwork). On
// session/prompt it drives a full loopback TCP handshake through the kernel socket
// table — listen, connect, accept, write, read on 127.0.0.1 — entirely via mid-turn
// `net.*` syscalls, and echoes the bytes it read back. This proves the converged
// net.* sync-bridge ops route through the async-agent executor's pushFrame path (the
// same path the echo agent proved for fs.*), which the in-sandbox HTTP proxy guest is
// then built on. Binary `data` args ride the syscall channel via the $u8 tag.

import { SabExecutionEndpoint, type SabRingLayout } from "@rivet-dev/agentos-runtime-browser";
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

const PORT = 39556;

/** Blocking kernel syscall (the SAB shim). Returns the ConvergedSyncResponse value
 * ({socketId} / {data:base64} / ...), or throws on a sync-bridge error. */
function syscall(operation: string, arg: unknown): Record<string, unknown> {
	const raw = endpoint!.syscall(encodeSyscall(operation, [arg]));
	const response = JSON.parse(decoder.decode(raw)) as {
		kind?: number;
		value?: Record<string, unknown>;
		error?: string;
	};
	if (response.error) throw new Error(`${operation}: ${response.error}`);
	return response.value ?? {};
}

function decodeBase64(base64: string): Uint8Array {
	const binary = atob(base64);
	const out = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
	return out;
}

/** Full loopback handshake through the kernel socket table, returning the received
 * bytes as text. Mirrors the proven convwasi runNetLoopback, but driven entirely from
 * inside the async-agent executor over the SAB syscall channel. */
function runLoopback(message: string): string {
	const listener = syscall("net.listen", { host: "127.0.0.1", port: PORT });
	const client = syscall("net.connect", { host: "127.0.0.1", port: PORT });
	const accepted = syscall("net.accept", { socketId: listener.socketId });
	syscall("net.write", { socketId: client.socketId, data: encoder.encode(message) });
	const read = syscall("net.read", { socketId: accepted.socketId });
	const received = typeof read.data === "string" ? decoder.decode(decodeBase64(read.data)) : "";
	syscall("net.close", { socketId: client.socketId });
	syscall("net.close", { socketId: accepted.socketId });
	syscall("net.close", { socketId: listener.socketId });
	return received;
}

async function handleLine(line: string): Promise<void> {
	const request = JSON.parse(line) as { id: number; method: string; params?: { protocolVersion?: number } };
	await Promise.resolve();
	const { id, method, params } = request;
	let body: Record<string, unknown>;
	switch (method) {
		case "initialize":
			body = {
				result: {
					protocolVersion: params?.protocolVersion ?? 1,
					agentInfo: { name: "async-loopback", version: "0.0.0" },
					agentCapabilities: {},
				},
			};
			break;
		case "session/new":
			body = { result: { sessionId: "async-loopback-session" } };
			break;
		case "session/prompt": {
			let content: string;
			try {
				content = runLoopback("ping-loopback");
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
