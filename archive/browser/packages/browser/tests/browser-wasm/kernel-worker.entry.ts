// M2a harness (esbuild-bundled): boots the Agent OS kernel INSIDE a worker and
// drives a wire-frame round-trip through a main-thread async relay — proving the
// converged kernel runs in a worker (spec §3.1), the foundation for the async-agent
// executor. Uses postMessage relay (no SAB yet; guest syscalls are M2b).

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import {
	decodeBareProtocolFrame,
	encodeBareProtocolFrame,
} from "@rivet-dev/agentos-runtime-core/protocol-frames";
import { SIDECAR_PROTOCOL_SCHEMA } from "@rivet-dev/agentos-runtime-core/protocol-schema";
import {
	decodeAcpResponse,
	encodeAcpRequest,
} from "../../../core/src/sidecar/agentos-acp-protocol.ts";

const WASM_MODULE_URL = "/wasm/agentos_sidecar_browser.js";
const WASM_BINARY_URL = "/wasm/agentos_sidecar_browser_bg.wasm";
const ACP_NS = "dev.rivet.agent-os.acp";

/** Main-thread async relay to the in-worker kernel. */
class KernelWorkerRelay {
	private readonly worker: Worker;
	private nextId = 1;
	private readonly pending = new Map<
		number,
		{ resolve: (v: unknown) => void; reject: (e: unknown) => void }
	>();

	constructor(workerUrl: string) {
		this.worker = new Worker(workerUrl, { type: "module" });
		this.worker.onmessage = (event: MessageEvent) => {
			const m = event.data as { type: string; id: number; [k: string]: unknown };
			const entry = this.pending.get(m.id);
			if (!entry) return;
			this.pending.delete(m.id);
			if (m.type === "error") entry.reject(new Error(String(m.message)));
			else entry.resolve(m);
		};
		this.worker.onerror = (e) => {
			for (const { reject } of this.pending.values()) reject(e);
			this.pending.clear();
		};
	}

	private call<T>(message: Record<string, unknown>, transfer: Transferable[] = []): Promise<T> {
		const id = this.nextId++;
		return new Promise<T>((resolve, reject) => {
			this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
			this.worker.postMessage({ ...message, id }, transfer);
		});
	}

	async boot(): Promise<string> {
		const r = await this.call<{ sidecarId: string }>({
			type: "boot",
			moduleUrl: WASM_MODULE_URL,
			binaryUrl: WASM_BINARY_URL,
		});
		return r.sidecarId;
	}

	async pushFrame(frame: Uint8Array): Promise<Uint8Array> {
		const r = await this.call<{ frame: Uint8Array }>({ type: "frame", frame }, [frame.buffer]);
		return r.frame;
	}
}

let nextRequestId = 1;

function authenticateFrame(): Uint8Array {
	return encodeBareProtocolFrame({
		frame_type: "request",
		schema: SIDECAR_PROTOCOL_SCHEMA,
		request_id: nextRequestId++,
		ownership: { scope: "connection", connection_id: "conn-1" },
		payload: {
			type: "authenticate",
			client_name: "agentos-vm-kernel-worker-test",
			auth_token: "",
			protocol_version: SIDECAR_PROTOCOL_SCHEMA.version,
			bridge_version: 1,
		},
	} as never);
}

function getSessionStateFrame(): Uint8Array {
	const payload = encodeAcpRequest({
		tag: "AcpGetSessionStateRequest",
		val: { sessionId: "does-not-exist" },
	} as never);
	return encodeBareProtocolFrame({
		frame_type: "request",
		schema: SIDECAR_PROTOCOL_SCHEMA,
		request_id: nextRequestId++,
		ownership: {
			scope: "vm",
			connection_id: "conn-1",
			session_id: "session-1",
			vm_id: "vm-1",
		},
		payload: { type: "ext", envelope: { namespace: ACP_NS, payload } },
	} as never);
}

function decodeResponse(bytes: Uint8Array): {
	payloadType?: string;
	acpTag?: string;
	acpMessage?: string;
} {
	const frame = decodeBareProtocolFrame(bytes) as { payload: Record<string, unknown> };
	const payload = frame.payload;
	const out: { payloadType?: string; acpTag?: string; acpMessage?: string } = {
		payloadType: payload.type as string,
	};
	if (payload.type === "ext" || payload.type === "ext_result") {
		const env = payload.envelope as { payload: Uint8Array };
		const acp = decodeAcpResponse(env.payload) as { tag: string; val?: { message?: string } };
		out.acpTag = acp.tag;
		out.acpMessage = acp.val?.message;
	}
	return out;
}

(globalThis as unknown as { __kernelWorker: unknown }).__kernelWorker = {
	async run() {
		const relay = new KernelWorkerRelay("/agentos-vm-kernel.worker.js");
		const sidecarId = await relay.boot();
		const authResp = decodeResponse(await relay.pushFrame(authenticateFrame()));
		const acpResp = decodeResponse(await relay.pushFrame(getSessionStateFrame()));
		return { sidecarId, authResp, acpResp };
	},
};

const status = document.getElementById("status");
if (status) status.textContent = "ready";
