import { describe, expect, it } from "vitest";
import * as protocol from "../src/generated-protocol.js";
import {
	classifySidecarWrittenProtocolFrame,
	decodeBareProtocolFrame,
	decodeProtocolFramePayload,
	encodeBareProtocolFrame,
	encodeProtocolFramePayload,
	fromGeneratedSidecarWrittenProtocolFrame,
	HostProtocolFrameFactory,
	resolveSidecarRequestFramePayload,
	toGeneratedProtocolFrame,
} from "../src/protocol-frames.js";
import { SIDECAR_PROTOCOL_SCHEMA } from "../src/protocol-schema.js";

const textDecoder = new TextDecoder();

const ownership = {
	scope: "connection" as const,
	connection_id: "conn",
};

const generatedAuthOwnership = {
	scope: "connection" as const,
	connection_id: "conn-1",
};

const GENERATED_AUTH_FRAME_HEX =
	"000f6167656e746f732d73696465636172080007000000000000000006636f6e6e2d31000e67656e6572617465642d7465737405746f6b656e080001000000";

const hostCallbackRequest = {
	frame_type: "sidecar_request" as const,
	schema: SIDECAR_PROTOCOL_SCHEMA,
	request_id: 7,
	ownership,
	payload: {
		type: "host_callback" as const,
		invocation_id: "invocation",
		callback_key: "tool",
		input: {},
		timeout_ms: 1000,
	},
};

