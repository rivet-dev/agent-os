// Converged PTY bridge translation layer.
//
// Pseudo-terminal syscalls route to the wasm sidecar's generic guest-kernel-call
// wire payload (`guest_kernel_call` -> `agentos_vm::core::guest_pty`),
// which drives the kernel's `PtyManager` (line discipline, termios, window size).
// This mirrors `converged-net-bridge.ts`: the wire `operation` string is the
// sync-bridge op string, and `payload` is the JSON request body the Rust
// dispatcher decodes. The PTY is fd/stream based, so `pty.read`/`pty.write`
// carry a kernel fd and binary data is base64-encoded into the opaque JSON
// payload (read payloads come back base64; the guest decodes). Unit-tested in
// Node without a wasm sidecar.

import type { LiveResponsePayload } from "@rivet-dev/agentos-runtime-core";
import { encodeBase64 } from "./converged-base64.js";
import type { GuestKernelCallRequestPayload } from "./converged-net-bridge.js";
import { SYNC_BRIDGE_KIND_JSON } from "./sync-bridge.js";

type GuestKernelResult = Extract<
	LiveResponsePayload,
	{ type: "guest_kernel_result" }
>;

/** Guest `pty.*` sync-bridge operations serviced by the wasm sidecar. */
export const CONVERGED_PTY_BRIDGE_OPERATIONS = [
	"pty.open",
	"pty.read",
	"pty.write",
	"pty.close",
	"pty.resize",
	"pty.setForegroundPgid",
	"pty.tcgetattr",
	"pty.tcsetattr",
] as const;

const CONVERGED_PTY_BRIDGE_OPERATION_SET = new Set<string>(
	CONVERGED_PTY_BRIDGE_OPERATIONS,
);

export function isConvergedPtyBridgeOperation(operation: string): boolean {
	return CONVERGED_PTY_BRIDGE_OPERATION_SET.has(operation);
}

/**
 * Translate a guest `pty.*` sync-bridge call into a `guest_kernel_call` request
 * payload. `args[0]` is the structured request object; the `pty.write` `data`
 * field is base64-encoded into the JSON the kernel dispatcher expects.
 */
export function convergedPtyRequestPayload(
	operation: string,
	args: readonly unknown[],
	executionId: string,
): GuestKernelCallRequestPayload {
	if (!isConvergedPtyBridgeOperation(operation)) {
		throw new Error(`converged pty bridge has no mapping for ${operation}`);
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
 * Decode a `guest_kernel_result` into the sync-bridge response the guest pty
 * module consumes. The kernel dispatcher returns JSON; binary read payloads
 * stay base64 strings (the guest decodes them), so this is a JSON passthrough.
 */
export function convergedPtySyncResponse(result: GuestKernelResult): {
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
		case "pty.open":
			return {};
		case "pty.read":
			return stripUndefined({
				fd: requireFd(options),
				maxBytes: options.maxBytes,
				// Browser-side pty reads run inside the wasm sidecar's synchronous
				// pushFrame path. A non-zero Rust timeout reaches Condvar::wait_timeout,
				// which is not usable in the browser/no-threads build and aborts the
				// wasm module. Keep reads non-blocking here; callers poll/pump from JS.
				timeoutMs: 0,
			});
		case "pty.write":
			return {
				fd: requireFd(options),
				data: encodeBase64(requireData(options)),
			};
		case "pty.close":
		case "pty.tcgetattr":
			return { fd: requireFd(options) };
		case "pty.setForegroundPgid":
			return stripUndefined({
				fd: requireFd(options),
				pgid: options.pgid,
			});
		case "pty.resize":
			return {
				fd: requireFd(options),
				cols: requireNumber(options, "cols"),
				rows: requireNumber(options, "rows"),
			};
		case "pty.tcsetattr":
			return buildTermiosRequest(options);
		default:
			throw new Error(`converged pty bridge has no mapping for ${operation}`);
	}
}

/**
 * `pty.tcsetattr` passes only the termios fields the guest wants changed
 * (raw-mode toggles + optional control chars), so unspecified fields are
 * stripped and the kernel leaves them untouched.
 */
function buildTermiosRequest(
	options: Record<string, unknown>,
): Record<string, unknown> {
	const request = stripUndefined({
		fd: requireFd(options),
		icrnl: options.icrnl,
		opost: options.opost,
		onlcr: options.onlcr,
		icanon: options.icanon,
		echo: options.echo,
		isig: options.isig,
	});
	if (options.cc && typeof options.cc === "object") {
		const cc = options.cc as Record<string, unknown>;
		request.cc = stripUndefined({
			vintr: cc.vintr,
			vquit: cc.vquit,
			vsusp: cc.vsusp,
			veof: cc.veof,
			verase: cc.verase,
		});
	}
	return request;
}

function requireFd(options: Record<string, unknown>): number {
	const fd = options.fd;
	if (typeof fd !== "number") {
		throw new Error("converged pty bridge call requires numeric fd");
	}
	return fd;
}

function requireNumber(options: Record<string, unknown>, key: string): number {
	const value = options[key];
	if (typeof value !== "number") {
		throw new Error(`converged pty bridge call requires numeric ${key}`);
	}
	return value;
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
	throw new Error("converged pty bridge write requires binary data");
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
