// M3 kernel worker: the Agent OS converged kernel + the M1 reactor + the resumable
// drive loop, all in one Worker (AGENTOS-WEB-ASYNC-AGENTS.md §3.2.1). It spawns
// async agent execution workers, services their syscalls/stdout via the reactor,
// and drives create_session/session/prompt to completion via deliver_agent_output —
// NEVER block-waiting inside a pushFrame while an agent makes a mid-turn syscall.

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import {
	decodeBareProtocolFrame,
	encodeBareProtocolFrame,
} from "@rivet-dev/agentos-runtime-core/protocol-frames";
import { SIDECAR_PROTOCOL_SCHEMA } from "@rivet-dev/agentos-runtime-core/protocol-schema";
import {
	ConvergedSyncBridgeHandler,
	DEFERRED,
	FRAME_STDERR,
	FRAME_STDOUT,
	KernelReactor,
	PushFrameSidecarTransport,
	REACTOR_CONTROL_BYTES,
	sabRingByteLength,
	type SabRingLayout,
} from "@rivet-dev/agentos-runtime-browser";
import {
	decodeAcpResponse,
	encodeAcpRequest,
} from "../../../core/src/sidecar/agentos-acp-protocol.ts";
import { driveAgentInteraction } from "../../src/agent-drive-loop.js";
import { decodeSyscall } from "./syscall-codec.js";

const WASM_MODULE_URL = "/wasm/agentos_sidecar_browser.js";
const WASM_BINARY_URL = "/wasm/agentos_sidecar_browser_bg.wasm";
// The adapter entrypoint (argv[0]) selects which agent worker the main relay spawns.
// In the real executor the entrypoint is a VFS module; in this harness each test
// agent is a prebuilt worker bundle keyed by its `/bin/<name>` entrypoint.
const AGENT_WORKERS: Record<string, string> = {
	"/bin/async-echo-agent": "/async-echo-agent.worker.js",
	"/bin/async-infer-agent": "/async-infer-agent.worker.js",
	"/bin/async-loopback-agent": "/async-loopback-agent.worker.js",
	"/bin/async-proxy-agent": "/async-proxy-agent.worker.js",
	"/bin/pty-loopback-agent": "/pty-loopback-agent.worker.js",
	"/opt/agentos/bin/async-echo-agent": "/async-echo-agent.worker.js",
	"/opt/agentos/bin/async-infer-agent": "/async-infer-agent.worker.js",
	"/opt/agentos/bin/async-loopback-agent": "/async-loopback-agent.worker.js",
	"/opt/agentos/bin/async-proxy-agent": "/async-proxy-agent.worker.js",
	"/opt/agentos/bin/pty-loopback-agent": "/pty-loopback-agent.worker.js",
};
const DEFAULT_AGENT_WORKER_URL = "/async-echo-agent.worker.js";
const ACP_NS = "dev.rivet.agent-os.acp";
const LAYOUT: SabRingLayout = { slotCount: 64, slotBytes: 4096 };
const DRIVE_TIMEOUT_MS = 30_000;

interface KernelWasm {
	default(input?: unknown): Promise<unknown>;
	AgentOsBrowserSidecarWasm: new (hostBridge?: unknown) => {
		readonly sidecarId: string;
		pushFrame(frame: Uint8Array): unknown;
	};
}

function decodeBase64(base64: string): Uint8Array {
	const binary = atob(base64);
	const bytes = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
	return bytes;
}

