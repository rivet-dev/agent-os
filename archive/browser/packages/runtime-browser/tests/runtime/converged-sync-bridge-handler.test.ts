import { describe, expect, it } from "vitest";
import type { LiveGuestFilesystemStat } from "@rivet-dev/agentos-runtime-core";
import type { LiveOwnershipScope } from "@rivet-dev/agentos-runtime-core/ownership";
import {
	decodeProtocolFramePayload,
	encodeProtocolFramePayload,
	type LiveProtocolFrame,
	type LiveResponsePayload,
} from "@rivet-dev/agentos-runtime-core/protocol-frames";
import type { LiveRequestPayload } from "@rivet-dev/agentos-runtime-core/request-payloads";
import { SIDECAR_PROTOCOL_SCHEMA } from "@rivet-dev/agentos-runtime-core/protocol-schema";
import {
	ConvergedSyncBridgeHandler,
	type ConvergedSidecarRequestTransport,
	PushFrameSidecarTransport,
} from "../../src/converged-sync-bridge-handler.js";
import {
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
} from "../../src/sync-bridge.js";

const OWNERSHIP: LiveOwnershipScope = {
	scope: "vm",
	connection_id: "conn",
	session_id: "session",
	vm_id: "vm-1",
};

function stat(isDirectory: boolean): LiveGuestFilesystemStat {
	return {
		mode: isDirectory ? 0o040755 : 0o100644,
		size: 4,
		blocks: 1,
		dev: 1,
		rdev: 0,
		is_directory: isDirectory,
		is_symbolic_link: false,
		atime_ms: 1,
		mtime_ms: 2,
		ctime_ms: 3,
		birthtime_ms: 4,
		ino: 9,
		nlink: 1,
		uid: 0,
		gid: 0,
	};
}

function serviceRequest(request: LiveRequestPayload): LiveResponsePayload {
	if (request.type === "guest_filesystem_call") {
		switch (request.operation) {
			case "read_file":
				return {
					type: "guest_filesystem_result",
					operation: "read_file",
					path: request.path,
					content: "hello",
					encoding: "utf8",
				};
			case "read_dir":
				return {
					type: "guest_filesystem_result",
					operation: "read_dir",
					path: request.path,
					entries: [
						{ name: "file.txt", isDirectory: false, isSymbolicLink: false },
						{ name: "subdir", isDirectory: true, isSymbolicLink: false },
					],
				};
			case "lstat":
				return {
					type: "guest_filesystem_result",
					operation: "lstat",
					path: request.path,
					stat: stat(request.path.endsWith("subdir")),
				};
			default:
				return {
					type: "guest_filesystem_result",
					operation: request.operation,
					path: request.path,
				};
		}
	}
	if (request.type === "guest_kernel_call") {
		const response = new TextEncoder().encode(
			JSON.stringify({ socketId: 1, localPort: 5000 }),
		);
		return {
			type: "guest_kernel_result",
			payload: response.buffer.slice(
				response.byteOffset,
				response.byteOffset + response.byteLength,
			),
		};
	}
	return { type: "rejected", code: "unexpected", message: request.type };
}

// Mock transport: services requests directly, no frame codec involved.
function mockTransport(): ConvergedSidecarRequestTransport & {
	calls: LiveRequestPayload[];
} {
	const calls: LiveRequestPayload[] = [];
	return {
		calls,
		sendRequest(payload) {
			calls.push(payload);
			const response = serviceRequest(payload);
			if (response.type === "rejected") {
				throw new Error(response.message);
			}
			return response;
		},
	};
}

function handler(transport: ConvergedSidecarRequestTransport) {
	return new ConvergedSyncBridgeHandler({ transport, executionId: "exec-1" });
}

