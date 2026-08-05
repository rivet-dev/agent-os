// Converged networking bridge translation layer.
//
// Unlike `fs.*`, legacy guest networking never went through the SAB sync-bridge
// (it used an async network adapter), so there is no legacy op to mirror. The
// converged executor introduces synchronous guest `net.*` / `dns.*` sync-bridge
// operations that route to the wasm sidecar's generic guest-kernel-call wire
// payload (`guest_kernel_call` -> `agentos_vm::core::guest_net`), which
// drives the kernel socket table (the single network-policy enforcement point,
// S1) over loopback.
//
// This module is the pure translation between a guest net/dns sync-bridge call
// and the `guest_kernel_call` request / `guest_kernel_result` response. The
// wire `operation` string is identical to the sync-bridge op string, and the
// `payload` is the JSON request body the Rust dispatcher decodes. Binary socket
// payloads are base64-encoded (the wire `payload` is opaque JSON). Unit-tested
// in Node without a wasm sidecar.

import type { LiveRequestPayload, LiveResponsePayload } from "@rivet-dev/agentos-runtime-core";
import { encodeBase64 } from "./converged-base64.js";
import { SYNC_BRIDGE_KIND_JSON } from "./sync-bridge.js";

export type GuestKernelCallRequestPayload = Extract<
	LiveRequestPayload,
	{ type: "guest_kernel_call" }
>;

export type GuestKernelResult = Extract<
	LiveResponsePayload,
	{ type: "guest_kernel_result" }
>;

/** Guest network/DNS sync-bridge operations serviced by the wasm sidecar. */
export const CONVERGED_NET_BRIDGE_OPERATIONS = [
	"net.connect",
	"net.listen",
	"net.accept",
	"net.read",
	"net.write",
	"net.poll",
	"net.shutdown",
	"net.close",
	"net.udp_bind",
	"net.send_to",
	"net.recv_from",
	"dns.lookup",
] as const;

const CONVERGED_NET_BRIDGE_OPERATION_SET = new Set<string>(
	CONVERGED_NET_BRIDGE_OPERATIONS,
);

export function isConvergedNetBridgeOperation(operation: string): boolean {
	return CONVERGED_NET_BRIDGE_OPERATION_SET.has(operation);
}

/**
 * Translate a guest `net.*` / `dns.*` sync-bridge call into a
 * `guest_kernel_call` request payload. `args[0]` is the structured request
 * object (the guest net module passes a typed options object); binary `data`
 * fields are base64-encoded into the JSON the kernel dispatcher expects.
 */
export function convergedNetRequestPayload(
	operation: string,
	args: readonly unknown[],
	executionId: string,
): GuestKernelCallRequestPayload {
	if (!isConvergedNetBridgeOperation(operation)) {
		throw new Error(
			`converged net bridge has no mapping for ${operation}`,
		);
	}
	const options = (args[0] ?? {}) as Record<string, unknown>;
	const request = buildKernelRequest(operation, options);
	return {
		type: "guest_kernel_call",
		execution_id: executionId,
		operation,
		payload: encodeJsonBytes(request),
	};
}

/**
 * Decode a `guest_kernel_result` into the sync-bridge response the guest net
 * module consumes. The kernel dispatcher already returns JSON; binary read
 * payloads stay base64 strings (the guest module decodes them), so this is a
 * JSON passthrough.
 */
export function convergedNetSyncResponse(result: GuestKernelResult): {
	kind: typeof SYNC_BRIDGE_KIND_JSON;
	value: unknown;
} {
	return {
		kind: SYNC_BRIDGE_KIND_JSON,
		value: decodeJsonBytes(result.payload),
	};
}

function buildKernelRequest(
	operation: string,
	options: Record<string, unknown>,
): Record<string, unknown> {
	switch (operation) {
		case "net.connect":
		case "net.listen":
		case "net.udp_bind":
			return stripUndefined({
				host: options.host,
				port: options.port,
				backlog: options.backlog,
			});
		case "net.accept":
		case "net.close":
			return { socketId: requireSocketId(options) };
		case "net.read":
		case "net.recv_from":
			return stripUndefined({
				socketId: requireSocketId(options),
				maxBytes: options.maxBytes,
			});
		case "net.write":
			return {
				socketId: requireSocketId(options),
				data: encodeBase64(requireData(options)),
			};
		case "net.send_to":
			return stripUndefined({
				socketId: requireSocketId(options),
				host: options.host,
				port: options.port,
				data: encodeBase64(requireData(options)),
			});
		case "net.poll":
			return stripUndefined({
				socketId: requireSocketId(options),
				events: options.events,
				timeoutMs: options.timeoutMs,
			});
		case "net.shutdown":
			return {
				socketId: requireSocketId(options),
				how: options.how ?? "both",
			};
		case "dns.lookup":
			return { hostname: String(options.hostname ?? "") };
		default:
			throw new Error(`converged net bridge has no mapping for ${operation}`);
	}
}

function requireSocketId(options: Record<string, unknown>): number {
	const socketId = options.socketId;
	if (typeof socketId !== "number") {
		throw new Error("converged net bridge call requires numeric socketId");
	}
	return socketId;
}

function requireData(options: Record<string, unknown>): Uint8Array {
	const data = options.data;
	if (data instanceof Uint8Array) {
		return data;
	}
	if (data instanceof ArrayBuffer) {
		return new Uint8Array(data);
	}
	if (ArrayBuffer.isView(data)) {
		return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
	}
	throw new Error("converged net bridge write/send requires binary data");
}

function stripUndefined(
	record: Record<string, unknown>,
): Record<string, unknown> {
	const result: Record<string, unknown> = {};
	for (const [key, value] of Object.entries(record)) {
		if (value !== undefined) {
			result[key] = value;
		}
	}
	return result;
}

function encodeJsonBytes(value: unknown): ArrayBuffer {
	const bytes = new TextEncoder().encode(JSON.stringify(value));
	return bytes.buffer.slice(
		bytes.byteOffset,
		bytes.byteOffset + bytes.byteLength,
	);
}

function decodeJsonBytes(payload: ArrayBuffer): unknown {
	if (payload.byteLength === 0) {
		return null;
	}
	return JSON.parse(new TextDecoder().decode(new Uint8Array(payload)));
}
