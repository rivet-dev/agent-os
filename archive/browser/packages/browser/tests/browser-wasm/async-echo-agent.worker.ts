// An ASYNC ACP echo agent running in an execution worker (AGENTOS-WEB-ASYNC-
// AGENTS.md §3.2). Unlike the synchronous in-process echo agent, this one runs in
// its own Worker and replies ASYNCHRONOUSLY (it awaits a microtask before
// responding) — exactly the shape a real agent has while awaiting an LLM. Its
// stdio uses the §3.2 split: stdin arrives via postMessage (event-loop friendly,
// so the agent could await the model), and stdout is written to the SAB
// up-channel via SabExecutionEndpoint (so the blocked kernel reactor is woken).

import { SabExecutionEndpoint, type SabRingLayout } from "@rivet-dev/agentos-runtime-browser";

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

/** Make a synchronous kernel syscall mid-turn (the blocking SAB shim). Returns the
 * decoded ConvergedSyncResponse ({kind, value?}) or {error}. */
function syscall(operation: string, args: unknown[]): { kind?: number; value?: unknown; error?: string } {
	const response = endpoint!.syscall(encoder.encode(JSON.stringify({ operation, args })));
	return JSON.parse(decoder.decode(response));
}

async function handleLine(line: string): Promise<void> {
	const request = JSON.parse(line) as {
		id: number;
		method: string;
		params?: { protocolVersion?: number };
	};
	// Genuinely async: yield to the event loop before replying (stands in for an
	// awaited LLM call). With the OLD synchronous AcpCore this could never be
	// delivered (the kernel would be blocked inside pushFrame); the resumable path
	// makes it work.
	await Promise.resolve();
	const { id, method, params } = request;
	let body: Record<string, unknown>;
	switch (method) {
		case "initialize":
			body = {
				result: {
					protocolVersion: params?.protocolVersion ?? 1,
					agentInfo: { name: "async-echo", version: "0.0.0" },
					agentCapabilities: {},
				},
			};
			break;
		case "session/new":
			body = { result: { sessionId: "async-echo-session" } };
			break;
		case "session/prompt": {
			// HARDENED: make kernel syscalls MID-TURN (the case that re-enters
			// pushFrame under the synchronous path). The reactor services these inline
			// (non-nested) while the kernel worker drives this prompt. Echo the file
			// content back so the test can prove the syscall round-trip happened
			// during the turn.
			syscall("fs.mkdir", ["/work"]);
			syscall("fs.writeFile", ["/work/turn.txt", "hello-from-mid-turn-syscall"]);
			const read = syscall("fs.readFile", ["/work/turn.txt"]);
			body = { result: { stopReason: "end_turn", content: read.value ?? `ERR:${read.error}` } };
			break;
		}
		default:
			body = { error: { code: -32601, message: `method not found: ${method}` } };
	}
	const response = `${JSON.stringify({ jsonrpc: "2.0", id, ...body })}\n`;
	endpoint!.writeStdout(encoder.encode(response));
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
