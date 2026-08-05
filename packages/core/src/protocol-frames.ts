import { toExactUint8Array } from "./bytes.js";
import {
	errorSidecarResponsePayload,
	fromGeneratedSidecarRequestPayload,
	isMatchingSidecarResponsePayload,
	type LiveSidecarRequestPayload,
	type LiveSidecarResponsePayload,
	toGeneratedSidecarResponsePayload,
} from "./callbacks.js";
import {
	fromGeneratedEventPayload,
	type LiveSidecarEventPayload,
} from "./event-buffer.js";
import {
	decodeJsonFramePayload,
	encodeJsonFramePayload,
	type TransportPayloadCodec,
} from "./frame-payload-codec.js";
import type { ClassifiedFrame } from "./frame-rpc.js";
import * as protocol from "./generated-protocol.js";
import { bigIntToSafeNumber } from "./numbers.js";
import {
	fromGeneratedOwnershipScope,
	type LiveOwnershipScope,
	toGeneratedOwnershipScope,
} from "./ownership.js";
import {
	SIDECAR_PROTOCOL_SCHEMA,
	validateSidecarProtocolSchema,
} from "./protocol-schema.js";
import {
	type LiveRequestPayload,
	toGeneratedRequestPayload,
} from "./request-payloads.js";
import {
	fromGeneratedResponsePayload,
	type LiveResponsePayload,
} from "./response-payloads.js";

export interface LiveRequestFrame {
	frame_type: "request";
	schema: typeof SIDECAR_PROTOCOL_SCHEMA;
	request_id: number;
	ownership: LiveOwnershipScope;
	payload: LiveRequestPayload;
}

export interface LiveEventFrame {
	frame_type: "event";
	schema: typeof SIDECAR_PROTOCOL_SCHEMA;
	ownership: LiveOwnershipScope;
	payload: LiveSidecarEventPayload;
}

export interface LiveSidecarRequestFrame {
	frame_type: "sidecar_request";
	schema: typeof SIDECAR_PROTOCOL_SCHEMA;
	request_id: number;
	ownership: LiveOwnershipScope;
	payload: LiveSidecarRequestPayload;
}

export interface LiveResponseFrame {
	frame_type: "response";
	schema: typeof SIDECAR_PROTOCOL_SCHEMA;
	request_id: number;
	ownership: LiveOwnershipScope;
	payload: LiveResponsePayload;
}

export interface LiveSidecarResponseFrame {
	frame_type: "sidecar_response";
	schema: typeof SIDECAR_PROTOCOL_SCHEMA;
	request_id: number;
	ownership: LiveOwnershipScope;
	payload: LiveSidecarResponsePayload;
}

export type LiveControlPayload = {
	type: "shutdown";
	reason: string;
};

export interface LiveControlFrame {
	frame_type: "control";
	schema: typeof SIDECAR_PROTOCOL_SCHEMA;
	payload: LiveControlPayload;
}

export type LiveProtocolFrame =
	| LiveRequestFrame
	| LiveResponseFrame
	| LiveEventFrame
	| LiveSidecarRequestFrame
	| LiveSidecarResponseFrame
	| LiveControlFrame;

export type LiveSidecarWrittenProtocolFrame =
	| LiveResponseFrame
	| LiveEventFrame
	| LiveSidecarRequestFrame;

export type ProtocolFramePayloadCodec = TransportPayloadCodec;

export type ClassifiedSidecarWrittenProtocolFrame = ClassifiedFrame<
	LiveResponseFrame,
	LiveEventFrame,
	LiveSidecarRequestFrame
>;

export type LiveSidecarRequestHandler = (
	request: LiveSidecarRequestFrame,
) => Promise<LiveSidecarResponsePayload> | LiveSidecarResponsePayload;

export class HostProtocolFrameFactory {
	private nextRequestId = 1;

	createRequestFrame(input: {
		ownership: LiveOwnershipScope;
		payload: LiveRequestPayload;
	}): LiveRequestFrame {
		return {
			frame_type: "request",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			request_id: this.nextRequestId++,
			ownership: input.ownership,
			payload: input.payload,
		};
	}

	createSidecarResponseFrame(input: {
		request: LiveSidecarRequestFrame;
		payload: LiveSidecarResponsePayload;
	}): LiveSidecarResponseFrame {
		return {
			frame_type: "sidecar_response",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			request_id: input.request.request_id,
			ownership: input.request.ownership,
			payload: input.payload,
		};
	}

	createControlFrame(payload: LiveControlPayload): LiveControlFrame {
		return {
			frame_type: "control",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			payload,
		};
	}
}