// The async-agent execution host bridge passed to the wasm sidecar: startExecution
// spawns an agent worker + registers its SAB pair with the reactor; writeExecutionStdin
// posts stdin to the agent (the §3.2 split); the kernel drives output via the reactor.
function createAsyncAgentHost(reactor: KernelReactor, controlSab: SharedArrayBuffer) {
	let contextCounter = 0;
	let execCounter = 0;
	let lastExecutionId: string | null = null;
	const agents = new Set<string>();

	const parse = (json: string): Record<string, unknown> => {
		try {
			const value = JSON.parse(json);
			return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {};
		} catch {
			return {};
		}
	};

	// The MAIN thread spawns the agent worker (review F11): a nested `new Worker()`
	// cannot finish loading while the kernel worker is blocked in Atomics.wait. The
	// kernel allocates + owns the SAB pair (so reactor reads work over shared memory,
	// no thread dependency) and asks the main relay to spawn the worker + forward
	// stdin. The agent's stdout/syscalls come back over the SAB.
	const toMain = (m: Record<string, unknown>) => (self as unknown as Worker).postMessage(m);
	const bridge: Record<string, (json: string) => unknown> = {
		createJavascriptContext() {
			contextCounter += 1;
			return { contextId: `agent-ctx-${contextCounter}` };
		},
		createWasmContext() {
			contextCounter += 1;
			return { contextId: `agent-wasm-ctx-${contextCounter}` };
		},
		startExecution(json: string) {
			execCounter += 1;
			const executionId = `agent-exec-${execCounter}`;
			const argv = (parse(json).argv as unknown[] | undefined) ?? [];
			const entrypoint = String(argv[0] ?? "");
			const workerUrl = AGENT_WORKERS[entrypoint] ?? DEFAULT_AGENT_WORKER_URL;
			const upSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
			const downSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
			reactor.register(executionId, { upSab, downSab, layout: LAYOUT });
			// Bind a syscall handler to this agent's VM (the in-flight request's
			// ownership) over the in-worker pushFrame, for its mid-turn syscalls.
			syscallHandlers.set(
				executionId,
				new ConvergedSyncBridgeHandler({
					transport: new PushFrameSidecarTransport({
						pushFrame: (frame) => pushFrame(frame),
						ownership: currentOwnership as never,
						codec: "bare",
					}),
					executionId,
				}),
			);
			toMain({
				type: "spawn-agent",
				executionId,
				workerUrl,
				upSab,
				downSab,
				controlSab,
				layout: LAYOUT,
			});
			agents.add(executionId);
			lastExecutionId = executionId;
			return { executionId };
		},
		writeExecutionStdin(json: string) {
			const request = parse(json);
			const executionId = String(request.executionId ?? "");
			if (agents.has(executionId)) {
				const chunk = decodeBase64(String(request.chunkBase64 ?? ""));
				toMain({ type: "agent-stdin", executionId, chunk });
			}
			return {};
		},
		closeExecutionStdin() {
			return {};
		},
		killExecution(json: string) {
			const request = parse(json);
			const executionId = String(request.executionId ?? "");
			if (agents.has(executionId)) {
				toMain({ type: "kill-agent", executionId });
				reactor.kill(executionId);
				agents.delete(executionId);
			}
			return {};
		},
		// Unused in the resumable path (the kernel drives output via the reactor).
		pollExecutionEvent() {
			return null;
		},
		createWorker() {
			return { workerId: "agent-worker" };
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
	return { bridge, takeLastExecutionId: () => lastExecutionId };
}

let sidecar: InstanceType<KernelWasm["AgentOsBrowserSidecarWasm"]> | null = null;
let reactor: KernelReactor | null = null;
let host: ReturnType<typeof createAsyncAgentHost> | null = null;
let nextDeliverRid = 1_000_000;
// Continuous reactor drive for interactive (long-lived) guests like the PTY shell.
let continuousDriveRunning = false;
function startContinuousDrive(): void {
	if (continuousDriveRunning) return;
	continuousDriveRunning = true;
	const loop = () => {
		try {
			reactor?.drainOnce();
		} catch {
			// A hostile frame kills only the offending execution inside drainOnce.
		}
		setTimeout(loop, 0);
	};
	setTimeout(loop, 0);
}
// The completion channel for DEFERRED syscalls (§6): the MAIN thread runs the async
// host-callback (on-device inference via the chrome-llm adapter) and writes the
// result here; the reactor drains it and unblocks the waiting guest. The kernel owns
// the SAB and hands it to the main relay at boot (controlSab is the GEN the main
// thread bumps so the blocked reactor wakes).
let completionSab: SharedArrayBuffer | null = null;
let controlSab: SharedArrayBuffer | null = null;

// Per-agent syscall handlers (the reactor's serviceSyscall), and the ownership of
// the in-flight request (single-in-flight) used to bind a handler to its VM.
const syscallHandlers = new Map<string, ConvergedSyncBridgeHandler>();
let currentOwnership: unknown = null;

// Service an agent execution's mid-turn syscall against the in-worker kernel. The
// agent sends `{operation, args}` (JSON); we run it through the converged
// guest-syscall handler (op→guest_*_call→pushFrame→result) and return the
// ConvergedSyncResponse as JSON. This runs from the reactor's drainOnce — which the
// drive loop calls OUTSIDE any pushFrame — so the pushFrame here is NOT nested
// (re-entrancy-safe; §3.2.1).
function serviceSyscall(executionId: string, payload: Uint8Array): Uint8Array | typeof DEFERRED {
	const respond = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));
	let request: { operation: string; args: unknown[] };
	try {
		request = decodeSyscall(payload);
	} catch {
		return respond({ error: "invalid syscall payload" });
	}
	// host.inference: the one ASYNC syscall (§6). It is NOT answered here — the kernel
	// asks the MAIN thread to run the on-device-inference host-callback and returns
	// DEFERRED, leaving the guest blocked in its shim. The main thread writes the
	// reply to the completion channel (encodeSyscallCompletion) and the reactor
	// delivers it. Mediated by construction: the guest can only reach the model
	// through this kernel-brokered op, never as an ambient capability.
	if (request.operation === "host.inference") {
		const body = String((request.args ?? [])[0] ?? "");
		(self as unknown as Worker).postMessage({ type: "host-inference", executionId, body });
		return DEFERRED;
	}
	const handler = syscallHandlers.get(executionId);
	if (!handler) return respond({ error: `no syscall handler for ${executionId}` });
	try {
		const result = handler.handle(String(request.operation ?? ""), (request.args ?? []) as unknown[]);
		return respond(result);
	} catch (error) {
		return respond({ error: error instanceof Error ? error.message : String(error), code: (error as { code?: string }).code });
	}
}

