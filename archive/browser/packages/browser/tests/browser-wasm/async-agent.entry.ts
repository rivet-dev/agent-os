// M3 gate harness: a full create_session round-trip against an ASYNC agent running
// in its own worker, driven by the resumable path through the in-worker kernel.
// Proves the re-entrancy fix end-to-end: AcpCore drives an async agent (which
// replies on the event loop, not synchronously) without block-waiting inside
// pushFrame. The agent here is syscall-free; the hardened (mid-turn syscall) variant
// is a follow-up.

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import {
	decodeAcpResponse,
	encodeAcpRequest,
} from "../../../core/src/sidecar/agentos-acp-protocol.ts";
import { ACP_NAMESPACE as ACP_NS, KernelWorkerRelay, bootstrapVm, send } from "./async-harness.js";

(globalThis as unknown as { __asyncAgent: unknown }).__asyncAgent = {
	async run() {
		const relay = new KernelWorkerRelay("/async-kernel.worker.js");
		const sidecarId = await relay.boot();
		const vm = await bootstrapVm(relay);

		const acp = encodeAcpRequest({
			tag: "AcpCreateSessionRequest",
			val: {
				agentType: "async-echo",
				runtime: "JavaScript",
				adapterEntrypoint: "/bin/async-echo-agent",
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
		const payload = await send(
			relay,
			{ scope: "vm", connection_id: vm.connectionId, session_id: vm.sessionId, vm_id: vm.vmId },
			{ type: "ext", envelope: { namespace: ACP_NS, payload: acp } },
		);

		const out: {
			sidecarId: string;
			payloadType?: string;
			acpTag?: string;
			sessionId?: string;
			acpMessage?: string;
			promptContent?: string;
		} = { sidecarId, payloadType: payload.type as string };
		const vmOwnership = { scope: "vm", connection_id: vm.connectionId, session_id: vm.sessionId, vm_id: vm.vmId };
		if (payload.type === "ext" || payload.type === "ext_result") {
			const env = payload.envelope as { payload: Uint8Array };
			const decoded = decodeAcpResponse(env.payload) as { tag: string; val?: { sessionId?: string; message?: string } };
			out.acpTag = decoded.tag;
			out.sessionId = decoded.val?.sessionId;
			out.acpMessage = decoded.val?.message;
		}

		// HARDENED: a session/prompt during which the agent makes a mid-turn fs
		// syscall (serviced inline by the reactor). The prompt result echoes the file
		// content, proving the syscall round-trip happened during the turn.
		if (out.acpTag === "AcpSessionCreatedResponse" && out.sessionId) {
			const promptAcp = encodeAcpRequest({
				tag: "AcpSessionRequest",
				val: {
					sessionId: out.sessionId,
					method: "session/prompt",
					params: JSON.stringify({ prompt: [{ type: "text", text: "go" }] }),
				},
			} as never);
			const promptPayload = await send(relay, vmOwnership, {
				type: "ext",
				envelope: { namespace: ACP_NS, payload: promptAcp },
			});
			if (promptPayload.type === "ext" || promptPayload.type === "ext_result") {
				const env = promptPayload.envelope as { payload: Uint8Array };
				const decoded = decodeAcpResponse(env.payload) as { tag: string; val?: { response?: string } };
				if (decoded.tag === "AcpSessionRpcResponse" && decoded.val?.response) {
					const rpc = JSON.parse(decoded.val.response) as { result?: { content?: string } };
					out.promptContent = rpc.result?.content;
				}
			}
		}
		return out;
	},
};

const status = document.getElementById("status");
if (status) status.textContent = "ready";
