// Minimal real ACP agent for the native core round-trip test. It intentionally
// uses only stdio so the test exercises process launch, framing, session
// creation, and prompting without external services.

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
		return;
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
