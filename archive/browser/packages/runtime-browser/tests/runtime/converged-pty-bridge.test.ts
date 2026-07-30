import { describe, expect, it } from "vitest";
import type { GuestKernelResult } from "../../src/converged-net-bridge.js";
import {
	CONVERGED_PTY_BRIDGE_OPERATIONS,
	convergedPtyRequestPayload,
	convergedPtySyncResponse,
	isConvergedPtyBridgeOperation,
} from "../../src/converged-pty-bridge.js";
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

describe("converged pty bridge translation", () => {
	it("exposes the converged pty operation set", () => {
		expect(isConvergedPtyBridgeOperation("pty.open")).toBe(true);
		expect(isConvergedPtyBridgeOperation("pty.tcsetattr")).toBe(true);
		expect(isConvergedPtyBridgeOperation("net.connect")).toBe(false);
		expect(CONVERGED_PTY_BRIDGE_OPERATIONS).toContain("pty.resize");
		expect(CONVERGED_PTY_BRIDGE_OPERATIONS).toContain(
			"pty.setForegroundPgid",
		);
	});

	it("maps pty.open into an empty guest_kernel_call payload", () => {
		const open = convergedPtyRequestPayload("pty.open", [{}], "exec-1");
		expect(open.type).toBe("guest_kernel_call");
		expect(open.execution_id).toBe("exec-1");
		expect(open.operation).toBe("pty.open");
		expect(decodePayload(open.payload)).toEqual({});
	});

	it("carries fd + read bounds for pty.read but forces browser reads non-blocking", () => {
		const read = convergedPtyRequestPayload(
			"pty.read",
			[{ fd: 7, maxBytes: 1024, timeoutMs: 25 }],
			"exec-1",
		);
		expect(decodePayload(read.payload)).toEqual({
			fd: 7,
			maxBytes: 1024,
			timeoutMs: 0,
		});
		// Browser wasm cannot use Rust timed waits in the synchronous pushFrame path,
		// so even an unspecified timeout becomes a non-blocking poll.
		const bare = convergedPtyRequestPayload("pty.read", [{ fd: 7 }], "exec-1");
		expect(decodePayload(bare.payload)).toEqual({ fd: 7, timeoutMs: 0 });
	});

	it("base64-encodes binary write data", () => {
		const write = convergedPtyRequestPayload(
			"pty.write",
			[{ fd: 3, data: new Uint8Array([0, 1, 2, 253]) }],
			"exec-1",
		);
		expect(decodePayload(write.payload)).toEqual({ fd: 3, data: "AAEC/Q==" });
	});

	it("maps resize cols/rows and strips unset termios fields", () => {
		const resize = convergedPtyRequestPayload(
			"pty.resize",
			[{ fd: 4, cols: 120, rows: 40 }],
			"exec-1",
		);
		expect(decodePayload(resize.payload)).toEqual({
			fd: 4,
			cols: 120,
			rows: 40,
		});

		const raw = convergedPtyRequestPayload(
			"pty.tcsetattr",
			[{ fd: 4, icanon: false, echo: false, cc: { vintr: 3 } }],
			"exec-1",
		);
		expect(decodePayload(raw.payload)).toEqual({
			fd: 4,
			icanon: false,
			echo: false,
			cc: { vintr: 3 },
		});
	});

	it("maps foreground process group updates with optional pgid", () => {
		const defaultPgid = convergedPtyRequestPayload(
			"pty.setForegroundPgid",
			[{ fd: 4 }],
			"exec-1",
		);
		expect(decodePayload(defaultPgid.payload)).toEqual({ fd: 4 });

		const explicitPgid = convergedPtyRequestPayload(
			"pty.setForegroundPgid",
			[{ fd: 4, pgid: 99 }],
			"exec-1",
		);
		expect(decodePayload(explicitPgid.payload)).toEqual({ fd: 4, pgid: 99 });
	});

	it("requires fd and binary data where mandatory", () => {
		expect(() =>
			convergedPtyRequestPayload("pty.close", [{}], "exec-1"),
		).toThrow(/numeric fd/);
		expect(() =>
			convergedPtyRequestPayload("pty.write", [{ fd: 1 }], "exec-1"),
		).toThrow(/binary data/);
		expect(() =>
			convergedPtyRequestPayload("pty.resize", [{ fd: 1, cols: 80 }], "exec-1"),
		).toThrow(/numeric rows/);
	});

	it("rejects unknown operations", () => {
		expect(() =>
			convergedPtyRequestPayload("pty.teleport", [{}], "exec-1"),
		).toThrow(/no mapping/);
	});

	it("decodes guest_kernel_result JSON passthrough", () => {
		const response = convergedPtySyncResponse(
			jsonResult({ masterFd: 3, slaveFd: 4, path: "/dev/pts/0" }),
		);
		expect(response).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { masterFd: 3, slaveFd: 4, path: "/dev/pts/0" },
		});
	});
});
