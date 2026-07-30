import { describe, expect, it } from "vitest";
import { createConvergedExecutionHostBridge } from "../../src/converged-execution-host-bridge.js";

describe("converged execution host bridge", () => {
	it("echoes the configured execution id from startExecution", () => {
		const host = createConvergedExecutionHostBridge();
		host.setNextExecutionId("exec-42");
		expect(host.bridge.startExecution("{}")).toEqual({
			executionId: "exec-42",
		});
	});

	it("returns context and worker handles for the execution lifecycle", () => {
		const host = createConvergedExecutionHostBridge();
		expect(host.bridge.createJavascriptContext("{}")).toMatchObject({
			contextId: expect.stringContaining("converged-ctx-"),
		});
		expect(
			host.bridge.createWorker(JSON.stringify({ runtime: "javascript" })),
		).toMatchObject({
			workerId: expect.stringContaining("converged-worker-"),
			runtime: "javascript",
		});
	});

	it("treats stdin/kill/terminate as no-ops and polls no events", () => {
		const host = createConvergedExecutionHostBridge();
		expect(host.bridge.writeExecutionStdin("{}")).toEqual({});
		expect(host.bridge.killExecution("{}")).toEqual({});
		expect(host.bridge.terminateWorker("{}")).toEqual({});
		expect(host.bridge.pollExecutionEvent("{}")).toBeNull();
	});
});
