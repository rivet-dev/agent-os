// Shared main-thread harness for the async-agent gates (AGENTOS-WEB-ASYNC-AGENTS.md
// §3.2 / §6). The kernel runs in its own worker; this is the relay + VM bootstrap the
// gate entries drive it through. Two responsibilities live here:
//
//  1. Agent execution workers are spawned HERE on the main thread (review F11): the
//     kernel worker cannot spawn a nested worker while blocked in Atomics.wait. The
//     kernel owns the SABs and asks us to spawn + forward stdin; the agent's
//     stdout/syscalls flow back to the kernel over the shared SABs.
//  2. DEFERRED inference syscalls (§6) are completed HERE: the kernel posts
//     `host-inference`; we run the on-device-inference host-callback (the chrome-llm
//     adapter, mock or real LanguageModel), write the reply to the kernel's completion
//     channel, and bump GEN so the blocked reactor wakes and delivers it.

import {
	decodeBareProtocolFrame,
	encodeBareProtocolFrame,
} from "@rivet-dev/agentos-runtime-core/protocol-frames";
import { SIDECAR_PROTOCOL_SCHEMA } from "@rivet-dev/agentos-runtime-core/protocol-schema";
import {
	SabRing,
	type SabRingLayout,
	encodeSyscallCompletion,
} from "@rivet-dev/agentos-runtime-browser";
import {
	handleChatCompletion,
	type LanguageModelSession,
} from "../../src/chrome-llm-adapter.js";
import {
	decodeAcpResponse,
	encodeAcpRequest,
} from "../../../core/src/sidecar/agentos-acp-protocol.ts";

const ACP_NS = "dev.rivet.agent-os.acp";
// Must match the kernel worker's LAYOUT (the completion ring is shared memory).
const LAYOUT: SabRingLayout = { slotCount: 64, slotBytes: 4096 };

let nextRequestId = 1;

export class KernelWorkerRelay {
	private readonly worker: Worker;
	private id = 1;
	private readonly pending = new Map<number, { resolve: (v: any) => void; reject: (e: any) => void }>();
	private readonly agents = new Map<string, Worker>();
	// Completion channel for DEFERRED inference syscalls; populated from the kernel's
	// `booted` message. Null until boot resolves.
	private completion: SabRing | null = null;
	private control: Int32Array | null = null;
	/** Direct main-thread <-> agent-worker channel for interactive guests (the PTY
	 * terminal). Agent workers are spawned HERE, so the host can postMessage them
	 * straight (out-of-band of the SAB/ACP path) for live keystroke/output streaming.
	 * Set before create_session so the first agent message is observed. */
	public onAgentMessage: ((executionId: string, data: unknown) => void) | null =
		null;
	/** Execution id of the most recently spawned agent worker. */
	public lastAgentExecutionId: string | null = null;

	/** @param inferenceSession the on-device model the host-callback drives (a mock
	 * sentinel for the CI gate, the real `LanguageModel` for the Nano smoke). When
	 * null, a `host-inference` callback errors (no agent should issue one). */
	constructor(
		url: string,
		private readonly inferenceSession: LanguageModelSession | null = null,
	) {
		this.worker = new Worker(url, { type: "module" });
		this.worker.onmessage = (e: MessageEvent) => this.onMessage(e);
	}

	private onMessage(e: MessageEvent): void {
		const m = e.data as { type: string; id: number; executionId?: string };
		if (m.type === "spawn-agent") {
			const s = m as unknown as {
				executionId: string;
				workerUrl: string;
				upSab: SharedArrayBuffer;
				downSab: SharedArrayBuffer;
				controlSab: SharedArrayBuffer;
				layout: unknown;
			};
			const agent = new Worker(s.workerUrl, { type: "module" });
			agent.onmessage = (ev: MessageEvent) =>
				this.onAgentMessage?.(s.executionId, ev.data);
			agent.postMessage({ type: "init", upSab: s.upSab, downSab: s.downSab, controlSab: s.controlSab, layout: s.layout });
			this.agents.set(s.executionId, agent);
			this.lastAgentExecutionId = s.executionId;
			return;
		}
		if (m.type === "agent-stdin") {
			const s = m as unknown as { executionId: string; chunk: Uint8Array };
			this.agents.get(s.executionId)?.postMessage({ type: "stdin", chunk: s.chunk });
			return;
		}
		if (m.type === "kill-agent") {
			this.agents.get(m.executionId!)?.terminate();
			this.agents.delete(m.executionId!);
			return;
		}
		if (m.type === "host-inference") {
			void this.completeInference(m as unknown as { executionId: string; body: string });
			return;
		}
		const entry = this.pending.get(m.id);
		if (!entry) return;
		this.pending.delete(m.id);
		if (m.type === "error") entry.reject(new Error(String((m as any).message)));
		else entry.resolve(m);
	}