describe("converged sync bridge handler", () => {
	it("routes fs.readFile through the transport", () => {
		const transport = mockTransport();
		const response = handler(transport).handle("fs.readFile", ["/tmp/a.txt"]);
		expect(response).toEqual({ kind: SYNC_BRIDGE_KIND_TEXT, value: "hello" });
		expect(transport.calls[0]).toMatchObject({
			type: "guest_filesystem_call",
			operation: "read_file",
			path: "/tmp/a.txt",
		});
	});

	it("returns NONE for mutations", () => {
		const transport = mockTransport();
		expect(handler(transport).handle("fs.writeFile", ["/tmp/a.txt", "x"])).toEqual({
			kind: SYNC_BRIDGE_KIND_NONE,
		});
	});

	it("maps fs.readDir typed entries in a single round-trip (no per-entry lstat)", () => {
		const transport = mockTransport();
		const response = handler(transport).handle("fs.readDir", ["/dir"]);
		expect(response).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: [
				{ name: "file.txt", isDirectory: false, isSymbolicLink: false },
				{ name: "subdir", isDirectory: true, isSymbolicLink: false },
			],
		});
		expect(transport.calls.map((call) => (call as { operation?: string }).operation)).toEqual([
			"read_dir",
		]);
	});

	it("routes net.connect through guest_kernel_call and decodes the JSON result", () => {
		const transport = mockTransport();
		const response = handler(transport).handle("net.connect", [
			{ host: "127.0.0.1", port: 8080 },
		]);
		expect(response).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { socketId: 1, localPort: 5000 },
		});
		expect(transport.calls[0]).toMatchObject({
			type: "guest_kernel_call",
			operation: "net.connect",
			execution_id: "exec-1",
		});
	});

	it("reports which operations it services", () => {
		const h = handler(mockTransport());
		expect(h.handles("fs.readFile")).toBe(true);
		expect(h.handles("fs.readDir")).toBe(true);
		expect(h.handles("net.connect")).toBe(true);
		expect(h.handles("child_process.spawn")).toBe(false);
	});
});

describe("PushFrameSidecarTransport", () => {
	// Validates the real frame encode -> pushFrame -> decode round-trip with the
	// JSON codec and fs payloads (no binary `data`); the BARE codec + net
	// ArrayBuffer payloads are exercised against the real wasm sidecar in the
	// browser harness.
	function fakeSidecar(): (frame: Uint8Array) => Uint8Array {
		return (frameBytes) => {
			const frame = decodeProtocolFramePayload(
				frameBytes,
				"json",
			) as unknown as LiveProtocolFrame;
			if (frame.frame_type !== "request") {
				throw new Error(`expected request frame, got ${frame.frame_type}`);
			}
			return encodeProtocolFramePayload(
				{
					frame_type: "response",
					schema: SIDECAR_PROTOCOL_SCHEMA,
					request_id: frame.request_id,
					ownership: frame.ownership,
					payload: serviceRequest(frame.payload),
				},
				"json",
			);
		};
	}

	it("round-trips an fs read through real frame encode/decode", () => {
		const transport = new PushFrameSidecarTransport({
			pushFrame: fakeSidecar(),
			ownership: OWNERSHIP,
			codec: "json",
		});
		const result = handler(transport).handle("fs.readFile", ["/tmp/a.txt"]);
		expect(result).toEqual({ kind: SYNC_BRIDGE_KIND_TEXT, value: "hello" });
	});

	it("throws on a rejected response frame", () => {
		const reject = (): Uint8Array =>
			encodeProtocolFramePayload(
				{
					frame_type: "response",
					schema: SIDECAR_PROTOCOL_SCHEMA,
					request_id: 1,
					ownership: OWNERSHIP,
					payload: { type: "rejected", code: "denied", message: "nope" },
				},
				"json",
			);
		const transport = new PushFrameSidecarTransport({
			pushFrame: reject,
			ownership: OWNERSHIP,
			codec: "json",
		});
		expect(() => handler(transport).handle("fs.readFile", ["/x"])).toThrow("nope");
	});
});
