import { describe, expect, it } from "vitest";
import { AgentOs } from "../src/agent-os.js";

const SESSION_ID = "session-1";

function createSessionUpdateNotification(text: string) {
	return {
		jsonrpc: "2.0" as const,
		method: "session/update",
		params: {
			update: {
				sessionUpdate: "agent_message_chunk",
				content: {
					text,
				},
			},
		},
	};
}

function createTrackedAgent(initialTexts: string[] = []) {
	const agent = Object.create(AgentOs.prototype) as AgentOs & {
		_sessions: Map<string, unknown>;
		_recordSessionNotification: (
			session: Record<string, unknown>,
			sequenceNumber: number,
			notification: ReturnType<typeof createSessionUpdateNotification>,
		) => void;
	};

	const events = initialTexts.map((text, index) => ({
		sequenceNumber: index + 1,
		notification: createSessionUpdateNotification(text),
	}));

	const trackedSession = {
		sessionId: SESSION_ID,
		agentType: "codex",
		processId: "proc-1",
		pid: null,
		closed: false,
		modes: null,
		configOptions: [],
		capabilities: {},
		agentInfo: null,
		highestSequenceNumber: events.at(-1)?.sequenceNumber ?? null,
		events,
		eventHandlers: new Set(),
		sessionEventDispatchScheduled: false,
		permissionHandlers: new Set(),
		configOverrides: new Map(),
		pendingPermissionReplies: new Map(),
	};

	agent._sessions = new Map([[SESSION_ID, trackedSession]]);
	return { agent, trackedSession };
}

function readText(event: { params?: unknown }): string {
	const params = event.params as {
		update?: { content?: { text?: string } };
	};
	return params.update?.content?.text ?? "";
}

async function flushSessionEventDispatch(): Promise<void> {
	await Promise.resolve();
}

describe("AgentOs session event ordering", () => {
	it("replays buffered events to late subscribers before returning and keeps live delivery ordered", async () => {
		const { agent, trackedSession } = createTrackedAgent(["alpha", "beta"]);
		const seen: string[] = [];

		const unsubscribe = agent.onSessionEvent(SESSION_ID, (event) => {
			seen.push(readText(event));
		});

		expect(seen).toEqual(["alpha", "beta"]);

		agent._recordSessionNotification(
			trackedSession,
			4,
			createSessionUpdateNotification("delta"),
		);
		agent._recordSessionNotification(
			trackedSession,
			3,
			createSessionUpdateNotification("gamma"),
		);
		await flushSessionEventDispatch();

		expect(seen).toEqual(["alpha", "beta", "gamma", "delta"]);

		unsubscribe();
		agent._recordSessionNotification(
			trackedSession,
			5,
			createSessionUpdateNotification("epsilon"),
		);
		await flushSessionEventDispatch();

		expect(seen).toEqual(["alpha", "beta", "gamma", "delta"]);
	});

	it("delivers out-of-order sidecar events to subscribers in sequence order", async () => {
		const { agent, trackedSession } = createTrackedAgent();
		const seen: string[] = [];

		agent.onSessionEvent(SESSION_ID, (event) => {
			seen.push(readText(event));
		});

		agent._recordSessionNotification(
			trackedSession,
			2,
			createSessionUpdateNotification("second"),
		);
		agent._recordSessionNotification(
			trackedSession,
			1,
			createSessionUpdateNotification("first"),
		);
		await flushSessionEventDispatch();

		expect(seen).toEqual(["first", "second"]);
		expect(agent.getSessionEvents(SESSION_ID).map((event) => event.sequenceNumber)).toEqual([
			1,
			2,
		]);
	});
});
