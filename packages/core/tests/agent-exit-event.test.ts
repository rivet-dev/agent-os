import { describe, expect, it } from "vitest";
import type { AgentExitEvent } from "../src/agent-os.js";
import { AgentOs } from "../src/agent-os.js";
import { encodeAcpEvent } from "../src/sidecar/agentos-acp-protocol.js";

const SESSION_ID = "session-1";
const ACP_EXTENSION_NAMESPACE = "dev.rivet.agent-os.acp";

type TrackedAgent = AgentOs & {
	_sessions: Map<string, unknown>;
	_agentExitHandlers: Map<string, Set<(event: AgentExitEvent) => void>>;
	_agentExitHandler?: (event: AgentExitEvent) => void;
	_handleAcpExtEvent(env: { namespace: string; payload: Uint8Array }): void;
};

function createTrackedAgent(): TrackedAgent {
	const agent = Object.create(AgentOs.prototype) as TrackedAgent;
	agent._sessions = new Map([
		[
			SESSION_ID,
			{
				sessionId: SESSION_ID,
				agentType: "codex",
				processId: "acp-agent-1",
				pid: 4242,
			},
		],
	]);
	agent._agentExitHandlers = new Map();
	return agent;
}

function encodeAgentExitedEvent(overrides?: {
	sessionId?: string;
	pid?: number | null;
	exitCode?: number | null;
}): Uint8Array {
	return encodeAcpEvent({
		tag: "AcpAgentExitedEvent",
		val: {
			sessionId: overrides?.sessionId ?? SESSION_ID,
			agentType: "codex",
			processId: "acp-agent-1",
			pid: overrides?.pid === undefined ? 4242 : overrides.pid,
			exitCode: overrides?.exitCode === undefined ? 7 : overrides.exitCode,
			restart: "not_attempted",
			restartCount: 0,
			maxRestarts: 0,
		},
	});
}

describe("AgentOs onAgentExit dispatch", () => {
	it("decodes AcpAgentExitedEvent and invokes the handler with session context", () => {
		const agent = createTrackedAgent();
		const seen: AgentExitEvent[] = [];
		agent._agentExitHandler = (event) => {
			seen.push(event);
		};

		agent._handleAcpExtEvent({
			namespace: ACP_EXTENSION_NAMESPACE,
			payload: encodeAgentExitedEvent(),
		});

		expect(seen).toEqual([
			{
				sessionId: SESSION_ID,
				agentType: "codex",
				processId: "acp-agent-1",
				pid: 4242,
				exitCode: 7,
				restart: "not_attempted",
				restartCount: 0,
				maxRestarts: 0,
			},
		]);
	});

	it("delivers events for unknown sessions with a null pid", () => {
		const agent = createTrackedAgent();
		const seen: AgentExitEvent[] = [];
		agent._agentExitHandler = (event) => {
			seen.push(event);
		};

		agent._handleAcpExtEvent({
			namespace: ACP_EXTENSION_NAMESPACE,
			payload: encodeAgentExitedEvent({
				sessionId: "evicted-session",
				pid: null,
				exitCode: null,
			}),
		});

		expect(seen).toHaveLength(1);
		expect(seen[0]).toMatchObject({
			sessionId: "evicted-session",
			pid: null,
			exitCode: null,
			restart: "not_attempted",
		});
	});

	it("swallows handler errors so event delivery keeps moving", () => {
		const agent = createTrackedAgent();
		agent._agentExitHandler = () => {
			throw new Error("subscriber exploded");
		};

		expect(() =>
			agent._handleAcpExtEvent({
				namespace: ACP_EXTENSION_NAMESPACE,
				payload: encodeAgentExitedEvent(),
			}),
		).not.toThrow();
	});
});
