import { describe, expect, it } from "vitest";
import type { SessionStreamEntry } from "../src/index.js";
import { AgentOs } from "../src/agent-os.js";
import { encodeAcpEvent } from "../src/sidecar/agentos-acp-protocol.js";

const SESSION_ID = "session-1";
const ACP_EXTENSION_NAMESPACE = "dev.rivet.agent-os.acp";

function createTrackedAgent() {
	const agent = Object.create(AgentOs.prototype) as AgentOs & {
		_durableSessionEventHandlers: Map<
			string,
			Set<(entry: SessionStreamEntry) => void>
		>;
		_agentStderrHandler?: (event: {
			sessionId: string;
			agentType: string;
			processId: string;
			pid: number | null;
			chunk: Uint8Array;
		}) => void;
		_handleAcpExtEvent(env: { namespace: string; payload: Uint8Array }): void;
	};
	agent._durableSessionEventHandlers = new Map();
	return agent;
}

function emitText(agent: AgentOs, sequence: bigint, text: string): void {
	(
		agent as unknown as {
			_handleAcpExtEvent(env: {
				namespace: string;
				payload: Uint8Array;
			}): void;
		}
	)._handleAcpExtEvent({
		namespace: ACP_EXTENSION_NAMESPACE,
		payload: encodeAcpEvent({
			tag: "AcpDurableSessionEvent",
			val: {
				sessionId: SESSION_ID,
				sequence,
				timestamp: "2026-07-18T00:00:00.000Z",
				event: {
					tag: "AcpDurableSessionUpdate",
					val: {
						update: JSON.stringify({
							sessionUpdate: "agent_message_chunk",
							content: { type: "text", text },
						}),
					},
				},
			},
		}),
	});
}

function readText(event: SessionStreamEntry): string {
	return event.type === "agent_message_chunk" && event.content.type === "text"
		? event.content.text
		: "";
}

describe("AgentOs session event ordering", () => {
	it("subscribes to live events without replaying buffered history", () => {
		const agent = createTrackedAgent();
		const seen: string[] = [];
		const unsubscribe = agent.onSessionEvent(SESSION_ID, (event) => {
			seen.push(readText(event));
		});

		expect(seen).toEqual([]);
		emitText(agent, 1n, "delta");
		emitText(agent, 2n, "gamma");
		expect(seen).toEqual(["delta", "gamma"]);

		unsubscribe();
		emitText(agent, 3n, "epsilon");
		expect(seen).toEqual(["delta", "gamma"]);
	});

	it("delivers live sidecar events to subscribers in arrival order", () => {
		const agent = createTrackedAgent();
		const seen: string[] = [];
		agent.onSessionEvent(SESSION_ID, (event) => seen.push(readText(event)));
		emitText(agent, 1n, "second");
		emitText(agent, 2n, "first");
		expect(seen).toEqual(["second", "first"]);
	});

	it("routes ACP agent stderr events to the agent stderr handler", () => {
		const agent = createTrackedAgent();
		const chunks: string[] = [];
		agent._agentStderrHandler = (event) => {
			chunks.push(new TextDecoder().decode(event.chunk));
		};
		agent._handleAcpExtEvent({
			namespace: ACP_EXTENSION_NAMESPACE,
			payload: encodeAcpEvent({
				tag: "AcpAgentStderrEvent",
				val: {
					sessionId: SESSION_ID,
					agentType: "codex",
					processId: "proc-1",
					chunk: new TextEncoder().encode("agent log\n").buffer,
				},
			}),
		});
		expect(chunks).toEqual(["agent log\n"]);
	});
});
