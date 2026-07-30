// Converged dgram (UDP) bridge translation layer.
//
// The guest worker already issues `dgram.*` sync-bridge ops (positional args).
// This routes them to the wasm kernel UDP socket table via `guest_kernel_call`
// (`dgram.*` ops in `sidecar-core::guest_net`) instead of the legacy in-process
// TS dgram sessions, and maps the kernel responses back to the exact shapes the
// guest dgram module consumes (mirroring the legacy servicer). Buffer-size ops
// have no kernel counterpart and are handled inline (advisory).

import { encodeBase64 } from "./converged-base64.js";
import type { ConvergedSyncResponse } from "./converged-fs-bridge.js";
import type { GuestKernelCallRequestPayload } from "./converged-net-bridge.js";
import { SYNC_BRIDGE_KIND_JSON } from "./sync-bridge.js";

const KERNEL_DGRAM_OPERATIONS = new Set([
	"dgram.create",
	"dgram.bind",
	"dgram.recv",
	"dgram.send",
	"dgram.close",
	"dgram.address",
]);
const BUFFER_SIZE_OPERATIONS = new Set([
	"dgram.setBufferSize",
	"dgram.getBufferSize",
]);
const DEFAULT_BUFFER_SIZE = 65536;

export function isConvergedDgramBridgeOperation(operation: string): boolean {
	return (
		KERNEL_DGRAM_OPERATIONS.has(operation) ||
		BUFFER_SIZE_OPERATIONS.has(operation)
	);
}

/** True if `operation` needs a wasm kernel round-trip (vs an inline response). */
export function dgramOperationUsesKernel(operation: string): boolean {
	return KERNEL_DGRAM_OPERATIONS.has(operation);
}

/** Inline response for buffer-size ops (no kernel counterpart; advisory). */
export function convergedDgramInlineResponse(
	operation: string,
): ConvergedSyncResponse {
	if (operation === "dgram.getBufferSize") {
		return { kind: SYNC_BRIDGE_KIND_JSON, value: { size: DEFAULT_BUFFER_SIZE } };
	}
	return { kind: SYNC_BRIDGE_KIND_JSON, value: { ok: true } };
}

/** Translate a guest `dgram.*` sync op + positional args into a kernel call. */
export function convergedDgramRequestPayload(
	operation: string,
	args: readonly unknown[],
	executionId: string,
): GuestKernelCallRequestPayload {
	const request = buildKernelRequest(operation, args);
	return {
		type: "guest_kernel_call",
		execution_id: executionId,
		operation,
		payload: encodeJsonBytes(request),
	};
}

/** Map a kernel `dgram.*` JSON result back into the guest's expected shape. */
export function convergedDgramSyncResponse(
	operation: string,
	result: unknown,
): ConvergedSyncResponse {
	const value = (result ?? {}) as Record<string, unknown>;
	switch (operation) {
		case "dgram.create":
			return { kind: SYNC_BRIDGE_KIND_JSON, value: { socketId: value.socketId } };
		case "dgram.bind":
			return { kind: SYNC_BRIDGE_KIND_JSON, value: { ok: true } };
		case "dgram.close":
			return { kind: SYNC_BRIDGE_KIND_JSON, value: { ok: true } };
		case "dgram.send":
			return { kind: SYNC_BRIDGE_KIND_JSON, value: { bytes: value.bytes ?? 0 } };
		case "dgram.recv": {
			if (value.data === null || value.data === undefined) {
				return { kind: SYNC_BRIDGE_KIND_JSON, value: null };
			}
			const remoteAddress = String(value.remoteAddress ?? "");
			return {
				kind: SYNC_BRIDGE_KIND_JSON,
				value: {
					type: "message",
					data: value.data,
					remoteAddress,
					remotePort: value.remotePort,
					remoteFamily: remoteAddress.includes(":") ? "IPv6" : "IPv4",
				},
			};
		}
		case "dgram.address": {
			if (value.host === null || value.host === undefined) {
				const error = new Error("getsockname EBADF");
				(error as { code?: string }).code = "EBADF";
				throw error;
			}
			const address = String(value.host);
			return {
				kind: SYNC_BRIDGE_KIND_JSON,
				value: {
					address,
					port: value.port,
					family: address.includes(":") ? "IPv6" : "IPv4",
				},
			};
		}
		default:
			throw new Error(`converged dgram bridge has no response for ${operation}`);
	}
}

function buildKernelRequest(
	operation: string,
	args: readonly unknown[],
): Record<string, unknown> {
	switch (operation) {
		case "dgram.create":
			return {};
		case "dgram.bind": {
			const options = (args[1] ?? {}) as { port?: unknown; address?: unknown };
			return {
				socketId: requireSocketId(args[0]),
				host: typeof options.address === "string" ? options.address : "127.0.0.1",
				port: typeof options.port === "number" ? options.port : 0,
			};
		}
		case "dgram.recv":
			return { socketId: requireSocketId(args[0]) };
		case "dgram.send": {
			const target = (args[2] ?? {}) as { port?: unknown; address?: unknown };
			return {
				socketId: requireSocketId(args[0]),
				host: typeof target.address === "string" ? target.address : "127.0.0.1",
				port: typeof target.port === "number" ? target.port : 0,
				data: encodeBase64(toUint8Array(args[1])),
			};
		}
		case "dgram.close":
		case "dgram.address":
			return { socketId: requireSocketId(args[0]) };
		default:
			throw new Error(`converged dgram bridge has no mapping for ${operation}`);
	}
}

function requireSocketId(value: unknown): number {
	const socketId = typeof value === "string" ? Number(value) : value;
	if (typeof socketId !== "number" || Number.isNaN(socketId)) {
		throw new Error("converged dgram bridge call requires a numeric socketId");
	}
	return socketId;
}

function toUint8Array(value: unknown): Uint8Array {
	if (value instanceof Uint8Array) return value;
	if (value instanceof ArrayBuffer) return new Uint8Array(value);
	if (ArrayBuffer.isView(value)) {
		return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
	}
	throw new Error("converged dgram send requires binary data");
}

function encodeJsonBytes(value: unknown): ArrayBuffer {
	const bytes = new TextEncoder().encode(JSON.stringify(value));
	return bytes.buffer.slice(
		bytes.byteOffset,
		bytes.byteOffset + bytes.byteLength,
	);
}