	// Run one async host-callback to the on-device model and deliver the reply to the
	// blocked guest via the kernel's completion channel. This is the single async hop
	// of the inference path (§6); everything else (the guest's net/fs syscalls) is
	// synchronous over the SAB.
	private async completeInference(m: { executionId: string; body: string }): Promise<void> {
		if (!this.completion || !this.control) throw new Error("relay: completion channel not ready");
		const responseJson = this.inferenceSession
			? await handleChatCompletion(m.body, this.inferenceSession)
			: JSON.stringify({ error: { type: "no_model", message: "no inference session bound" } });
		const result = new TextEncoder().encode(responseJson);
		if (!this.completion.tryWrite(encodeSyscallCompletion(m.executionId, result))) {
			throw new Error("relay: completion ring full");
		}
		// Bump GEN + notify so the reactor (blocked in Atomics.wait) wakes and drains.
		Atomics.add(this.control, 0, 1);
		Atomics.notify(this.control, 0);
	}

	private call<T>(message: Record<string, unknown>, transfer: Transferable[] = []): Promise<T> {
		const id = this.id++;
		return new Promise<T>((resolve, reject) => {
			this.pending.set(id, { resolve, reject });
			this.worker.postMessage({ ...message, id }, transfer);
		});
	}

	async boot(): Promise<string> {
		const booted = await this.call<{
			sidecarId: string;
			completionSab: SharedArrayBuffer;
			controlSab: SharedArrayBuffer;
		}>({ type: "boot" });
		this.completion = new SabRing(booted.completionSab, LAYOUT);
		this.control = new Int32Array(booted.controlSab, 0, 1);
		return booted.sidecarId;
	}

	async pushFrame(frame: Uint8Array, ownership: unknown): Promise<Uint8Array> {
		return (await this.call<{ frame: Uint8Array }>({ type: "frame", frame, ownership }, [frame.buffer])).frame;
	}

	/** Post a message straight to a spawned agent worker (interactive PTY channel). */
	postToAgent(executionId: string, message: unknown): void {
		this.agents.get(executionId)?.postMessage(message);
	}

	/** Start the kernel worker's continuous reactor drive so a long-lived interactive
	 * agent's mid-life syscalls are serviced outside any pushFrame turn. */
	async driveTerminal(): Promise<void> {
		await this.call<{ frame: Uint8Array }>({ type: "drive-terminal" });
	}
}

export async function send(
	relay: KernelWorkerRelay,
	ownership: unknown,
	payload: unknown,
): Promise<Record<string, unknown>> {
	const responseBytes = await relay.pushFrame(
		encodeBareProtocolFrame({
			frame_type: "request",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			request_id: nextRequestId++,
			ownership,
			payload,
		} as never),
		ownership,
	);
	return (decodeBareProtocolFrame(responseBytes) as { payload: Record<string, unknown> }).payload;
}

export async function bootstrapVm(relay: KernelWorkerRelay) {
	const agentPackages = [
		["async-echo", "async-echo-agent"],
		["async-infer", "async-infer-agent"],
		["async-loopback", "async-loopback-agent"],
		["async-proxy", "async-proxy-agent"],
		["pty-loopback", "pty-loopback-agent"],
	] as const;
	const agentPackageEntries = agentPackages.flatMap(([name, acpEntrypoint]) => [
		{
			path: `/opt/agentos/pkgs/${name}/current/agentos-package.json`,
			kind: "file",
			mode: 0o644,
			uid: 0,
			gid: 0,
			content: JSON.stringify({
				name,
				version: "1.0.0",
				agent: { acpEntrypoint },
			}),
			encoding: "utf8",
			executable: false,
		},
		{
			path: `/opt/agentos/bin/${acpEntrypoint}`,
			kind: "file",
			mode: 0o755,
			uid: 0,
			gid: 0,
			content: "",
			encoding: "utf8",
			executable: true,
		},
	]);
	const authed = await send(
		relay,
		{ scope: "connection", connection_id: "client-hint" },
		{
			type: "authenticate",
			client_name: "async-agent-test",
			auth_token: "",
			protocol_version: SIDECAR_PROTOCOL_SCHEMA.version,
			bridge_version: 1,
		},
	);
	const connectionId = authed.connection_id as string;
	const opened = await send(
		relay,
		{ scope: "connection", connection_id: connectionId },
		{ type: "open_session", placement: { kind: "shared", pool: null }, metadata: {} },
	);
	const sessionId = opened.session_id as string;
	const created = await send(
		relay,
		{ scope: "session", connection_id: connectionId, session_id: sessionId },
		{
			type: "create_vm",
			runtime: "java_script",
			config: {
				rootFilesystem: {
					mode: "ephemeral",
					disableDefaultBaseLayer: false,
					lowers: [],
					bootstrapEntries: agentPackageEntries,
				},
				permissions: { fs: "allow", network: "allow", childProcess: "allow", process: "allow", env: "allow", binding: "allow" },
			},
		},
	);
	return { connectionId, sessionId, vmId: created.vm_id as string };
}

export const ACP_NAMESPACE = ACP_NS;

