// Minimal ACP echo agent: a real guest program that speaks ACP JSON-RPC over
// stdin/stdout, used by the browser ACP round-trip test. It uses ONLY stdin/stdout
// (no fs/net/process), so it emits no guest kernel calls and needs no GuestRequest
// servicing in the browser AcpHost — exactly the minimal-round-trip path the spec
// describes. It implements the handshake `agentos-sidecar-core::AcpCore` drives:
// `initialize` -> `session/new` (-> optional `session/prompt`).

import { createInterface } from "node:readline";

function send(message) {
	process.stdout.write(`${JSON.stringify(message)}\n`);
}

const rl = createInterface({ input: process.stdin });
rl.on("line", (raw) => {
	const line = raw.trim();
	if (!line) return;
	let request;
	try {
		request = JSON.parse(line);
	} catch {
		return; // ignore non-JSON lines
	}
	const { id, method, params } = request;
	switch (method) {
		case "initialize":
			send({
				jsonrpc: "2.0",
				id,
				result: {
					protocolVersion: params?.protocolVersion ?? 1,
					agentInfo: { name: "echo", version: "0.0.0" },
					agentCapabilities: {},
				},
			});
			break;
		case "session/new":
			send({ jsonrpc: "2.0", id, result: { sessionId: "echo-session-1" } });
			break;
		case "session/prompt":
			// Echo a single assistant turn then end.
			send({ jsonrpc: "2.0", id, result: { stopReason: "end_turn" } });
			break;
		default:
			send({
				jsonrpc: "2.0",
				id,
				error: { code: -32601, message: `method not found: ${method}` },
			});
	}
});
