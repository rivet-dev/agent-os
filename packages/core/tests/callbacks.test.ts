import { describe, expect, test } from "vitest";
import {
	errorSidecarResponsePayload,
	fromGeneratedSidecarRequestPayload,
	isMatchingSidecarResponsePayload,
	toGeneratedSidecarResponsePayload,
	type CallbackSidecarRequestPayload,
} from "../src/callbacks.js";

describe("sidecar callback helpers", () => {
	test("matches callback request and response payload families", () => {
		const request: CallbackSidecarRequestPayload = {
			type: "host_callback",
			invocation_id: "invocation-1",
			callback_key: "tool",
			input: null,
			timeout_ms: 1000,
		};

		expect(
			isMatchingSidecarResponsePayload(request, {
				type: "host_callback_result",
				invocation_id: "invocation-1",
			}),
		).toBe(true);
		expect(
			isMatchingSidecarResponsePayload(request, {
				type: "js_bridge_result",
				call_id: "call-1",
			}),
		).toBe(false);
	});

	test("formats callback errors as matching responses", () => {
		expect(
			errorSidecarResponsePayload(
				{
					type: "js_bridge_call",
					call_id: "call-1",
					mount_id: "mount-1",
					operation: "read",
					args: {},
				},
				new Error("boom"),
			),
		).toEqual({
			type: "js_bridge_result",
			call_id: "call-1",
			error: "boom",
		});
	});

	test("formats ext errors as namespace-scoped payloads", () => {
		const envelope = {
			namespace: "dev.test",
			payload: new Uint8Array([1, 2, 3]),
		};

		expect(
			errorSidecarResponsePayload(
				{
					type: "ext",
					envelope,
				},
				new Error("ignored"),
			),
		).toEqual({
			type: "ext_result",
			envelope: {
				namespace: "dev.test",
				payload: Buffer.from("ignored", "utf8"),
			},
		});
	});

	test("maps generated sidecar callback requests to live payloads", () => {
		expect(
			fromGeneratedSidecarRequestPayload({
				tag: "HostCallbackRequest",
				val: {
					invocationId: "invocation-1",
					callbackKey: "tool",
					input: '{"path":"/tmp/file"}',
					timeoutMs: 2500n,
				},
			}),
		).toEqual({
			type: "host_callback",
			invocation_id: "invocation-1",
			callback_key: "tool",
			input: { path: "/tmp/file" },
			timeout_ms: 2500,
		});

		expect(
			fromGeneratedSidecarRequestPayload({
				tag: "JsBridgeCallRequest",
				val: {
					callId: "call-1",
					mountId: "mount-1",
					operation: "read",
					args: '["/tmp/file"]',
				},
			}),
		).toEqual({
			type: "js_bridge_call",
			call_id: "call-1",
			mount_id: "mount-1",
			operation: "read",
			args: ["/tmp/file"],
		});
	});

	test("maps live callback responses to generated payloads", () => {
		expect(
			toGeneratedSidecarResponsePayload({
				type: "host_callback_result",
				invocation_id: "invocation-1",
				result: { ok: true },
			}),
		).toEqual({
			tag: "HostCallbackResultResponse",
			val: {
				invocationId: "invocation-1",
				result: '{"ok":true}',
				error: null,
			},
		});

		expect(
			toGeneratedSidecarResponsePayload({
				type: "js_bridge_result",
				call_id: "call-1",
				error: "boom",
			}),
		).toEqual({
			tag: "JsBridgeResultResponse",
			val: {
				callId: "call-1",
				result: null,
				error: "boom",
			},
		});
	});
});
