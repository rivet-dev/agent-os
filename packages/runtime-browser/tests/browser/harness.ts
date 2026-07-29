import { expect, type Page } from "@playwright/test";
import type {
	ExecOptions,
	OSConfig,
	ProcessConfig,
	TimingMitigation,
} from "../../src/runtime.js";

export type HarnessStdioEvent = {
	channel: "stdout" | "stderr";
	message: string;
};

export type HarnessCreateRuntimeOptions = {
	filesystem?: "memory" | "opfs";
	timingMitigation?: TimingMitigation;
	commandExecutor?: "echo";
	denyFsRead?: boolean;
	denyChildProcess?: boolean;
	denyNetwork?: boolean;
	denyNetworkPort?: number;
	payloadLimits?: {
		base64TransferBytes?: number;
		jsonPayloadBytes?: number;
	};
	useDefaultNetwork?: boolean;
	processConfig?: ProcessConfig;
	osConfig?: OSConfig;
};

export type HarnessCreateRuntimeResponse = {
	crossOriginIsolated: boolean;
	runtimeId: string;
	workerUrl: string;
};

export type HarnessExecResponse = {
	crossOriginIsolated: boolean;
	result: {
		code: number;
		errorMessage?: string;
	};
	stdio: HarnessStdioEvent[];
	permissionDecisions: {
		deniedFsReads: number;
	};
};

export type HarnessTerminatePendingResponse = {
	outcome: "resolved" | "rejected";
	resultCode: number | null;
	errorMessage: string | null;
	signaled?: boolean;
	debug: {
		disposed: boolean;
		pendingCount: number;
		signalState: number[];
		signalHandlers: Array<{
			executionId: string;
			handlers: Array<{
				signal: number;
				action: string;
				mask: number[];
				flags: number;
			}>;
		}>;
		workerOnmessage: "null" | "set";
		workerOnerror: "null" | "set";
	};
};

export type HarnessRuntimeDebugResponse =
	HarnessTerminatePendingResponse["debug"];

export type HarnessSmokeResponse = HarnessExecResponse & {
	workerUrl: string;
};

type AgentOsBrowserHarness = {
	createRuntime(
		options?: HarnessCreateRuntimeOptions,
	): Promise<HarnessCreateRuntimeResponse>;
	exec(
		runtimeId: string,
		code: string,
		options?: ExecOptions,
	): Promise<HarnessExecResponse>;
	disposeRuntime(runtimeId: string): Promise<void>;
	disposeAllRuntimes(): Promise<void>;
	terminatePendingExec(
		runtimeId: string,
		code: string,
		delayMs?: number,
	): Promise<HarnessTerminatePendingResponse>;
	signalPendingExec(
		runtimeId: string,
		code: string,
		signal?: number,
		delayMs?: number,
	): Promise<HarnessTerminatePendingResponse>;
	debugPendingExec(
		runtimeId: string,
		code: string,
		delayMs?: number,
	): Promise<HarnessTerminatePendingResponse>;
	runtimeDebug(runtimeId: string): Promise<HarnessRuntimeDebugResponse>;
	smoke(): Promise<HarnessSmokeResponse>;
};

declare global {
	interface Window {
		__agentOsBrowserHarness?: AgentOsBrowserHarness;
	}
}

// The converged harness (wasm kernel) is the sole conformance runtime; the
// legacy in-process TS-kernel harness has been removed.
export async function openHarnessPage(page: Page): Promise<void> {
	await page.goto("/frontend/converged-conformance-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");
}

export async function createRuntime(
	page: Page,
	options?: HarnessCreateRuntimeOptions,
): Promise<HarnessCreateRuntimeResponse> {
	return page.evaluate(async (optionsArg) => {
		const harness = window.__agentOsBrowserHarness;
		if (!harness) {
			throw new Error("Browser harness is unavailable on window");
		}
		return harness.createRuntime(optionsArg);
	}, options);
}

