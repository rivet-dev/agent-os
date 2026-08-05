// Execution host bridge for the converged wasm sidecar.
//
// In the converged browser runtime the guest runs in the browser worker, not in
// the wasm sidecar. But guest `net.*`/`dgram.*` syscalls need a kernel process
// (pid) and socket ownership inside the sidecar, which is created by an `execute`
// wire request. That request drives these host-bridge callbacks. They are
// no-ops that just satisfy the sidecar's execution lifecycle: the only one that
// matters is `startExecution`, which echoes the driver-provided execution id so
// the sidecar registers the kernel process under the SAME id the guest worker
// uses in its sync-bridge `guest_kernel_call`s.
//
// The wasm `BrowserJsBridge` invokes each method with a JSON request string and
// JSON-decodes the return value.

export interface ConvergedExecutionHostBridge {
	/** Set the execution id `startExecution` will echo for the next `execute`. */
	setNextExecutionId(executionId: string): void;
	/** The host-bridge object passed to `new BrowserSidecarWasm(bridge)`. */
	readonly bridge: Record<string, (requestJson: string) => unknown>;
}

export function createConvergedExecutionHostBridge(): ConvergedExecutionHostBridge {
	let nextExecutionId = "converged-exec";
	let contextCounter = 0;
	let workerCounter = 0;

	const bridge: Record<string, (requestJson: string) => unknown> = {
		createJavascriptContext() {
			contextCounter += 1;
			return { contextId: `converged-ctx-${contextCounter}` };
		},
		createWasmContext() {
			contextCounter += 1;
			return { contextId: `converged-wasm-ctx-${contextCounter}` };
		},
		startExecution() {
			return { executionId: nextExecutionId };
		},
		createWorker(requestJson: string) {
			workerCounter += 1;
			const request = parse(requestJson);
			return {
				workerId: `converged-worker-${workerCounter}`,
				runtime: typeof request.runtime === "string" ? request.runtime : undefined,
			};
		},
		writeExecutionStdin() {
			return {};
		},
		closeExecutionStdin() {
			return {};
		},
		killExecution() {
			return {};
		},
		pollExecutionEvent() {
			return null;
		},
		terminateWorker() {
			return {};
		},
		// Diagnostics/observability callbacks the execute lifecycle emits; no-ops
		// here (the converged driver surfaces events through its own channels).
		emitStructuredEvent() {
			return {};
		},
		emitDiagnostic() {
			return {};
		},
		emitLog() {
			return {};
		},
		emitLifecycle() {
			return {};
		},
	};

	return {
		setNextExecutionId(executionId: string) {
			nextExecutionId = executionId;
		},
		bridge,
	};
}

function parse(requestJson: string): Record<string, unknown> {
	try {
		const value = JSON.parse(requestJson);
		return typeof value === "object" && value !== null
			? (value as Record<string, unknown>)
			: {};
	} catch {
		return {};
	}
}
