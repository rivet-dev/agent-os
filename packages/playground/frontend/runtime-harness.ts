import {
	allowAll,
	createBrowserDriver,
	createBrowserRuntimeDriverFactory,
	type ExecOptions,
	type ExecResult,
	type NodeRuntimeDriver,
	type TimingMitigation,
} from "@rivet-dev/agent-os-browser";

type HarnessStdioEvent = {
	channel: "stdout" | "stderr";
	message: string;
};

type HarnessCreateRuntimeOptions = {
	filesystem?: "memory" | "opfs";
	timingMitigation?: TimingMitigation;
	payloadLimits?: {
		base64TransferBytes?: number;
		jsonPayloadBytes?: number;
	};
	useDefaultNetwork?: boolean;
};

type HarnessRuntimeDebugState = {
	disposed: boolean;
	pendingCount: number;
	signalState: number[];
	workerOnmessage: "null" | "set";
	workerOnerror: "null" | "set";
};

type HarnessTerminatePendingResponse = {
	outcome: "resolved" | "rejected";
	resultCode: number | null;
	errorMessage: string | null;
	debug: HarnessRuntimeDebugState;
};

type HarnessRuntimeEntry = {
	runtime: NodeRuntimeDriver;
	stdio: HarnessStdioEvent[];
};

type HarnessExecResponse = {
	crossOriginIsolated: boolean;
	result: ExecResult;
	stdio: HarnessStdioEvent[];
};

type HarnessCreateRuntimeResponse = {
	crossOriginIsolated: boolean;
	runtimeId: string;
	workerUrl: string;
};

type HarnessSmokeResponse = HarnessExecResponse & {
	workerUrl: string;
};

interface AgentOsBrowserHarness {
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
	smoke(): Promise<HarnessSmokeResponse>;
}

declare global {
	interface Window {
		__agentOsBrowserHarness?: AgentOsBrowserHarness;
	}
}

const runtimes = new Map<string, HarnessRuntimeEntry>();
const statusElement = document.querySelector<HTMLElement>("#harness-status");
const workerUrl = new URL("/agent-os-worker.js", window.location.origin);
const runtimeFactory = createBrowserRuntimeDriverFactory({ workerUrl });

function setStatus(
	state: "loading" | "ready" | "error",
	message: string,
): void {
	if (!statusElement) {
		return;
	}
	statusElement.dataset.state = state;
	statusElement.textContent = message;
}

function requireRuntime(runtimeId: string): HarnessRuntimeEntry {
	const entry = runtimes.get(runtimeId);
	if (!entry) {
		throw new Error(`Unknown browser harness runtime: ${runtimeId}`);
	}
	return entry;
}

function takeStdio(entry: HarnessRuntimeEntry): HarnessStdioEvent[] {
	const stdio = [...entry.stdio];
	entry.stdio.length = 0;
	return stdio;
}

function getRuntimeDebugState(
	runtime: NodeRuntimeDriver,
): HarnessRuntimeDebugState {
	const internal = runtime as NodeRuntimeDriver & {
		disposed?: boolean;
		pending?: Map<number, unknown>;
		syncBridge?: { signalBuffer: SharedArrayBuffer };
		worker?: { onmessage: unknown; onerror: unknown };
	};

	return {
		disposed: internal.disposed === true,
		pendingCount: internal.pending?.size ?? 0,
		signalState: internal.syncBridge
			? Array.from(new Int32Array(internal.syncBridge.signalBuffer))
			: [],
		workerOnmessage: internal.worker?.onmessage === null ? "null" : "set",
		workerOnerror: internal.worker?.onerror === null ? "null" : "set",
	};
}

const harness: AgentOsBrowserHarness = {
	async createRuntime(options) {
		const system = await createBrowserDriver({
			filesystem: options?.filesystem ?? "memory",
			permissions: allowAll,
			useDefaultNetwork: options?.useDefaultNetwork,
		});
		const stdio: HarnessStdioEvent[] = [];
		const runtime = runtimeFactory.createRuntimeDriver({
			system,
			runtime: system.runtime,
			onStdio: (event) => {
				stdio.push({
					channel: event.channel,
					message: event.message,
				});
			},
			timingMitigation: options?.timingMitigation,
			payloadLimits: options?.payloadLimits,
		});
		const runtimeId = globalThis.crypto.randomUUID();

		runtimes.set(runtimeId, {
			runtime,
			stdio,
		});

		return {
			crossOriginIsolated: window.crossOriginIsolated,
			runtimeId,
			workerUrl: workerUrl.href,
		};
	},

	async exec(runtimeId, code, options) {
		const entry = requireRuntime(runtimeId);
		entry.stdio.length = 0;
		const result = await entry.runtime.exec(code, options);
		return {
			crossOriginIsolated: window.crossOriginIsolated,
			result,
			stdio: takeStdio(entry),
		};
	},

	async disposeRuntime(runtimeId) {
		const entry = requireRuntime(runtimeId);
		runtimes.delete(runtimeId);
		if (typeof entry.runtime.terminate === "function") {
			await entry.runtime.terminate();
			return;
		}
		entry.runtime.dispose();
	},

	async disposeAllRuntimes() {
		const runtimeEntries = Array.from(runtimes.entries());
		runtimes.clear();
		for (const [, entry] of runtimeEntries) {
			try {
				if (typeof entry.runtime.terminate === "function") {
					await entry.runtime.terminate();
				} else {
					entry.runtime.dispose();
				}
			} catch {
				entry.runtime.dispose();
			}
		}
	},

	async terminatePendingExec(runtimeId, code, delayMs = 20) {
		const entry = requireRuntime(runtimeId);
		entry.stdio.length = 0;
		const execution = entry.runtime.exec(code);
		await new Promise((resolve) => setTimeout(resolve, delayMs));
		if (typeof entry.runtime.terminate === "function") {
			await entry.runtime.terminate();
		} else {
			entry.runtime.dispose();
		}

		let outcome: HarnessTerminatePendingResponse["outcome"] = "resolved";
		let resultCode: number | null = null;
		let errorMessage: string | null = null;

		try {
			const result = await execution;
			resultCode = result.code;
		} catch (error) {
			outcome = "rejected";
			errorMessage = error instanceof Error ? error.message : String(error);
		}

		runtimes.delete(runtimeId);
		return {
			outcome,
			resultCode,
			errorMessage,
			debug: getRuntimeDebugState(entry.runtime),
		};
	},

	async smoke() {
		const { runtimeId } = await harness.createRuntime();
		try {
			const response = await harness.exec(
				runtimeId,
				'console.log("harness-ready");',
			);
			return {
				...response,
				workerUrl: workerUrl.href,
			};
		} finally {
			await harness.disposeRuntime(runtimeId);
		}
	},
};

window.__agentOsBrowserHarness = harness;
setStatus("ready", "ready");