export async function execRuntime(
	page: Page,
	runtimeId: string,
	code: string,
	options?: ExecOptions,
): Promise<HarnessExecResponse> {
	return page.evaluate(
		async ({ runtimeId: runtimeIdArg, code: codeArg, options: optionsArg }) => {
			const harness = window.__agentOsBrowserHarness;
			if (!harness) {
				throw new Error("Browser harness is unavailable on window");
			}
			return harness.exec(runtimeIdArg, codeArg, optionsArg);
		},
		{ runtimeId, code, options },
	);
}

export async function disposeRuntime(
	page: Page,
	runtimeId: string,
): Promise<void> {
	await page.evaluate(async (runtimeIdArg) => {
		const harness = window.__agentOsBrowserHarness;
		if (!harness) {
			return;
		}
		await harness.disposeRuntime(runtimeIdArg);
	}, runtimeId);
}

export async function disposeAllRuntimes(page: Page): Promise<void> {
	await page.evaluate(async () => {
		const harness = window.__agentOsBrowserHarness;
		if (!harness) {
			return;
		}
		await harness.disposeAllRuntimes();
	});
}

export async function terminatePendingExec(
	page: Page,
	runtimeId: string,
	code: string,
	delayMs?: number,
): Promise<HarnessTerminatePendingResponse> {
	return page.evaluate(
		async ({ runtimeId: runtimeIdArg, code: codeArg, delayMs: delayMsArg }) => {
			const harness = window.__agentOsBrowserHarness;
			if (!harness) {
				throw new Error("Browser harness is unavailable on window");
			}
			return harness.terminatePendingExec(runtimeIdArg, codeArg, delayMsArg);
		},
		{ runtimeId, code, delayMs },
	);
}

export async function signalPendingExec(
	page: Page,
	runtimeId: string,
	code: string,
	signal?: number,
	delayMs?: number,
): Promise<HarnessTerminatePendingResponse> {
	return page.evaluate(
		async ({
			runtimeId: runtimeIdArg,
			code: codeArg,
			signal: signalArg,
			delayMs: delayMsArg,
		}) => {
			const harness = window.__agentOsBrowserHarness;
			if (!harness) {
				throw new Error("Browser harness is unavailable on window");
			}
			return harness.signalPendingExec(
				runtimeIdArg,
				codeArg,
				signalArg,
				delayMsArg,
			);
		},
		{ runtimeId, code, signal, delayMs },
	);
}

export async function debugPendingExec(
	page: Page,
	runtimeId: string,
	code: string,
	delayMs?: number,
): Promise<HarnessTerminatePendingResponse> {
	return page.evaluate(
		async ({ runtimeId: runtimeIdArg, code: codeArg, delayMs: delayMsArg }) => {
			const harness = window.__agentOsBrowserHarness;
			if (!harness) {
				throw new Error("Browser harness is unavailable on window");
			}
			return harness.debugPendingExec(runtimeIdArg, codeArg, delayMsArg);
		},
		{ runtimeId, code, delayMs },
	);
}

export async function runtimeDebug(
	page: Page,
	runtimeId: string,
): Promise<HarnessRuntimeDebugResponse> {
	return page.evaluate(async (runtimeIdArg) => {
		const harness = window.__agentOsBrowserHarness;
		if (!harness) {
			throw new Error("Browser harness is unavailable on window");
		}
		return harness.runtimeDebug(runtimeIdArg);
	}, runtimeId);
}

export async function smokeHarness(page: Page): Promise<HarnessSmokeResponse> {
	return page.evaluate(async () => {
		const harness = window.__agentOsBrowserHarness;
		if (!harness) {
			throw new Error("Browser harness is unavailable on window");
		}
		return harness.smoke();
	});
}

export function getLastStdioMessage(
	response: HarnessExecResponse,
	channel: HarnessStdioEvent["channel"],
): string {
	const message = response.stdio
		.filter((event) => event.channel === channel)
		.at(-1)?.message;
	if (!message) {
		throw new Error(`Missing ${channel} output in harness response`);
	}
	return message;
}
