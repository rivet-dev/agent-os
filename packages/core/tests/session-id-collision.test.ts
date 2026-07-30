import { afterEach, describe, expect, test, vi } from "vitest";
import { AgentOs } from "../src/index.js";
import { encodeAcpEvent } from "../src/sidecar/agentos-acp-protocol.js";

const ACP_EXTENSION_NAMESPACE = "dev.rivet.agent-os.acp";

function storedSession(sessionId: string) {
	return {
		sessionId,
		agent: "mock",
		cwd: "/workspace",
		additionalDirectories: "[]",
		state: JSON.stringify({ status: "idle" }),
		latestSequence: 0n,
		title: null,
		metadata: null,
		createdAt: "2026-01-01T00:00:00.000Z",
		updatedAt: "2026-01-01T00:00:00.000Z",
	};
}

describe("public/native session identity isolation", () => {
	let vm: AgentOs | null = null;

	afterEach(async () => {
		await vm?.dispose();
		vm = null;
	});

	test("public subscriptions remain isolated without a client-owned adapter-id map", async () => {
		vm = await AgentOs.create({});
		const internal = vm as unknown as {
			_sendAcpRequest(request: unknown): Promise<unknown>;
		};
		const send = vi
			.spyOn(internal, "_sendAcpRequest")
			.mockImplementation(
				async (request: { tag: string; val?: { sessionId?: string } }) => {
					if (request.tag !== "AcpOpenSessionRequest") {
						throw new Error(`unexpected request ${request.tag}`);
					}
					const sessionId = request.val?.sessionId ?? "main";
					return {
						tag: "AcpOpenSessionResponse",
						val: { session: storedSession(sessionId) },
					} as never;
				},
			);

		expect(
			await vm.openSession({
				sessionId: "public-a",
				agent: "mock",
			}),
		).toBeUndefined();
		expect(
			await vm.openSession({
				sessionId: "public-b",
				agent: "mock",
			}),
		).toBeUndefined();
		expect(await vm.openSession({ agent: "mock" })).toBeUndefined();
		expect(send).toHaveBeenCalledTimes(3);
		expect(send).toHaveBeenNthCalledWith(
			1,
			expect.objectContaining({
				tag: "AcpOpenSessionRequest",
				val: expect.objectContaining({ sessionId: "public-a" }),
			}),
		);
		expect(send).toHaveBeenNthCalledWith(
			3,
			expect.objectContaining({
				tag: "AcpOpenSessionRequest",
				val: expect.objectContaining({ sessionId: null }),
			}),
		);

		const firstEvents: unknown[] = [];
		const secondEvents: unknown[] = [];
		vm.onSessionEvent("public-a", (event) => firstEvents.push(event));
		vm.onSessionEvent("public-b", (event) => secondEvents.push(event));

		for (const [sessionId, sequence] of [
			["public-a", 1n],
			["public-b", 1n],
		] as const) {
			const payload = encodeAcpEvent({
				tag: "AcpDurableSessionEvent",
				val: {
					sessionId,
					sequence,
					timestamp: "2026-01-01T00:00:01.000Z",
					event: {
						tag: "AcpDurableSessionUpdate",
						val: {
							update: JSON.stringify({
								sessionUpdate: "agent_message_chunk",
								content: { type: "text", text: sessionId },
							}),
						},
					},
				},
			});
			(
				vm as unknown as {
					_handleAcpExtEvent(event: {
						namespace: string;
						payload: Uint8Array;
					}): void;
				}
			)._handleAcpExtEvent({ namespace: ACP_EXTENSION_NAMESPACE, payload });
		}

		expect(firstEvents).toHaveLength(1);
		expect(secondEvents).toHaveLength(1);
		expect(firstEvents[0]).toMatchObject({
			sessionId: "public-a",
			sequence: 1,
		});
		expect(secondEvents[0]).toMatchObject({
			sessionId: "public-b",
			sequence: 1,
		});
	});
});
