// Execution host bridge for the converged Agent OS wasm sidecar.
//
// Two roles, selected by configuration:
//
// 1. DRIVER mode (default, no `agentExecutor`): mirrors agentos's no-op
//    execution host bridge. In the converged browser runtime the guest runs in the
//    browser worker (driven by @rivet-dev/agentos-runtime-browser's runtime driver), not in the
//    wasm sidecar; the sidecar only needs a kernel process (pid) for socket
//    ownership, created by an `execute` wire request. `startExecution` echoes the
//    driver-provided execution id (set via `setNextExecutionId`); the stdio
//    callbacks are no-ops (the driver owns the guest's stdio).
//
// 2. AGENT mode (`agentExecutor` provided): runs a SYNCHRONOUS in-process agent for
//    ACP `create_session`. Because the converged sidecar + the host-free `AcpCore`
//    run synchronously on the main thread (a single `pushFrame` drives the whole
//    initialize+session/new handshake), and the main thread may NOT block-wait on a
//    Worker (`Atomics.wait` is forbidden there; postMessage can't arrive mid-frame),
//    an *async* agent worker cannot be driven from here — see
//    AGENTOS-WEB-CONVERGENCE.md Step 7. A synchronous agent (each stdin line maps to
//    response lines with no async I/O, e.g. an ACP echo/test adapter) CAN: it runs
//    in the same call stack, so `writeExecutionStdin` synchronously produces the
//    output that the immediately-following `pollExecutionEvent` returns, and the
//    synchronous `AcpCore` completes within the one `pushFrame`.
//
// The wasm `AgentOsBrowserSidecarWasm` host bridge invokes each method with a JSON
// request string and JSON-decodes the return value.

/** A synchronous ACP agent: each newline-delimited stdin JSON-RPC line maps to
 * zero or more response lines, computed with no async I/O. */
export interface SyncAgent {
	/** Handle one complete stdin line (no trailing newline); return response lines. */
	handleLine(line: string): string[];
}

/** Creates a {@link SyncAgent} for one execution (the launched ACP adapter). */
export interface SyncAgentExecutor {
	createAgent(request: {
		executionId: string;
		vmId: string;
		argv: string[];
		env: Record<string, string>;
		cwd: string;
	}): SyncAgent;
}

export interface ConvergedExecutionHostBridgeOptions {
	/** When set, `startExecution` runs a synchronous in-process agent (AGENT mode). */
	agentExecutor?: SyncAgentExecutor;
}

export interface ConvergedExecutionHostBridge {
	/** Set the execution id `startExecution` echoes next (DRIVER mode). */
	setNextExecutionId(executionId: string): void;
	/** The host-bridge object passed to `new AgentOsBrowserSidecarWasm(bridge)`. */
	readonly bridge: Record<string, (requestJson: string) => unknown>;
}

interface AgentSession {
	vmId: string;
	agent: SyncAgent;
	buffer: string;
	events: unknown[];
	exited: boolean;
}

function decodeBase64ToText(base64: string): string {
	const binary = atob(base64);
	const bytes = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
	return new TextDecoder().decode(bytes);
}

function encodeTextToBase64(text: string): string {
	const bytes = new TextEncoder().encode(text);
	let binary = "";
	for (const byte of bytes) binary += String.fromCharCode(byte);
	return btoa(binary);
}

export function createConvergedExecutionHostBridge(
	options: ConvergedExecutionHostBridgeOptions = {},
): ConvergedExecutionHostBridge {
	let nextExecutionId = "converged-exec";
	let contextCounter = 0;
	let workerCounter = 0;
	let agentCounter = 0;
	const executor = options.agentExecutor;
	const sessions = new Map<string, AgentSession>();

	const bridge: Record<string, (requestJson: string) => unknown> = {
		createJavascriptContext() {
			contextCounter += 1;
			return { contextId: `converged-ctx-${contextCounter}` };
		},
		createWasmContext() {
			contextCounter += 1;
			return { contextId: `converged-wasm-ctx-${contextCounter}` };
		},
		startExecution(requestJson: string) {
			if (!executor) {
				// DRIVER mode: echo the driver-provided id.
				return { executionId: nextExecutionId };
			}
			// AGENT mode: mint a fresh id and instantiate the synchronous agent.
			const request = parse(requestJson);
			agentCounter += 1;
			const executionId = `agent-exec-${agentCounter}`;
			const vmId = typeof request.vmId === "string" ? request.vmId : "";
			const agent = executor.createAgent({
				executionId,
				vmId,
				argv: Array.isArray(request.argv) ? (request.argv as string[]) : [],
				env:
					request.env && typeof request.env === "object"
						? (request.env as Record<string, string>)
						: {},
				cwd: typeof request.cwd === "string" ? request.cwd : "",
			});
			sessions.set(executionId, {
				vmId,
				agent,
				buffer: "",
				events: [],
				exited: false,
			});
			return { executionId };
		},
		createWorker(requestJson: string) {
			workerCounter += 1;
			const request = parse(requestJson);
			return {
				workerId: `converged-worker-${workerCounter}`,
				runtime: typeof request.runtime === "string" ? request.runtime : undefined,
			};
		},
		writeExecutionStdin(requestJson: string) {
			const request = parse(requestJson);
			const executionId =
				typeof request.executionId === "string" ? request.executionId : "";
			const session = sessions.get(executionId);
			if (!session) return {};
			const chunkBase64 =
				typeof request.chunkBase64 === "string" ? request.chunkBase64 : "";
			session.buffer += decodeBase64ToText(chunkBase64);
			let newlineIndex = session.buffer.indexOf("\n");
			while (newlineIndex >= 0) {
				const line = session.buffer.slice(0, newlineIndex);
				session.buffer = session.buffer.slice(newlineIndex + 1);
				if (line.trim()) {
					for (const output of session.agent.handleLine(line)) {
						session.events.push({
							type: "stdout",
							vmId: session.vmId,
							executionId,
							chunkBase64: encodeTextToBase64(`${output}\n`),
						});
					}
				}
				newlineIndex = session.buffer.indexOf("\n");
			}
			return {};
		},
		closeExecutionStdin() {
			return {};
		},
		killExecution(requestJson: string) {
			const request = parse(requestJson);
			const executionId =
				typeof request.executionId === "string" ? request.executionId : "";
			const session = sessions.get(executionId);
			if (session && !session.exited) {
				session.exited = true;
				session.events.push({
					type: "exited",
					vmId: session.vmId,
					executionId,
					exitCode: 0,
				});
			}
			return {};
		},
		pollExecutionEvent(requestJson: string) {
			const request = parse(requestJson);
			const vmId = typeof request.vmId === "string" ? request.vmId : "";
			for (const session of sessions.values()) {
				if (session.vmId === vmId && session.events.length > 0) {
					return session.events.shift();
				}
			}
			return null;
		},
		terminateWorker() {
			return {};
		},
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
