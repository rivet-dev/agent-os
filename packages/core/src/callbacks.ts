import {
	fromGeneratedExtEnvelope,
	toGeneratedExtEnvelope,
} from "./ext.js";
import type * as protocol from "./generated-protocol.js";
import { parseJsonUtf8, stringifyJsonUtf8 } from "./json.js";
import { bigIntToSafeNumber } from "./numbers.js";

export interface CallbackExtEnvelope {
	namespace: string;
	payload: Uint8Array;
}

export type CallbackSidecarRequestPayload =
	| {
			type: "host_callback";
			invocation_id: string;
			callback_key: string;
			input: unknown;
			timeout_ms: number;
	  }
	| {
			type: "js_bridge_call";
			call_id: string;
			mount_id: string;
			operation: string;
			args: unknown;
	  }
	| {
			type: "ext";
			envelope: CallbackExtEnvelope;
	  };

export type CallbackSidecarResponsePayload =
	| {
			type: "host_callback_result";
			invocation_id: string;
			result?: unknown;
			error?: string;
	  }
	| {
			type: "js_bridge_result";
			call_id: string;
			result?: unknown;
			error?: string;
	  }
	| {
			type: "ext_result";
			envelope: CallbackExtEnvelope;
	  };

export type LiveSidecarRequestPayload = CallbackSidecarRequestPayload;
export type LiveSidecarResponsePayload = CallbackSidecarResponsePayload;

export function isMatchingSidecarResponsePayload(
	request: CallbackSidecarRequestPayload,
	response: CallbackSidecarResponsePayload,
): boolean {
	switch (request.type) {
		case "host_callback":
			return response.type === "host_callback_result";
		case "js_bridge_call":
			return response.type === "js_bridge_result";
		case "ext":
			return response.type === "ext_result";
	}
}

export function errorSidecarResponsePayload(
	request: CallbackSidecarRequestPayload,
	error: unknown,
): CallbackSidecarResponsePayload {
	const message = error instanceof Error ? error.message : String(error);
	switch (request.type) {
		case "host_callback":
			return {
				type: "host_callback_result",
				invocation_id: request.invocation_id,
				error: message,
			};
		case "js_bridge_call":
			return {
				type: "js_bridge_result",
				call_id: request.call_id,
				error: message,
			};
		case "ext":
			return {
				type: "ext_result",
				envelope: {
					namespace: request.envelope.namespace,
					payload: Buffer.from(message, "utf8"),
				},
			};
	}
}

export function fromGeneratedSidecarRequestPayload(
	payload: protocol.SidecarRequestPayload,
): LiveSidecarRequestPayload {
	switch (payload.tag) {
		case "HostCallbackRequest":
			return {
				type: "host_callback",
				invocation_id: payload.val.invocationId,
				callback_key: payload.val.callbackKey,
				input: parseJsonUtf8(payload.val.input, "host callback input"),
				timeout_ms: bigIntToSafeNumber(
					payload.val.timeoutMs,
					"host callback timeout",
				),
			};
		case "JsBridgeCallRequest":
			return {
				type: "js_bridge_call",
				call_id: payload.val.callId,
				mount_id: payload.val.mountId,
				operation: payload.val.operation,
				args: parseJsonUtf8(payload.val.args, "js bridge call args"),
			};
		case "ExtEnvelope":
			return {
				type: "ext",
				envelope: fromGeneratedExtEnvelope(payload.val),
			};
	}
}

export function toGeneratedSidecarResponsePayload(
	payload: LiveSidecarResponsePayload,
): protocol.SidecarResponsePayload {
	switch (payload.type) {
		case "host_callback_result":
			return {
				tag: "HostCallbackResultResponse",
				val: {
					invocationId: payload.invocation_id,
					result:
						payload.result === undefined
							? null
							: stringifyJsonUtf8(
									payload.result,
									"host_callback_result.result",
								),
					error: payload.error ?? null,
				},
			};
		case "js_bridge_result":
			return {
				tag: "JsBridgeResultResponse",
				val: {
					callId: payload.call_id,
					result:
						payload.result === undefined
							? null
							: stringifyJsonUtf8(payload.result, "js_bridge_result.result"),
					error: payload.error ?? null,
				},
			};
		case "ext_result":
			return {
				tag: "ExtEnvelope",
				val: toGeneratedExtEnvelope(payload.envelope),
			};
	}
}
