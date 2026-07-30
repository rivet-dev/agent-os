import { afterEach, describe, expect, it, vi } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import {
	decodeAcpCallbackResponse,
	encodeAcpCallback,
} from "../src/sidecar/agentos-acp-protocol.js";
import { SidecarProcess } from "../src/sidecar/rpc-client.js";

const ACP_TEST_PERMISSIONS = {
	fs: "allow",
	childProcess: "allow",
} as const;
const ACP_EXTENSION_NAMESPACE = "dev.rivet.agent-os.acp";
const session = {
	connectionId: "conn-1",
	sessionId: "sidecar-session-1",
} as const;
const vm = {
	vmId: "vm-1",
} as const;

async function dispatchAcpRequest(
	agent: AgentOs,
	request: {
		id: number | string | null;
		method: string;
		params?: Record<string, unknown>;
	},
) {
	const runtime = agent as unknown as {
		_sidecarClient: SidecarProcess;
		_sidecarSession: { connectionId: string; sessionId: string };
		_sidecarVm: { vmId: string };
	};
	const client = (
		runtime._sidecarClient as unknown as {
			protocolClient: {
				protocolClient: {
					writeFrame: (frame: unknown) => Promise<void>;
					dispatchSidecarRequest: (request: unknown) => Promise<void>;
				};
			};
		}
	).protocolClient.protocolClient;
	let writtenFrame: {
		payload: {
			type: "ext_result";
			envelope: {
				namespace: string;
				payload: Uint8Array;
			};
		};
	} | null = null;
	const originalWriteFrame = client.writeFrame.bind(client);
	client.writeFrame = async (frame) => {
		const typedFrame = frame as {
			frame_type?: string;
		};
		if (typedFrame.frame_type === "sidecar_response") {
			writtenFrame = frame as typeof writtenFrame;
			return;
		}
		await originalWriteFrame(frame);
	};
	try {
		await client.dispatchSidecarRequest({
			frame_type: "sidecar_request",
			schema: { name: "agentos-sidecar", version: 8 },
			request_id: -101,
			ownership: {
				scope: "vm",
				connection_id: runtime._sidecarSession.connectionId,
				session_id: runtime._sidecarSession.sessionId,
				vm_id: runtime._sidecarVm.vmId,
			},
			payload: {
				type: "ext",
				envelope: {
					namespace: ACP_EXTENSION_NAMESPACE,
					payload: encodeAcpCallback({
						tag: "AcpHostRequestCallback",
						val: {
							sessionId: "acp-session-test",
							request: JSON.stringify({
								jsonrpc: "2.0",
								id: request.id,
								method: request.method,
								...(request.params ? { params: request.params } : {}),
							}),
						},
					}),
				},
			},
		});
	} finally {
		client.writeFrame = originalWriteFrame;
	}
	expect(writtenFrame).not.toBeNull();
	expect(writtenFrame?.payload.type).toBe("ext_result");
	expect(writtenFrame?.payload.envelope.namespace).toBe(
		ACP_EXTENSION_NAMESPACE,
	);
	const callbackResponse = decodeAcpCallbackResponse(
		writtenFrame!.payload.envelope.payload,
	);
	expect(callbackResponse.tag).toBe("AcpHostRequestCallbackResponse");
	if (callbackResponse.tag !== "AcpHostRequestCallbackResponse") {
		throw new Error("expected host request callback response");
	}
	expect(callbackResponse.val.response).not.toBeNull();
	return JSON.parse(callbackResponse.val.response ?? "null") as {
		jsonrpc: "2.0";
		id: number | string | null;
		result?: unknown;
		error?: {
			code: number;
			message: string;
			data?: Record<string, unknown>;
		};
	};
}

describe("AgentOs ACP host dispatcher integration", () => {
	let agent: AgentOs | null = null;

	afterEach(async () => {
		if (agent) {
			await agent.dispose();
			agent = null;
		}
	});

	it("round-trips fs/read through the installed ACP host dispatcher", async () => {
		agent = await AgentOs.create({
			permissions: ACP_TEST_PERMISSIONS,
		});
		await agent.writeFile("/workspace/notes.txt", "alpha\nbeta\ngamma\n");

		const response = await dispatchAcpRequest(agent, {
			id: 61,
			method: "fs/read",
			params: {
				path: "/workspace/notes.txt",
				line: 2,
				limit: 2,
			},
		});

		expect(response.error).toBeUndefined();
		expect(response.result).toEqual({
			content: "beta\ngamma",
		});
	});

	it("round-trips terminal/create and terminal/write through the installed ACP host dispatcher", async () => {
		agent = await AgentOs.create({
			permissions: ACP_TEST_PERMISSIONS,
		});

		const created = await dispatchAcpRequest(agent, {
			id: 71,
			method: "terminal/create",
			params: {
				command: "node",
				args: [
					"-e",
					"process.stdin.once('data', (chunk) => { process.stdout.write(chunk); process.exit(0); });",
				],
			},
		});
		expect(created.error).toBeUndefined();
		const terminalId = (created.result as { terminalId: string }).terminalId;
		expect(terminalId).toMatch(/^acp-terminal-/);

		const writeResult = await dispatchAcpRequest(agent, {
			id: 72,
			method: "terminal/write",
			params: {
				terminalId,
				data: "hello from acp\n",
			},
		});
		expect(writeResult.error).toBeUndefined();
		expect(writeResult.result).toBeNull();

		const waited = await dispatchAcpRequest(agent, {
			id: 73,
			method: "terminal/wait_for_exit",
			params: { terminalId },
		});
		expect(waited.error).toBeUndefined();
		expect(waited.result).toEqual({
			exitCode: 0,
			signal: null,
		});

		const output = await dispatchAcpRequest(agent, {
			id: 74,
			method: "terminal/output",
			params: { terminalId },
		});
		expect(output.error).toBeUndefined();
		expect(output.result).toEqual({
			output: "hello from acp\r\nhello from acp\n",
			truncated: false,
			exitStatus: {
				exitCode: 0,
				signal: null,
			},
		});
	});

	it("keeps genuinely unknown ACP host methods on -32601", async () => {
		agent = await AgentOs.create({
			permissions: ACP_TEST_PERMISSIONS,
		});

		const response = await dispatchAcpRequest(agent, {
			id: 81,
			method: "host/not-found",
		});

		expect(response.result).toBeUndefined();
		expect(response.error).toEqual({
			code: -32601,
			message: "Method not found: host/not-found",
			data: {
				method: "host/not-found",
			},
		});
	});
});