export async function resolveSidecarRequestFramePayload(
	request: LiveSidecarRequestFrame,
	handler: LiveSidecarRequestHandler | null,
): Promise<LiveSidecarResponsePayload> {
	try {
		if (!handler) {
			throw new Error(
				`no sidecar request handler registered for ${request.payload.type}`,
			);
		}
		const payload = await handler(request);
		if (!isMatchingSidecarResponsePayload(request.payload, payload)) {
			throw new Error(
				`sidecar handler returned ${payload.type} for ${request.payload.type}`,
			);
		}
		return payload;
	} catch (error) {
		return errorSidecarResponsePayload(request.payload, error);
	}
}

export function toGeneratedProtocolFrame(
	frame: LiveProtocolFrame,
): protocol.ProtocolFrame {
	switch (frame.frame_type) {
		case "request":
			return {
				tag: "RequestFrame",
				val: {
					schema: frame.schema,
					requestId: BigInt(frame.request_id),
					ownership: toGeneratedOwnershipScope(frame.ownership),
					payload: toGeneratedRequestPayload(frame.payload),
				},
			};
		case "sidecar_response":
			return {
				tag: "SidecarResponseFrame",
				val: {
					schema: frame.schema,
					requestId: BigInt(frame.request_id),
					ownership: toGeneratedOwnershipScope(frame.ownership),
					payload: toGeneratedSidecarResponsePayload(frame.payload),
				},
			};
		case "control":
			return {
				tag: "ControlFrame",
				val: {
					schema: frame.schema,
					payload: toGeneratedControlPayload(frame.payload),
				},
			};
		case "response":
		case "event":
		case "sidecar_request":
			throw new Error(
				`BARE encoding is only implemented for host-written frames, received ${frame.frame_type}`,
			);
	}
}

export function encodeBareProtocolFrame(frame: LiveProtocolFrame): Uint8Array {
	return protocol.encodeProtocolFrame(toGeneratedProtocolFrame(frame));
}

export function decodeBareProtocolFrame(
	payload: Uint8Array,
): LiveSidecarWrittenProtocolFrame {
	return fromGeneratedSidecarWrittenProtocolFrame(
		protocol.decodeProtocolFrame(toExactUint8Array(payload)),
	);
}

export function encodeProtocolFramePayload(
	frame: LiveProtocolFrame,
	codec: ProtocolFramePayloadCodec,
): Uint8Array {
	if (codec === "json") {
		return encodeJsonFramePayload(frame);
	}
	return encodeBareProtocolFrame(frame);
}

export function decodeProtocolFramePayload(
	payload: Uint8Array,
	codec: ProtocolFramePayloadCodec,
): LiveSidecarWrittenProtocolFrame {
	if (codec === "json") {
		return decodeJsonFramePayload<LiveSidecarWrittenProtocolFrame>(payload);
	}
	return decodeBareProtocolFrame(payload);
}

export function classifySidecarWrittenProtocolFrame(
	frame: LiveSidecarWrittenProtocolFrame,
): ClassifiedSidecarWrittenProtocolFrame {
	switch (frame.frame_type) {
		case "response":
			return {
				kind: "response",
				requestId: frame.request_id,
				frame,
			};
		case "event":
			return { kind: "event", frame };
		case "sidecar_request":
			return { kind: "sidecarRequest", frame };
	}
}

export function fromGeneratedSidecarWrittenProtocolFrame(
	frame: protocol.ProtocolFrame,
): LiveSidecarWrittenProtocolFrame {
	switch (frame.tag) {
		case "ResponseFrame":
			return {
				frame_type: "response",
				schema: toLiveProtocolSchema(frame.val.schema),
				request_id: bigIntToSafeNumber(
					frame.val.requestId,
					"response request id",
				),
				ownership: fromGeneratedOwnershipScope(frame.val.ownership),
				payload: fromGeneratedResponsePayload(frame.val.payload),
			};
		case "EventFrame":
			return {
				frame_type: "event",
				schema: toLiveProtocolSchema(frame.val.schema),
				ownership: fromGeneratedOwnershipScope(frame.val.ownership),
				payload: fromGeneratedEventPayload(frame.val.payload),
			};
		case "SidecarRequestFrame":
			return {
				frame_type: "sidecar_request",
				schema: toLiveProtocolSchema(frame.val.schema),
				request_id: bigIntToSafeNumber(
					frame.val.requestId,
					"sidecar request id",
				),
				ownership: fromGeneratedOwnershipScope(frame.val.ownership),
				payload: fromGeneratedSidecarRequestPayload(frame.val.payload),
			};
		case "RequestFrame":
		case "SidecarResponseFrame":
		case "ControlFrame":
			throw new Error(`unsupported BARE protocol frame tag: ${frame.tag}`);
	}
}

export function toGeneratedControlPayload(
	payload: LiveControlPayload,
): protocol.ControlPayload {
	switch (payload.type) {
		case "shutdown":
			return {
				tag: "ShutdownControl",
				val: { reason: payload.reason },
			};
	}
}

export function toLiveProtocolSchema(
	schema: protocol.ProtocolSchema,
): typeof SIDECAR_PROTOCOL_SCHEMA {
	return validateSidecarProtocolSchema(schema);
}
