import { describe, expect, it } from "vitest";
import { createConvergedExecutionHostBridge } from "../../src/converged-execution-host-bridge.js";
import { createAgentOsConvergedSidecar } from "../../src/converged-sidecar.js";

// Unit coverage for the Agent OS converged-consumption layer (the TS that plugs
// the ACP wasm sidecar into @rivet-dev/agentos-runtime-browser's converged runtime). The
// end-to-end Chromium proof lives in tests/browser-wasm/converged-runtime.spec.ts;
// these are the fast headless units that pin the contract.

describe("converged execution host bridge", () => {
	it("echoes the execution id startExecution returns", () => {
		const host = createConvergedExecutionHostBridge();
		// Default before any set.
		expect(host.bridge.startExecution("{}")).toEqual({
			executionId: "converged-exec",
		});
		host.setNextExecutionId("exec-42");
		expect(host.bridge.startExecution("{}")).toEqual({
			executionId: "exec-42",
		});
	});

	it("mints distinct javascript context ids", () => {
		const host = createConvergedExecutionHostBridge();
		const a = host.bridge.createJavascriptContext("{}") as {
			contextId: string;
		};
		const b = host.bridge.createJavascriptContext("{}") as {
			contextId: string;
		};
		expect(a.contextId).not.toBe(b.contextId);
		expect(a.contextId).toMatch(/^converged-ctx-/);
	});

	it("treats the execution lifecycle callbacks as no-ops", () => {
		const host = createConvergedExecutionHostBridge();
		expect(host.bridge.writeExecutionStdin("{}")).toEqual({});
		expect(host.bridge.closeExecutionStdin("{}")).toEqual({});
		expect(host.bridge.killExecution("{}")).toEqual({});
		// No buffered events: the converged driver surfaces them on its own channels.
		expect(host.bridge.pollExecutionEvent("{}")).toBeNull();
	});

	it("reflects the worker runtime in createWorker", () => {
		const host = createConvergedExecutionHostBridge();
		const worker = host.bridge.createWorker(
			JSON.stringify({ runtime: "java_script" }),
		) as { workerId: string; runtime?: string };
		expect(worker.runtime).toBe("java_script");
		expect(worker.workerId).toMatch(/^converged-worker-/);
	});
});

describe("createAgentOsConvergedSidecar", () => {
	const config = { permissions: { fs: "allow" } } as never;

	it("builds ConvergedSidecarFactoryOptions carrying the config + bare codec", () => {
		const options = createAgentOsConvergedSidecar(config);
		expect(options.config).toBe(config);
		expect(options.codec).toBe("bare");
		expect(typeof options.loadSidecar).toBe("function");
	});

	it("honours an explicit codec + onFsReadDenied override", () => {
		const onFsReadDenied = () => {};
		const options = createAgentOsConvergedSidecar(config, {
			codec: "json" as never,
			onFsReadDenied,
		});
		expect(options.codec).toBe("json");
		expect(options.onFsReadDenied).toBe(onFsReadDenied);
	});
});
