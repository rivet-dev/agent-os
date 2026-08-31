import {
	ExecutionOutcome,
	ExecutionState,
} from "@rivet-dev/agentos-runtime-core/protocol";
import { describe, expect, it, vi } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import type { CodeExecutionResult } from "../src/language-execution.js";

const EXECUTION_ID = "execution-1";

type ExecutionCompletedHandler = (event: {
	executionId: string;
	generation: number;
	outcome: "cancelled";
}) => void;

type ExecutionAgent = {
	_executionOutputHandlers: Map<string, Set<(event: unknown) => void>>;
	_executionCompletedHandlers: Map<string, Set<ExecutionCompletedHandler>>;
	_sidecarClient: {
		sendVmRequest: ReturnType<typeof vi.fn>;
	};
	_sidecarSession: unknown;
	_sidecarVm: unknown;
	_executionOperation(
		payload: unknown,
		options: { signal?: AbortSignal },
	): Promise<CodeExecutionResult>;
};

function createExecutionAgent() {
	const agent = Object.create(AgentOs.prototype) as ExecutionAgent;
	agent._executionOutputHandlers = new Map();
	agent._executionCompletedHandlers = new Map();
	agent._sidecarSession = {};
	agent._sidecarVm = {};
	agent._sidecarClient = {
		sendVmRequest: vi.fn(async (_session, _vm, payload) => {
			switch (payload.type) {
				case "javascript_execution":
					return {
						type: "execution_accepted",
						response: {
							operationId: EXECUTION_ID,
							execution: {
								executionId: EXECUTION_ID,
								pid: 123,
								createdAtMs: 0n,
							},
						},
					};
				case "cancel_execution":
					setTimeout(() => {
						for (const handler of agent._executionCompletedHandlers.get("*") ??
							[]) {
							handler({
								executionId: EXECUTION_ID,
								generation: 1,
								outcome: "cancelled",
							});
						}
					}, 0);
					return {
						type: "execution_descriptor",
						response: {
							execution: {
								executionId: EXECUTION_ID,
								state: ExecutionState.Idle,
								retainedLanguage: null,
								createdAtMs: 0n,
								lastStartedAtMs: 0n,
								lastCompletedAtMs: 1n,
							},
						},
					};
				case "wait_execution":
					return {
						type: "execution_completed",
						response: {
							execution: null,
							outcome: ExecutionOutcome.Cancelled,
							exitCode: null,
							error: null,
							stdout: null,
							stderr: null,
							stdoutTruncated: null,
							stderrTruncated: null,
							evaluationValue: null,
							typeScriptCheckResult: null,
						},
					};
				default:
					throw new Error(`unexpected request: ${payload.type}`);
			}
		}),
	};
	return agent;
}

describe("AgentOs execution abort", () => {
	it("rejects without admission when the signal is already aborted", async () => {
		const agent = createExecutionAgent();
		const controller = new AbortController();
		const reason = new DOMException("stop", "AbortError");
		controller.abort(reason);

		await expect(
			agent._executionOperation(
				{ type: "javascript_execution" },
				{ signal: controller.signal },
			),
		).rejects.toBe(reason);
		expect(agent._sidecarClient.sendVmRequest).not.toHaveBeenCalled();
	});

	it("returns the structured cancellation result after admission", async () => {
		const agent = createExecutionAgent();
		const controller = new AbortController();
		const execution = agent._executionOperation(
			{ type: "javascript_execution" },
			{ signal: controller.signal },
		);
		const settled = execution.then(
			(result) => ({ status: "resolved" as const, result }),
			(error: unknown) => ({ status: "rejected" as const, error }),
		);

		await Promise.resolve();
		controller.abort(new DOMException("stop", "AbortError"));

		expect(await settled).toEqual({
			status: "resolved",
			result: {
				outcome: "cancelled",
				error: {
					code: "execution_failed",
					name: "ExecutionError",
					message: "execution completed with cancelled",
				},
			},
		});
		expect(agent._sidecarClient.sendVmRequest).toHaveBeenCalledWith(
			agent._sidecarSession,
			agent._sidecarVm,
			{ type: "cancel_execution", request: { executionId: EXECUTION_ID } },
		);
	});
});
