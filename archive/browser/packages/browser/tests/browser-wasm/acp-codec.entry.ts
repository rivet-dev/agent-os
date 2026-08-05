// Bundled (esbuild) ACP wire-codec harness for the Chromium ACP round-trip test.
// Builds real wire frames (authenticate + an ACP ext request) using the live
// @rivet-dev/agentos-runtime-core protocol codec + agent-os's generated ACP encoders, drives them
// through the agentos wasm sidecar's pushFrame, and decodes the ACP response. This
// proves a real ACP request/response round-trip through the converged wasm sidecar
// (BrowserAcpExtension -> AcpCore) in the browser.

// The @rivet-dev/agentos-runtime-core codec uses Node's Buffer for the ext-envelope `data` field;
// provide it in the browser so frame decoding works.
import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import { decodeBareProtocolFrame, encodeBareProtocolFrame } from "@rivet-dev/agentos-runtime-core/protocol-frames";
import { SIDECAR_PROTOCOL_SCHEMA } from "@rivet-dev/agentos-runtime-core/protocol-schema";
// agent-os's generated ACP BARE encoders (not exported from the package; imported by path).
import {
	decodeAcpResponse,
	encodeAcpRequest,
} from "../../../core/src/sidecar/agentos-acp-protocol.ts";

const ACP_NS = "dev.rivet.agent-os.acp";
const OWNERSHIP = {
	scope: "vm" as const,
	connection_id: "conn-1",
	session_id: "session-1",
	vm_id: "vm-1",
};

type PushFrame = (frame: Uint8Array) => Uint8Array;

let nextRequestId = 1;

function authenticateFrame(): Uint8Array {
	return encodeBareProtocolFrame({
		frame_type: "request",
		schema: SIDECAR_PROTOCOL_SCHEMA,
		request_id: nextRequestId++,
		ownership: { scope: "connection", connection_id: OWNERSHIP.connection_id },
		payload: {
			type: "authenticate",
			client_name: "agentos-browser-test",
			auth_token: "",
			protocol_version: SIDECAR_PROTOCOL_SCHEMA.version,
			bridge_version: 1,
		},
	} as never);
}

function acpGetSessionStateFrame(sessionId: string): Uint8Array {
	const payload = encodeAcpRequest({
		tag: "AcpGetSessionStateRequest",
		val: { sessionId },
	});
	return encodeBareProtocolFrame({
		frame_type: "request",
		schema: SIDECAR_PROTOCOL_SCHEMA,
		request_id: nextRequestId++,
		ownership: OWNERSHIP,
		payload: { type: "ext", envelope: { namespace: ACP_NS, payload } },
	} as never);
}

function decodeResponse(bytes: Uint8Array): {
	frameType: string;
	payloadType?: string;
	acpTag?: string;
	acpMessage?: string;
	rejected?: { code?: string; message?: string };
} {
	const frame = decodeBareProtocolFrame(bytes) as {
		frame_type: string;
		payload: Record<string, unknown>;
	};
	const payload = frame.payload;
	const out: ReturnType<typeof decodeResponse> = {
		frameType: frame.frame_type,
		payloadType: payload.type as string,
	};
	if (payload.type === "ext" || payload.type === "ext_result") {
		const env = payload.envelope as { payload: Uint8Array };
		const acp = decodeAcpResponse(env.payload) as { tag: string; val?: { code?: string; message?: string } };
		out.acpTag = acp.tag;
		out.acpMessage = acp.val?.message;
	} else if (payload.type === "rejected") {
		out.rejected = {
			code: payload.code as string,
			message: payload.message as string,
		};
	}
	return out;
}

(globalThis as unknown as { __acpHarness: unknown }).__acpHarness = {
	runGetSessionStateRoundTrip(pushFrame: PushFrame) {
		const authResp = decodeResponse(pushFrame(authenticateFrame()));
		const acpResp = decodeResponse(pushFrame(acpGetSessionStateFrame("does-not-exist")));
		return { authResp, acpResp };
	},
};
