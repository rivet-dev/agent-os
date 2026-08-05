import { describe, expect, it } from "vitest";
import {
	AgentInteractionError,
	type AgentDriveDeps,
	type AgentOutput,
	driveAgentInteraction,
} from "../../src/agent-drive-loop.js";

const text = (s: string) => new TextEncoder().encode(s);

/** A scripted deps: a queue of agent outputs, and deliver responses that go
 * pending until the final one returns the real result. */
function scriptedDeps(opts: {
	outputs: (AgentOutput | null)[];
	// For each delivered chunk, whether it is still pending; the last !pending one
	// carries the final result.
	deliver: { pending: boolean; response: string }[];
	clock?: number[];
}): { deps: AgentDriveDeps; delivered: Uint8Array[] } {
	const outputs = [...opts.outputs];
	const deliver = [...opts.deliver];
	const delivered: Uint8Array[] = [];
	const clock = opts.clock ? [...opts.clock] : [];
	let now = 0;
	return {
		delivered,
		deps: {
			pollAgentOutput: () => outputs.shift() ?? null,
			deliverAgentOutput: (_pid, chunk) => {
				delivered.push(chunk);
				const next = deliver.shift();
				if (!next) throw new Error("unexpected deliver");
				return { pending: next.pending, response: text(next.response) };
			},
			now: () => (clock.length ? (now = clock.shift()!) : now),
		},
	};
}

describe("driveAgentInteraction", () => {
	it("feeds stdout chunks until the handshake completes, returning the real result", () => {
		const { deps, delivered } = scriptedDeps({
			outputs: [
				{ kind: "stdout", payload: text("initialize-response") },
				{ kind: "stdout", payload: text("session-new-response") },
			],
			deliver: [
				{ pending: true, response: "acp-pending" },
				{ pending: false, response: "acp-session-created" },
			],
		});
		const result = driveAgentInteraction(deps, "proc-1", 1_000_000);
		expect(new TextDecoder().decode(result)).toBe("acp-session-created");
		// Both stdout chunks were fed in order.
		expect(delivered.map((d) => new TextDecoder().decode(d))).toEqual([
			"initialize-response",
			"session-new-response",
		]);
	});

	it("skips stderr (it does not advance the handshake)", () => {
		const { deps, delivered } = scriptedDeps({
			outputs: [
				{ kind: "stderr", payload: text("warning: noise") },
				{ kind: "stdout", payload: text("the-response") },
			],
			deliver: [{ pending: false, response: "done" }],
		});
		const result = driveAgentInteraction(deps, "p", 1_000_000);
		expect(new TextDecoder().decode(result)).toBe("done");
		expect(delivered).toHaveLength(1); // stderr was not delivered
	});

	it("throws if the agent exits before completing the interaction", () => {
		const { deps } = scriptedDeps({
			outputs: [{ kind: "exit", payload: new Uint8Array([0]) }],
			deliver: [],
		});
		expect(() => driveAgentInteraction(deps, "p", 1_000_000)).toThrow(AgentInteractionError);
	});

	it("throws on timeout when no output arrives", () => {
		const { deps } = scriptedDeps({ outputs: [null], deliver: [] });
		expect(() => driveAgentInteraction(deps, "p", 1_000_000)).toThrow(/no output/);
	});

	it("throws when the deadline has already passed", () => {
		const { deps } = scriptedDeps({
			outputs: [{ kind: "stdout", payload: text("x") }],
			deliver: [],
			clock: [2_000],
		});
		expect(() => driveAgentInteraction(deps, "p", 1_000)).toThrow(/timed out/);
	});
});