describe("protocol frame conversion", () => {
	it("creates host-written request, response, and control frames", () => {
		const factory = new HostProtocolFrameFactory();

		const first = factory.createRequestFrame({
			ownership,
			payload: {
				type: "authenticate",
				client_name: "agentos",
				auth_token: "token",
				protocol_version: 8,
				bridge_version: 1,
			},
		});
		const second = factory.createRequestFrame({
			ownership,
			payload: {
				type: "create_layer",
			},
		});

		expect(first).toMatchObject({
			frame_type: "request",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			request_id: 1,
			ownership,
		});
		expect(second.request_id).toBe(2);
		expect(
			factory.createSidecarResponseFrame({
				request: hostCallbackRequest,
				payload: {
					type: "host_callback_result",
					invocation_id: "invocation",
					result: { ok: true },
				},
			}),
		).toEqual({
			frame_type: "sidecar_response",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			request_id: 7,
			ownership,
			payload: {
				type: "host_callback_result",
				invocation_id: "invocation",
				result: { ok: true },
			},
		});
		expect(
			factory.createControlFrame({ type: "shutdown", reason: "test complete" }),
		).toEqual({
			frame_type: "control",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			payload: { type: "shutdown", reason: "test complete" },
		});
	});

	it("resolves sidecar request frame handlers", async () => {
		await expect(
			resolveSidecarRequestFramePayload(hostCallbackRequest, async () => ({
				type: "host_callback_result",
				invocation_id: "invocation",
				result: { ok: true },
			})),
		).resolves.toEqual({
			type: "host_callback_result",
			invocation_id: "invocation",
			result: { ok: true },
		});
	});

	it("returns error payloads for missing or mismatched sidecar handlers", async () => {
		await expect(
			resolveSidecarRequestFramePayload(hostCallbackRequest, null),
		).resolves.toMatchObject({
			type: "host_callback_result",
			invocation_id: "invocation",
			error: "no sidecar request handler registered for host_callback",
		});

		await expect(
			resolveSidecarRequestFramePayload(hostCallbackRequest, async () => ({
				type: "js_bridge_result",
				call_id: "call",
				result: {},
			})),
		).resolves.toMatchObject({
			type: "host_callback_result",
			invocation_id: "invocation",
			error: "sidecar handler returned js_bridge_result for host_callback",
		});
	});

	it("maps host-written request frames to generated protocol frames", () => {
		expect(
			toGeneratedProtocolFrame({
				frame_type: "request",
				schema: SIDECAR_PROTOCOL_SCHEMA,
				request_id: 7,
				ownership,
				payload: {
					type: "authenticate",
					client_name: "agentos",
					auth_token: "token",
					protocol_version: 8,
					bridge_version: 1,
				},
			}),
		).toMatchObject({
			tag: "RequestFrame",
			val: {
				requestId: 7n,
				payload: {
					tag: "AuthenticateRequest",
				},
			},
		});
	});

	it("encodes host-written frames as BARE protocol bytes", () => {
		const encoded = encodeBareProtocolFrame({
			frame_type: "sidecar_response",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			request_id: 8,
			ownership,
			payload: {
				type: "host_callback_result",
				invocation_id: "invocation",
				result: { ok: true },
			},
		});

		expect(protocol.decodeProtocolFrame(new Uint8Array(encoded)).tag).toBe(
			"SidecarResponseFrame",
		);

		const control = encodeBareProtocolFrame({
			frame_type: "control",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			payload: { type: "shutdown", reason: "test complete" },
		});
		expect(protocol.decodeProtocolFrame(new Uint8Array(control))).toMatchObject(
			{
				tag: "ControlFrame",
				val: {
					payload: {
						tag: "ShutdownControl",
						val: { reason: "test complete" },
					},
				},
			},
		);
	});

	it("matches native generated auth frame BARE bytes", () => {
		const encoded = encodeBareProtocolFrame({
			frame_type: "request",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			request_id: 7,
			ownership: generatedAuthOwnership,
			payload: {
				type: "authenticate",
				client_name: "generated-test",
				auth_token: "token",
				protocol_version: SIDECAR_PROTOCOL_SCHEMA.version,
				bridge_version: 1,
			},
		});

		expect(Buffer.from(encoded).toString("hex")).toBe(GENERATED_AUTH_FRAME_HEX);
		expect(protocol.decodeProtocolFrame(new Uint8Array(encoded))).toMatchObject(
			{
				tag: "RequestFrame",
				val: {
					requestId: 7n,
					payload: { tag: "AuthenticateRequest" },
				},
			},
		);
	});

	it("decodes sidecar-written response frames from generated protocol frames", () => {
		expect(
			fromGeneratedSidecarWrittenProtocolFrame({
				tag: "ResponseFrame",
				val: {
					schema: SIDECAR_PROTOCOL_SCHEMA,
					requestId: 9n,
					ownership: {
						tag: "ConnectionOwnership",
						val: { connectionId: "conn" },
					},
					payload: {
						tag: "VmCreatedResponse",
						val: { vmId: "vm" },
					},
				},
			}),
		).toEqual({
			frame_type: "response",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			request_id: 9,
			ownership,
			payload: { type: "vm_created", vm_id: "vm" },
		});
	});

	it("decodes sidecar-written event frames from BARE bytes", () => {
		const bytes = protocol.encodeProtocolFrame({
			tag: "EventFrame",
			val: {
				schema: SIDECAR_PROTOCOL_SCHEMA,
				ownership: {
					tag: "ConnectionOwnership",
					val: { connectionId: "conn" },
				},
				payload: {
					tag: "StructuredEvent",
					val: {
						name: "ready",
						detail: new Map([["ok", "true"]]),
					},
				},
			},
		});

		expect(decodeBareProtocolFrame(bytes)).toEqual({
			frame_type: "event",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			ownership,
			payload: {
				type: "structured",
				name: "ready",
				detail: { ok: "true" },
			},
		});
	});

	it("encodes and decodes JSON compatibility protocol frames", () => {
		const encoded = encodeProtocolFramePayload(
			{
				frame_type: "event",
				schema: SIDECAR_PROTOCOL_SCHEMA,
				ownership,
				payload: {
					type: "process_output",
					process_id: "proc",
					channel: "stdout",
					chunk: new Uint8Array([1, 2, 3]),
				},
			},
			"json",
		);

		expect(JSON.parse(textDecoder.decode(encoded)).payload.chunk).toEqual([
			1, 2, 3,
		]);

		const decoded = decodeProtocolFramePayload(encoded, "json");
		if (
			decoded.frame_type !== "event" ||
			decoded.payload.type !== "process_output"
		) {
			throw new Error("expected process_output event");
		}
		expect(decoded.payload.chunk).toBeInstanceOf(Uint8Array);
		expect(Array.from(decoded.payload.chunk)).toEqual([1, 2, 3]);
	});

	it("classifies sidecar-written protocol frames for RPC dispatch", () => {
		expect(
			classifySidecarWrittenProtocolFrame({
				frame_type: "response",
				schema: SIDECAR_PROTOCOL_SCHEMA,
				request_id: 10,
				ownership,
				payload: { type: "vm_created", vm_id: "vm" },
			}),
		).toMatchObject({
			kind: "response",
			requestId: 10,
			frame: { frame_type: "response" },
		});

		expect(
			classifySidecarWrittenProtocolFrame({
				frame_type: "event",
				schema: SIDECAR_PROTOCOL_SCHEMA,
				ownership,
				payload: {
					type: "structured",
					name: "ready",
					detail: {},
				},
			}),
		).toMatchObject({
			kind: "event",
			frame: { frame_type: "event" },
		});

		expect(
			classifySidecarWrittenProtocolFrame({
				frame_type: "sidecar_request",
				schema: SIDECAR_PROTOCOL_SCHEMA,
				request_id: 11,
				ownership,
				payload: {
					type: "host_callback",
					invocation_id: "invocation",
					callback_key: "tool",
					input: { ok: true },
					timeout_ms: 1_000,
				},
			}),
		).toMatchObject({
			kind: "sidecarRequest",
			frame: { frame_type: "sidecar_request" },
		});
	});

	it("rejects host-written generated frames on the sidecar-written decode path", () => {
		expect(() =>
			fromGeneratedSidecarWrittenProtocolFrame({
				tag: "RequestFrame",
				val: {
					schema: SIDECAR_PROTOCOL_SCHEMA,
					requestId: 1n,
					ownership: {
						tag: "ConnectionOwnership",
						val: { connectionId: "conn" },
					},
					payload: {
						tag: "CreateLayerRequest",
						val: null,
					},
				},
			}),
		).toThrow("unsupported BARE protocol frame tag: RequestFrame");
	});
});