export interface SessionPromptGateResult {
	sidecarId: string;
	payloadType?: string;
	acpTag?: string;
	sessionId?: string;
	acpCode?: string;
	acpMessage?: string;
	promptContent?: string;
}

export interface PersistentAgentSession {
	sidecarId: string;
	sessionId: string;
	executionId: string;
}

/** Boot + bootstrap a VM + create an ACP session, which spawns the agent worker, then
 * STOP (no prompt) so the host can drive the still-running agent directly over the
 * out-of-band agent channel. Used by the interactive PTY terminal: the shell agent
 * stays alive and the page streams keystrokes/output to it via the relay. */
export async function createPersistentAgentSession(
	relay: KernelWorkerRelay,
	opts: { agentType: string; adapterEntrypoint: string },
): Promise<PersistentAgentSession> {
	const sidecarId = await relay.boot();
	const vm = await bootstrapVm(relay);
	const vmOwnership = { scope: "vm", connection_id: vm.connectionId, session_id: vm.sessionId, vm_id: vm.vmId };

	const createAcp = encodeAcpRequest({
		tag: "AcpCreateSessionRequest",
		val: {
			agentType: opts.agentType,
			runtime: "JavaScript",
			adapterEntrypoint: opts.adapterEntrypoint,
			cwd: "/workspace",
			args: [],
			env: new Map<string, string>(),
			protocolVersion: 1,
			clientCapabilities: "{}",
			mcpServers: "[]",
			skipOsInstructions: false,
			additionalInstructions: null,
		},
	} as never);
	const created = await send(relay, vmOwnership, {
		type: "ext",
		envelope: { namespace: ACP_NS, payload: createAcp },
	});

	let sessionId = "";
	if (created.type === "ext" || created.type === "ext_result") {
		const env = created.envelope as { payload: Uint8Array };
		const decoded = decodeAcpResponse(env.payload) as { tag: string; val?: { sessionId?: string } };
		sessionId = decoded.val?.sessionId ?? "";
	}
	const executionId = relay.lastAgentExecutionId;
	if (!executionId) throw new Error("no agent worker was spawned for the session");
	return { sidecarId, sessionId, executionId };
}

/** Drive a complete gate against an async agent: boot, bootstrap a VM, create_session
 * (resumable), then session/prompt, returning the created session id + the prompt's
 * result content. Shared by the inference + loopback gates (they differ only in the
 * agent they launch and what the prompt content proves). */
export async function runSessionPromptGate(
	relay: KernelWorkerRelay,
	opts: { agentType: string; adapterEntrypoint: string; promptText: string },
): Promise<SessionPromptGateResult> {
	const sidecarId = await relay.boot();
	const vm = await bootstrapVm(relay);
	const vmOwnership = { scope: "vm", connection_id: vm.connectionId, session_id: vm.sessionId, vm_id: vm.vmId };

	const createAcp = encodeAcpRequest({
		tag: "AcpCreateSessionRequest",
		val: {
			agentType: opts.agentType,
			runtime: "JavaScript",
			adapterEntrypoint: opts.adapterEntrypoint,
			cwd: "/workspace",
			args: [],
			env: new Map<string, string>(),
			protocolVersion: 1,
			clientCapabilities: "{}",
			mcpServers: "[]",
			skipOsInstructions: false,
			additionalInstructions: null,
		},
	} as never);
	const created = await send(relay, vmOwnership, {
		type: "ext",
		envelope: { namespace: ACP_NS, payload: createAcp },
	});

	const out: SessionPromptGateResult = { sidecarId, payloadType: created.type as string };
	if (created.type === "ext" || created.type === "ext_result") {
		const env = created.envelope as { payload: Uint8Array };
		const decoded = decodeAcpResponse(env.payload) as {
			tag: string;
			val?: { sessionId?: string; code?: string; message?: string };
		};
		out.acpTag = decoded.tag;
		out.sessionId = decoded.val?.sessionId;
		out.acpCode = decoded.val?.code;
		out.acpMessage = decoded.val?.message;
	}
	if (out.acpTag !== "AcpSessionCreatedResponse" || !out.sessionId) return out;

	const promptAcp = encodeAcpRequest({
		tag: "AcpSessionRequest",
		val: {
			sessionId: out.sessionId,
			method: "session/prompt",
			params: JSON.stringify({ prompt: [{ type: "text", text: opts.promptText }] }),
		},
	} as never);
	const prompted = await send(relay, vmOwnership, {
		type: "ext",
		envelope: { namespace: ACP_NS, payload: promptAcp },
	});
	if (prompted.type === "ext" || prompted.type === "ext_result") {
		const env = prompted.envelope as { payload: Uint8Array };
		const decoded = decodeAcpResponse(env.payload) as { tag: string; val?: { response?: string } };
		if (decoded.tag === "AcpSessionRpcResponse" && decoded.val?.response) {
			const rpc = JSON.parse(decoded.val.response) as { result?: { content?: string } };
			out.promptContent = rpc.result?.content;
		}
	}
	return out;
}
