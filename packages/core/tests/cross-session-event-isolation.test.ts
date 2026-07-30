import { afterEach, describe, expect, test } from "vitest";
import type { SessionStreamEntry } from "../src/index.js";
import { AgentOs } from "../src/index.js";
import { encodeAcpEvent } from "../src/sidecar/agentos-acp-protocol.js";

const ACP_EXTENSION_NAMESPACE = "dev.rivet.agent-os.acp";

describe("cross-session ACP event isolation", () => {
	let vm: AgentOs | null = null;

	afterEach(async () => {
		await vm?.dispose();
		vm = null;
	});

	test("a durable event stamped for B is not delivered to A", async () => {
		vm = await AgentOs.create({});
		const aEvents: SessionStreamEntry[] = [];
		const bEvents: SessionStreamEntry[] = [];
		vm.onSessionEvent("session-A", (event) => aEvents.push(event));
		vm.onSessionEvent("session-B", (event) => bEvents.push(event));

		const payload = encodeAcpEvent({
			tag: "AcpDurableSessionEvent",
			val: {
				sessionId: "session-B",
				sequence: 1n,
				timestamp: "2026-07-18T00:00:00.000Z",
				event: {
					tag: "AcpDurableSessionUpdate",
					val: {
						update: JSON.stringify({
							sessionUpdate: "agent_message_chunk",
							content: { type: "text", text: "for B only" },
						}),
					},
				},
			},
		});

		(
			vm as unknown as {
				_handleAcpExtEvent(env: {
					namespace: string;
					payload: Uint8Array;
				}): void;
			}
		)._handleAcpExtEvent({ namespace: ACP_EXTENSION_NAMESPACE, payload });

		expect(aEvents).toHaveLength(0);
		expect(bEvents).toHaveLength(1);
		expect(bEvents[0]).toMatchObject({
			sessionId: "session-B",
			type: "agent_message_chunk",
		});
	});
});
