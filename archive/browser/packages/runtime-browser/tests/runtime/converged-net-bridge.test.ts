import { describe, expect, it } from "vitest";
import {
	CONVERGED_NET_BRIDGE_OPERATIONS,
	convergedNetRequestPayload,
	convergedNetSyncResponse,
	type GuestKernelResult,
	isConvergedNetBridgeOperation,
} from "../../src/converged-net-bridge.js";
import { SYNC_BRIDGE_KIND_JSON } from "../../src/sync-bridge.js";

function decodePayload(payload: ArrayBuffer): unknown {
	return JSON.parse(new TextDecoder().decode(new Uint8Array(payload)));
}

function jsonResult(value: unknown): GuestKernelResult {
	const bytes = new TextEncoder().encode(JSON.stringify(value));
	return {
		type: "guest_kernel_result",
		payload: bytes.buffer.slice(
			bytes.byteOffset,
			bytes.byteOffset + bytes.byteLength,
		),
	};
}

describe("converged net bridge translation", () => {
	it("exposes the converged net/dns operation set", () => {
		expect(isConvergedNetBridgeOperation("net.connect")).toBe(true);
		expect(isConvergedNetBridgeOperation("dns.lookup")).toBe(true);
		expect(isConvergedNetBridgeOperation("fs.readFile")).toBe(false);
		expect(CONVERGED_NET_BRIDGE_OPERATIONS).toContain("net.recv_from");
	});

	it("maps connect/listen into guest_kernel_call with JSON payloads", () => {
		const connect = convergedNetRequestPayload(
			"net.connect",
			[{ host: "127.0.0.1", port: 8080 }],
			"exec-1",
		);
		expect(connect.type).toBe("guest_kernel_call");
		expect(connect.execution_id).toBe("exec-1");
		expect(connect.operation).toBe("net.connect");
		expect(decodePayload(connect.payload)).toEqual({
			host: "127.0.0.1",
			port: 8080,
		});

		const listen = convergedNetRequestPayload(
			"net.listen",
			[{ host: "127.0.0.1", port: 9090, backlog: 16 }],
			"exec-1",
		);
		expect(decodePayload(listen.payload)).toEqual({
			host: "127.0.0.1",
			port: 9090,
			backlog: 16,
		});
	});

	it("base64-encodes binary write/send data", () => {
		const write = convergedNetRequestPayload(
			"net.write",
			[{ socketId: 3, data: new Uint8Array([0, 1, 2, 253]) }],
			"exec-1",
		);
		expect(decodePayload(write.payload)).toEqual({
			socketId: 3,
			data: "AAEC/Q==",
		});

		const send = convergedNetRequestPayload(
			"net.send_to",
			[
				{
					socketId: 5,
					host: "127.0.0.1",
					port: 7000,
					data: new Uint8Array([255]),
				},
			],
			"exec-1",
		);
		expect(decodePayload(send.payload)).toEqual({
			socketId: 5,
			host: "127.0.0.1",
			port: 7000,
			data: "/w==",
		});
	});

	it("defaults shutdown how and requires socketId/data", () => {
		const shutdown = convergedNetRequestPayload(
			"net.shutdown",
			[{ socketId: 1 }],
			"exec-1",
		);
		expect(decodePayload(shutdown.payload)).toEqual({
			socketId: 1,
			how: "both",
		});
		expect(() =>
			convergedNetRequestPayload("net.accept", [{}], "exec-1"),
		).toThrow(/numeric socketId/);
		expect(() =>
			convergedNetRequestPayload("net.write", [{ socketId: 1 }], "exec-1"),
		).toThrow(/binary data/);
	});

	it("rejects unknown operations", () => {
		expect(() =>
			convergedNetRequestPayload("net.teleport", [{}], "exec-1"),
		).toThrow(/no mapping/);
	});

	it("decodes guest_kernel_result JSON passthrough", () => {
		const response = convergedNetSyncResponse(
			jsonResult({ socketId: 4, localPort: 5050 }),
		);
		expect(response).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { socketId: 4, localPort: 5050 },
		});
	});
});
