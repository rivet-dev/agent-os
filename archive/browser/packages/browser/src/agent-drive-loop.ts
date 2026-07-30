// The kernel-worker drive loop for a resumable ACP interaction (AGENTOS-WEB-ASYNC-
// AGENTS.md §3.2.1). After a create_session / session/prompt wire frame returns
// AcpPending{processId}, the kernel worker drives the agent to completion: it reads
// the agent's stdout from the reactor (which services the agent's own syscalls in
// the meantime — legal, because we are NOT inside a pushFrame here) and feeds each
// chunk back via deliver_agent_output (a fresh, non-nested pushFrame) until the
// handshake/turn completes and yields the real result.
//
// This is pure orchestration over injected dependencies, so it is unit-testable
// without the wasm kernel, the reactor, or real workers.

export type AgentOutputKind = "stdout" | "stderr" | "exit";

export interface AgentOutput {
	kind: AgentOutputKind;
	payload: Uint8Array;
}

export interface DriveResult {
	/** True while the interaction is still in flight (AcpPendingResponse). */
	pending: boolean;
	/** The decoded response frame bytes (the real ACP result when !pending). */
	response: Uint8Array;
}

export interface AgentDriveDeps {
	/** Read the next output for this execution from the reactor, servicing the
	 * agent's syscalls while waiting; null on timeout. */
	pollAgentOutput(processId: string, deadlineMs: number): AgentOutput | null;
	/** Feed a chunk of agent stdout into the resumable handshake (a non-nested
	 * pushFrame) and return whether it is still pending + the response frame. */
	deliverAgentOutput(processId: string, chunk: Uint8Array): DriveResult;
	now(): number;
}

export class AgentInteractionError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "AgentInteractionError";
	}
}

/**
 * Drive a resumable ACP interaction to completion. Returns the real ACP result
 * frame bytes (the deferred response the kernel worker relays to the client).
 * Throws if the agent exits or times out before producing a result.
 */
export function driveAgentInteraction(
	deps: AgentDriveDeps,
	processId: string,
	deadlineMs: number,
): Uint8Array {
	for (;;) {
		if (deps.now() >= deadlineMs) {
			throw new AgentInteractionError(`agent interaction timed out (${processId})`);
		}
		const out = deps.pollAgentOutput(processId, deadlineMs);
		if (out === null) {
			throw new AgentInteractionError(
				`agent produced no output before the deadline (${processId})`,
			);
		}
		if (out.kind === "exit") {
			throw new AgentInteractionError(
				`agent exited before completing the ACP interaction (${processId})`,
			);
		}
		if (out.kind === "stderr") {
			// stderr is diagnostic; it does not advance the JSON-RPC handshake.
			continue;
		}
		const result = deps.deliverAgentOutput(processId, out.payload);
		if (!result.pending) {
			return result.response;
		}
	}
}
