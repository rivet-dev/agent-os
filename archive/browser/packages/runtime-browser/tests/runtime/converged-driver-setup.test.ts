import { describe, expect, it, vi } from "vitest";
import type { LiveResponsePayload } from "@rivet-dev/agentos-runtime-core/protocol-frames";
import {
	decodeProtocolFramePayload,
	encodeProtocolFramePayload,
	type LiveProtocolFrame,
} from "@rivet-dev/agentos-runtime-core/protocol-frames";
import type { LiveRequestPayload } from "@rivet-dev/agentos-runtime-core/request-payloads";
import { SIDECAR_PROTOCOL_SCHEMA } from "@rivet-dev/agentos-runtime-core/protocol-schema";
import type { CreateVmConfig } from "@rivet-dev/agentos-runtime-core/vm-config";
import { createConvergedServicer } from "../../src/converged-driver-setup.js";
import {
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
} from "../../src/sync-bridge.js";

// A fake wasm sidecar with an in-memory filesystem, exercised over the real
// JSON frame codec (no binary payloads on this path).
function fakeSidecar(): (frame: Uint8Array) => Uint8Array {
	const files = new Map<string, string>();
	const dirs = new Set<string>(["/"]);
	return (frameBytes) => {
		const frame = decodeProtocolFramePayload(
			frameBytes,
			"json",
		) as unknown as LiveProtocolFrame;
		if (frame.frame_type !== "request") {
			throw new Error(`expected request, got ${frame.frame_type}`);
		}
		return encodeProtocolFramePayload(
			{
				frame_type: "response",
				schema: SIDECAR_PROTOCOL_SCHEMA,
				request_id: frame.request_id,
				ownership: frame.ownership,
				payload: service(frame.payload, files, dirs),
			},
			"json",
		);
	};
}

function service(
	request: LiveRequestPayload,
	files: Map<string, string>,
	dirs: Set<string>,
): LiveResponsePayload {
	switch (request.type) {
		case "authenticate":
			return {
				type: "authenticated",
				sidecar_id: "s",
				connection_id: "c",
				max_frame_bytes: 1024,
			};
		case "open_session":
			return {
				type: "session_opened",
				session_id: "se",
				owner_connection_id: "c",
			};
		case "create_vm":
			return { type: "vm_created", vm_id: "vm" };
		case "execute":
			return { type: "process_started", process_id: request.process_id };
		case "guest_kernel_call": {
			const payload = new TextEncoder().encode(JSON.stringify({ socketId: 1 }));
			return {
				type: "guest_kernel_result",
				payload: payload.buffer.slice(
					payload.byteOffset,
					payload.byteOffset + payload.byteLength,
				),
			};
		}
		case "guest_filesystem_call": {
			const ok = (extra: Record<string, unknown>) => ({
				type: "guest_filesystem_result" as const,
				operation: request.operation,
				path: request.path,
				...extra,
			});
			switch (request.operation) {
				case "mkdir":
				case "create_dir":
					dirs.add(request.path);
					return ok({});
				case "write_file":
					files.set(request.path, request.content ?? "");
					return ok({});
				case "read_file":
					return ok({
						content: files.get(request.path) ?? "",
						encoding: "utf8",
					});
				default:
					return ok({});
			}
		}
		default:
			return { type: "rejected", code: "x", message: request.type };
	}
}

const CONFIG = {
	permissions: { fs: "allow" },
} as unknown as CreateVmConfig;

describe("converged driver setup", () => {
	it("routes fs ops to the wasm kernel and falls back to legacy otherwise", async () => {
		const legacy = vi.fn(async () => ({
			kind: SYNC_BRIDGE_KIND_JSON as typeof SYNC_BRIDGE_KIND_JSON,
			value: { legacy: true },
		}));
		const servicer = createConvergedServicer({
			pushFrame: fakeSidecar(),
			config: CONFIG,
			codec: "json",
		});

		expect(
			await servicer.route("exec-1", "fs.writeFile", ["/a.txt", "hi"], legacy),
		).toEqual({ kind: SYNC_BRIDGE_KIND_NONE });
		expect(
			await servicer.route("exec-1", "fs.readFile", ["/a.txt"], legacy),
		).toEqual({ kind: SYNC_BRIDGE_KIND_TEXT, value: "hi" });
		expect(legacy).not.toHaveBeenCalled();

		// A not-yet-converged family defers to legacy.
		const result = await servicer.route(
			"exec-1",
			"child_process.spawn",
			[{}],
			legacy,
		);
		expect(result).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { legacy: true },
		});
		expect(legacy).toHaveBeenCalledOnce();
	});

	it("lazily registers a kernel execution for net/dgram/pty, not for fs", async () => {
		const operations: string[] = [];
		const recordingFrame = (frame: Uint8Array): Uint8Array => {
			const decoded = decodeProtocolFramePayload(
				frame,
				"json",
			) as unknown as LiveProtocolFrame;
			if (decoded.frame_type === "request") {
				operations.push(decoded.payload.type);
			}
			return fakeSidecar()(frame);
		};
		const setNextExecutionId = vi.fn();
		const legacy = vi.fn(async () => ({
			kind: SYNC_BRIDGE_KIND_NONE as typeof SYNC_BRIDGE_KIND_NONE,
		}));
		const servicer = createConvergedServicer({
			pushFrame: recordingFrame,
			config: CONFIG,
			codec: "json",
			setNextExecutionId,
		});
		operations.length = 0; // ignore the bootstrap handshake

		// fs op: no execution registration.
		await servicer.route("exec-1", "fs.readFile", ["/a.txt"], legacy);
		expect(setNextExecutionId).not.toHaveBeenCalled();
		expect(operations).not.toContain("execute");

		// Registration happens before the handler runs; the net op response itself
		// can't round-trip its ArrayBuffer payload over the JSON test codec (real
		// wasm uses BARE), so ignore that and assert the registration side effects.
		const route = (op: string) =>
			servicer
				.route("exec-1", op, [{ host: "127.0.0.1", port: 80 }], legacy)
				.catch(() => undefined);

		// first net op: registers the execution once.
		await route("net.connect");
		expect(setNextExecutionId).toHaveBeenCalledWith("exec-1");
		expect(operations.filter((op) => op === "execute")).toHaveLength(1);

		// second net op for the same execution: no re-registration.
		await route("net.connect");
		expect(operations.filter((op) => op === "execute")).toHaveLength(1);

		// pty op for a new execution: also registers before the kernel call.
		await servicer
			.route("exec-pty", "pty.read", [{ fd: 4 }], legacy)
			.catch(() => undefined);
		expect(setNextExecutionId).toHaveBeenLastCalledWith("exec-pty");
		expect(operations.filter((op) => op === "execute")).toHaveLength(2);
	});
});
