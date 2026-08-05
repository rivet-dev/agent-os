import { describe, expect, it } from "vitest";
import {
	convergedDgramInlineResponse,
	convergedDgramRequestPayload,
	convergedDgramSyncResponse,
	dgramOperationUsesKernel,
	isConvergedDgramBridgeOperation,
} from "../../src/converged-dgram-bridge.js";
import { SYNC_BRIDGE_KIND_JSON } from "../../src/sync-bridge.js";

function decode(payload: ArrayBuffer): unknown {
	return JSON.parse(new TextDecoder().decode(new Uint8Array(payload)));
}

describe("converged dgram bridge", () => {
	it("classifies dgram operations and kernel vs inline", () => {
		expect(isConvergedDgramBridgeOperation("dgram.send")).toBe(true);
		expect(isConvergedDgramBridgeOperation("dgram.setBufferSize")).toBe(true);
		expect(isConvergedDgramBridgeOperation("net.connect")).toBe(false);
		expect(dgramOperationUsesKernel("dgram.send")).toBe(true);
		expect(dgramOperationUsesKernel("dgram.setBufferSize")).toBe(false);
	});

	it("maps bind/send positional args to kernel JSON", () => {
		const bind = convergedDgramRequestPayload(
			"dgram.bind",
			[3, { port: 5000, address: "127.0.0.1" }],
			"exec-1",
		);
		expect(bind.operation).toBe("dgram.bind");
		expect(decode(bind.payload)).toEqual({
			socketId: 3,
			host: "127.0.0.1",
			port: 5000,
		});

		const send = convergedDgramRequestPayload(
			"dgram.send",
			[4, new Uint8Array([1, 2, 3]), { port: 6000, address: "127.0.0.1" }],
			"exec-1",
		);
		expect(decode(send.payload)).toEqual({
			socketId: 4,
			host: "127.0.0.1",
			port: 6000,
			data: "AQID",
		});
	});

	it("maps kernel recv results to the guest message shape", () => {
		expect(
			convergedDgramSyncResponse("dgram.recv", {
				data: "AQID",
				remoteAddress: "127.0.0.1",
				remotePort: 50000,
			}),
		).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: {
				type: "message",
				data: "AQID",
				remoteAddress: "127.0.0.1",
				remotePort: 50000,
				remoteFamily: "IPv4",
			},
		});
		expect(
			convergedDgramSyncResponse("dgram.recv", { data: null }),
		).toEqual({ kind: SYNC_BRIDGE_KIND_JSON, value: null });
	});

	it("maps create/bind/close/address/send results", () => {
		expect(convergedDgramSyncResponse("dgram.create", { socketId: 7 })).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { socketId: 7 },
		});
		expect(convergedDgramSyncResponse("dgram.bind", {})).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { ok: true },
		});
		expect(convergedDgramSyncResponse("dgram.send", { bytes: 5 })).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { bytes: 5 },
		});
		expect(
			convergedDgramSyncResponse("dgram.address", { host: "127.0.0.1", port: 50001 }),
		).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { address: "127.0.0.1", port: 50001, family: "IPv4" },
		});
		expect(() =>
			convergedDgramSyncResponse("dgram.address", { host: null }),
		).toThrow(/EBADF/);
	});

	it("answers buffer-size ops inline", () => {
		expect(convergedDgramInlineResponse("dgram.getBufferSize")).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { size: 65536 },
		});
		expect(convergedDgramInlineResponse("dgram.setBufferSize")).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { ok: true },
		});
	});
});
