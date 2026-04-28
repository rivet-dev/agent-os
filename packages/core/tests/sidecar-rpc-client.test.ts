import { afterEach, describe, expect, it, vi } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import { NativeSidecarProcessClient } from "../src/sidecar/rpc-client.js";

function mockClient(sendRequest: ReturnType<typeof vi.fn>) {
	const client = Object.create(
		NativeSidecarProcessClient.prototype,
	) as NativeSidecarProcessClient & {
		sendRequest: typeof sendRequest;
	};
	client.sendRequest = sendRequest;
	return client;
}

const session = {
	connectionId: "conn-1",
	sessionId: "sidecar-session-1",
} as const;

const vm = {
	vmId: "vm-1",
} as const;

const ACP_TEST_PERMISSIONS = {
	fs: "allow",
	childProcess: "allow",
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
		_sidecarClient: NativeSidecarProcessClient;
		_sidecarSession: { connectionId: string; sessionId: string };
		_sidecarVm: { vmId: string };
	};
	const client = runtime._sidecarClient as unknown as {
		writeFrame: (frame: unknown) => Promise<void>;
		dispatchSidecarRequest: (request: unknown) => Promise<void>;
	};
	let writtenFrame: {
		payload: {
			type: "acp_request_result";
			response: {
				jsonrpc: "2.0";
				id: number | string | null;
				result?: unknown;
				error?: {
					code: number;
					message: string;
					data?: Record<string, unknown>;
				};
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
			schema: { name: "agent-os-sidecar", version: 1 },
			request_id: -101,
			ownership: {
				scope: "vm",
				connection_id: runtime._sidecarSession.connectionId,
				session_id: runtime._sidecarSession.sessionId,
				vm_id: runtime._sidecarVm.vmId,
			},
			payload: {
				type: "acp_request",
				session_id: "acp-session-test",
				request: {
					jsonrpc: "2.0",
					id: request.id,
					method: request.method,
					...(request.params ? { params: request.params } : {}),
				},
			},
		});
	} finally {
		client.writeFrame = originalWriteFrame;
	}
	expect(writtenFrame).not.toBeNull();
	expect(writtenFrame?.payload.type).toBe("acp_request_result");
	return writtenFrame!.payload.response;
}

describe("NativeSidecarProcessClient ACP session state handling", () => {
	it("accepts fallback ACP notifications in session state snapshots", async () => {
		const client = mockClient(
			vi.fn().mockResolvedValue({
				payload: {
					type: "session_state",
					session_id: "acp-session-1",
					agent_type: "codex",
					process_id: "acp-proc-1",
					closed: false,
					config_options: [],
					events: [
						{
							sequence_number: 7,
							notification: {
								jsonrpc: "2.0",
								method: "agentos/acp_notification_serialization_failed",
								params: {
									error: "failed to serialize ACP notification: boom",
								},
							},
						},
					],
				},
			}),
		);

		const state = await client.getSessionState(session, vm, "acp-session-1");
		expect(state.events).toEqual([
			{
				sequenceNumber: 7,
				notification: {
					jsonrpc: "2.0",
					method: "agentos/acp_notification_serialization_failed",
					params: {
						error: "failed to serialize ACP notification: boom",
					},
				},
			},
		]);
	});

	it("forwards acknowledged sequence numbers in getSessionState requests", async () => {
		const sendRequest = vi.fn().mockResolvedValue({
			payload: {
				type: "session_state",
				session_id: "acp-session-1",
				agent_type: "codex",
				process_id: "acp-proc-1",
				closed: false,
				config_options: [],
				events: [],
			},
		});
		const client = mockClient(sendRequest);

		await client.getSessionState(session, vm, "acp-session-1", {
			acknowledgedSequenceNumber: 41,
		});

		expect(sendRequest).toHaveBeenCalledWith(
			expect.objectContaining({
				payload: expect.objectContaining({
					type: "get_session_state",
					session_id: "acp-session-1",
					acknowledged_sequence_number: 41,
				}),
			}),
		);
	});

	it("surfaces typed getSessionState rejections without poisoning later requests", async () => {
		const sendRequest = vi
			.fn()
			.mockRejectedValueOnce(
				new Error(
					"sidecar rejected request 1: invalid_state: failed to serialize ACP notification: boom",
				),
			)
			.mockResolvedValueOnce({
				payload: {
					type: "session_state",
					session_id: "acp-session-1",
					agent_type: "codex",
					process_id: "acp-proc-1",
					closed: false,
					config_options: [],
					events: [],
				},
			});
		const client = mockClient(sendRequest);

		await expect(
			client.getSessionState(session, vm, "acp-session-1"),
		).rejects.toThrow(
			"invalid_state: failed to serialize ACP notification: boom",
		);

		const recovered = await client.getSessionState(
			session,
			vm,
			"acp-session-1",
		);
		expect(recovered.processId).toBe("acp-proc-1");
		expect(sendRequest).toHaveBeenCalledTimes(2);
	});

	it("dispatches forwarded ACP sidecar requests back as sidecar responses", async () => {
		const writeFrame = vi.fn().mockResolvedValue(undefined);
		const rejectPending = vi.fn();
		const sidecarRequestHandler = vi.fn().mockResolvedValue({
			type: "acp_request_result",
			response: {
				jsonrpc: "2.0",
				id: 41,
				result: {
					content: "beta",
				},
			},
		});
		const client = Object.create(
			NativeSidecarProcessClient.prototype,
		) as NativeSidecarProcessClient & {
			sidecarRequestHandler: typeof sidecarRequestHandler;
			writeFrame: typeof writeFrame;
			rejectPending: typeof rejectPending;
		};
		client.sidecarRequestHandler = sidecarRequestHandler;
		client.writeFrame = writeFrame;
		client.rejectPending = rejectPending;

		await (
			client as unknown as {
				dispatchSidecarRequest: (request: unknown) => Promise<void>;
			}
		).dispatchSidecarRequest({
			frame_type: "sidecar_request",
			schema: { name: "agent-os-sidecar", version: 1 },
			request_id: -7,
			ownership: {
				scope: "vm",
				connection_id: "conn-1",
				session_id: "sidecar-session-1",
				vm_id: "vm-1",
			},
			payload: {
				type: "acp_request",
				session_id: "acp-session-1",
				request: {
					jsonrpc: "2.0",
					id: 41,
					method: "host/echo",
					params: { path: "/workspace/notes.txt" },
				},
			},
		});

		expect(sidecarRequestHandler).toHaveBeenCalledTimes(1);
		expect(writeFrame).toHaveBeenCalledWith({
			frame_type: "sidecar_response",
			schema: { name: "agent-os-sidecar", version: 1 },
			request_id: -7,
			ownership: {
				scope: "vm",
				connection_id: "conn-1",
				session_id: "sidecar-session-1",
				vm_id: "vm-1",
			},
			payload: {
				type: "acp_request_result",
				response: {
					jsonrpc: "2.0",
					id: 41,
					result: {
						content: "beta",
					},
				},
			},
		});
		expect(rejectPending).not.toHaveBeenCalled();
	});
});

