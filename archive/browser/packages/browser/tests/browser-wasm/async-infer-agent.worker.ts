// An ASYNC ACP agent that answers a prompt using on-device inference
// (AGENTOS-WEB-ASYNC-AGENTS.md §6). On session/prompt it makes a SINGLE blocking
// `host.inference` syscall mid-turn: the kernel defers it to the main thread (the
// chrome-llm host-callback), the guest stays parked in its SAB shim, and the reply
// comes back as an OpenAI-shaped chat completion. The agent extracts the assistant
// message and echoes it as the prompt content.
//
// This stands in for the in-sandbox OpenAI proxy guest: it proves the async-inference
// transport (deferred syscall → host-callback → completion channel) end-to-end before
// the loopback-HTTP proxy + pi are layered on top. The model itself is reached ONLY
// through this kernel-brokered syscall — never as an ambient capability.

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

/** Blocking kernel syscall (the SAB shim). Returns the raw result bytes. */
function syscallRaw(operation: string, args: unknown[]): Uint8Array {
	return endpoint!.syscall(encoder.encode(JSON.stringify({ operation, args })));
}

/** Ask the on-device model (via the kernel-brokered host-callback) for a completion,
 * returning the assistant message text. The result bytes ARE the OpenAI chat
 * completion JSON the chrome-llm adapter produced. */
function infer(userText: string): string {
	const body = JSON.stringify({ messages: [{ role: "user", content: userText }] });
	const raw = syscallRaw("host.inference", [body]);
	const completion = JSON.parse(decoder.decode(raw)) as {
		choices?: { message?: { content?: string } }[];
		error?: { message?: string };
	};
	if (completion.error) return `ERR:${completion.error.message}`;
	return completion.choices?.[0]?.message?.content ?? "ERR:no-content";
}

async function handleLine(line: string): Promise<void> {
	const request = JSON.parse(line) as {
		id: number;
		method: string;
		params?: { protocolVersion?: number; prompt?: { text?: string }[] };
	};
	await Promise.resolve(); // genuinely async (stands in for the awaited turn)
	const { id, method, params } = request;
	let body: Record<string, unknown>;
	switch (method) {
		case "initialize":
			body = {
				result: {
					protocolVersion: params?.protocolVersion ?? 1,
					agentInfo: { name: "async-infer", version: "0.0.0" },
					agentCapabilities: {},
				},
			};
			break;
		case "session/new":
			body = { result: { sessionId: "async-infer-session" } };
			break;
		case "session/prompt": {
			const userText = params?.prompt?.[0]?.text ?? "ping";
			const answer = infer(userText);
			body = { result: { stopReason: "end_turn", content: answer } };
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