async function boot(): Promise<string> {
	controlSab = new SharedArrayBuffer(REACTOR_CONTROL_BYTES);
	completionSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
	reactor = new KernelReactor({
		controlSab,
		now: () => performance.now(),
		serviceSyscall,
		completionSab,
		completionLayout: LAYOUT,
	});
	host = createAsyncAgentHost(reactor, controlSab);
	const wasm = (await import(/* @vite-ignore */ WASM_MODULE_URL)) as KernelWasm;
	await wasm.default(WASM_BINARY_URL);
	sidecar = new wasm.AgentOsBrowserSidecarWasm(host.bridge);
	return sidecar.sidecarId;
}

/** Return the AcpPending processId if `responseBytes` is an ACP pending response. */
function pendingProcessId(responseBytes: Uint8Array): string | null {
	const frame = decodeBareProtocolFrame(responseBytes) as { payload: Record<string, unknown> };
	const payload = frame.payload;
	if (payload.type !== "ext" && payload.type !== "ext_result") return null;
	const env = payload.envelope as { payload: Uint8Array };
	const acp = decodeAcpResponse(env.payload) as { tag: string; val?: { processId?: string } };
	return acp.tag === "AcpPendingResponse" ? (acp.val?.processId ?? null) : null;
}

function buildDeliverFrame(ownership: unknown, processId: string, chunk: Uint8Array): Uint8Array {
	const acp = encodeAcpRequest({
		tag: "AcpDeliverAgentOutputRequest",
		val: { processId, chunk },
	} as never);
	return encodeBareProtocolFrame({
		frame_type: "request",
		schema: SIDECAR_PROTOCOL_SCHEMA,
		request_id: nextDeliverRid++,
		ownership,
		payload: { type: "ext", envelope: { namespace: ACP_NS, payload: acp } },
	} as never);
}

function pushFrame(frame: Uint8Array): Uint8Array {
	const response = sidecar!.pushFrame(frame);
	if (!(response instanceof Uint8Array)) throw new Error("kernel: pushFrame returned no frame");
	return response;
}

self.onmessage = async (event: MessageEvent) => {
	const message = event.data as { type: string; id: number; frame?: Uint8Array };
	try {
		if (message.type === "boot") {
			const sidecarId = await boot();
			// Hand the completion channel + GEN control to the main relay so it can
			// complete DEFERRED inference syscalls (write result + bump GEN to wake the
			// blocked reactor). LAYOUT is fixed/shared, so the relay knows it statically.
			(self as unknown as Worker).postMessage({
				type: "booted",
				id: message.id,
				sidecarId,
				completionSab,
				controlSab,
			});
			return;
		}
		if (message.type === "drive-terminal") {
			// Interactive guests (the PTY shell) keep issuing syscalls forever, but the
			// turn-based driveAgentInteraction only services the reactor during a
			// create_session/prompt pushFrame. Start a continuous drainOnce loop so the
			// agent's mid-life pty.* syscalls are serviced outside any pushFrame.
			startContinuousDrive();
			(self as unknown as Worker).postMessage({ type: "response", id: message.id, frame: new Uint8Array(0) }, []);
			return;
		}
		if (message.type === "frame") {
			const frame = message.frame!;
			// The client (main thread) passes the request's ownership alongside the
			// frame — we can't decode a client-written request here (decodeBareProtocolFrame
			// is for sidecar-written response frames). We need it to build deliver frames.
			const ownership = (message as { ownership?: unknown }).ownership;
			currentOwnership = ownership; // bound to any agent spawned during this pushFrame
			const responseBytes = pushFrame(frame);
			const processId = pendingProcessId(responseBytes);
			if (processId === null) {
				(self as unknown as Worker).postMessage(
					{ type: "response", id: message.id, frame: responseBytes },
					[responseBytes.buffer],
				);
				return;
			}
			// Resumable: drive the agent to completion, then relay the real result.
			const executionId = host!.takeLastExecutionId()!;
			const result = driveAgentInteraction(
				{
					now: () => performance.now(),
					pollAgentOutput: (_pid, deadlineMs) => {
						const out = reactor!.poll(executionId, deadlineMs);
						if (!out) return null;
						const kind = out.kind === FRAME_STDOUT ? "stdout" : out.kind === FRAME_STDERR ? "stderr" : "exit";
						return { kind, payload: out.payload };
					},
					deliverAgentOutput: (pid, chunk) => {
						const respBytes = pushFrame(buildDeliverFrame(ownership, pid, chunk));
						return { pending: pendingProcessId(respBytes) !== null, response: respBytes };
					},
				},
				processId,
				performance.now() + DRIVE_TIMEOUT_MS,
			);
			(self as unknown as Worker).postMessage(
				{ type: "response", id: message.id, frame: result },
				[result.buffer],
			);
		}
	} catch (error) {
		(self as unknown as Worker).postMessage({
			type: "error",
			id: message.id ?? -1,
			message: error instanceof Error ? (error.stack ?? error.message) : String(error),
		});
	}
};