describe("AgentOs ACP session event retention", () => {
	it("bounds mirrored session events and acknowledges the highest sequence on hydration", async () => {
		const getSessionState = vi
			.fn()
			.mockResolvedValueOnce({
				sessionId: "acp-session-1",
				agentType: "codex",
				processId: "acp-proc-1",
				closed: false,
				configOptions: [],
				events: Array.from({ length: 10_000 }, (_, index) => ({
					sequenceNumber: index,
					notification: {
						jsonrpc: "2.0" as const,
						method: "session/update",
						params: {
							update: {
								sessionUpdate: "agent_message_chunk",
								index,
							},
						},
					},
				})),
			})
			.mockResolvedValueOnce({
				sessionId: "acp-session-1",
				agentType: "codex",
				processId: "acp-proc-1",
				closed: false,
				configOptions: [],
				events: [
					{
						sequenceNumber: 10_000,
						notification: {
							jsonrpc: "2.0" as const,
							method: "session/update",
							params: {
								update: {
									sessionUpdate: "agent_message_chunk",
									index: 10_000,
								},
							},
						},
					},
				],
			});
		const agent = Object.create(AgentOs.prototype) as AgentOs & {
			_sidecarClient: {
				getSessionState: typeof getSessionState;
			};
			_sidecarSession: typeof session;
			_sidecarVm: typeof vm;
			_hydrateSessionState: (session: { sessionId: string }) => Promise<void>;
		};
		const trackedSession = {
			sessionId: "acp-session-1",
			agentType: "codex",
			processId: "",
			pid: null,
			closed: false,
			modes: null,
			configOptions: [],
			capabilities: {},
			agentInfo: null,
			highestSequenceNumber: null,
			events: [],
			eventHandlers: new Set(),
			permissionHandlers: new Set(),
			configOverrides: new Map(),
			pendingPermissionReplies: new Map(),
		};
		agent._sidecarClient = {
			getSessionState,
		};
		agent._sidecarSession = session;
		agent._sidecarVm = vm;

		await agent._hydrateSessionState(trackedSession);

		expect(trackedSession.events).toHaveLength(1024);
		expect(trackedSession.events[0]?.sequenceNumber).toBe(8_976);
		expect(trackedSession.events.at(-1)?.sequenceNumber).toBe(9_999);
		expect(trackedSession.highestSequenceNumber).toBe(9_999);

		await agent._hydrateSessionState(trackedSession);

		expect(getSessionState).toHaveBeenNthCalledWith(
			2,
			session,
			vm,
			"acp-session-1",
			{ acknowledgedSequenceNumber: 9_999 },
		);
		expect(trackedSession.events).toHaveLength(1024);
		expect(trackedSession.events.at(-1)?.sequenceNumber).toBe(10_000);
		expect(trackedSession.highestSequenceNumber).toBe(10_000);
	});
});

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
			output: "hello from acp\n",
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
