// Converged sync-bridge handler.
//
// Replaces the legacy `runtime-driver.ts` `handleSyncBridgeOperation` (which
// serviced guest syscalls against an in-process TypeScript kernel) with one
// that routes every guest syscall to the wasm sidecar (`archive/browser/crates/sidecar-browser`)
// over the wire protocol. The kernel becomes the single enforcement point for
// both native and browser; the legacy fail-open TS executor is retired once this
// is the live path.
//
// The handler is synchronous: it encodes a wire frame, calls the injected
// synchronous `pushFrame` (the wasm `BrowserSidecarWasm.pushFrame`), decodes the
// response frame, and returns the sync-bridge response the worker writes back to
// the SAB. It is unit-tested with a fake synchronous `pushFrame`, no wasm needed.

import type { LiveOwnershipScope } from "@rivet-dev/agentos-runtime-core/ownership";
import {
	decodeProtocolFramePayload,
	encodeProtocolFramePayload,
	HostProtocolFrameFactory,
	type LiveProtocolFrame,
	type ProtocolFramePayloadCodec,
} from "@rivet-dev/agentos-runtime-core/protocol-frames";
import type { LiveRequestPayload } from "@rivet-dev/agentos-runtime-core/request-payloads";
import type { LiveResponsePayload } from "@rivet-dev/agentos-runtime-core/response-payloads";
import {
	convergedFilesystemRequestPayload,
	convergedFilesystemSyncResponse,
	type ConvergedSyncResponse,
	type GuestFilesystemResult,
	isSingleCallFilesystemOperation,
} from "./converged-fs-bridge.js";
import {
	convergedNetRequestPayload,
	convergedNetSyncResponse,
	type GuestKernelResult,
	isConvergedNetBridgeOperation,
} from "./converged-net-bridge.js";
import {
	convergedDgramInlineResponse,
	convergedDgramRequestPayload,
	convergedDgramSyncResponse,
	dgramOperationUsesKernel,
	isConvergedDgramBridgeOperation,
} from "./converged-dgram-bridge.js";
import {
	convergedPtyRequestPayload,
	convergedPtySyncResponse,
	isConvergedPtyBridgeOperation,
} from "./converged-pty-bridge.js";
import { SYNC_BRIDGE_KIND_JSON } from "./sync-bridge.js";

/** Synchronous wasm sidecar frame dispatcher (`BrowserSidecarWasm.pushFrame`). */
export type ConvergedPushFrame = (frame: Uint8Array) => Uint8Array;

/**
 * Synchronous request transport seam between the handler and the wasm sidecar.
 * Splitting this out keeps the handler's translation logic unit-testable without
 * the directional wire codec (the host can only encode request frames, and the
 * JSON codec does not round-trip binary `data` payloads).
 */
export interface ConvergedSidecarRequestTransport {
	sendRequest(payload: LiveRequestPayload): LiveResponsePayload;
}

/**
 * Production transport: encodes a request frame, calls the synchronous wasm
 * `pushFrame`, and decodes the response frame.
 */
export class PushFrameSidecarTransport
	implements ConvergedSidecarRequestTransport
{
	private readonly frames = new HostProtocolFrameFactory();
	private readonly pushFrame: ConvergedPushFrame;
	private readonly ownership: LiveOwnershipScope;
	private readonly codec: ProtocolFramePayloadCodec;

	constructor(options: {
		pushFrame: ConvergedPushFrame;
		ownership: LiveOwnershipScope;
		codec?: ProtocolFramePayloadCodec;
	}) {
		this.pushFrame = options.pushFrame;
		this.ownership = options.ownership;
		this.codec = options.codec ?? "bare";
	}

	sendRequest(payload: LiveRequestPayload): LiveResponsePayload {
		const frame = this.frames.createRequestFrame({
			ownership: this.ownership,
			payload,
		});
		const responseBytes = this.pushFrame(
			encodeProtocolFramePayload(frame, this.codec),
		);
		const decoded = decodeProtocolFramePayload(
			responseBytes,
			this.codec,
		) as unknown as LiveProtocolFrame;
		if (decoded.frame_type !== "response") {
			throw new Error(
				`converged sync bridge expected a response frame, got ${decoded.frame_type}`,
			);
		}
		if (decoded.payload.type === "rejected") {
			const message = decoded.payload.message;
			const error = new Error(message);
			// Kernel failures carry a leading POSIX errno ("EACCES: ...") in the
			// message; surface that as the guest-visible error code (Node/POSIX
			// semantics) rather than the generic wire rejection code.
			const errno = /^(E[A-Z0-9_]+):/.exec(message);
			(error as { code?: string }).code = errno
				? errno[1]
				: decoded.payload.code;
			throw error;
		}
		return decoded.payload;
	}
}

export interface ConvergedSyncBridgeHandlerOptions {
	transport: ConvergedSidecarRequestTransport;
	executionId: string;
}

export class ConvergedSyncBridgeHandler {
	private readonly transport: ConvergedSidecarRequestTransport;
	private readonly executionId: string;

	constructor(options: ConvergedSyncBridgeHandlerOptions) {
		this.transport = options.transport;
		this.executionId = options.executionId;
	}

	/** True if this handler services `operation` against the wasm sidecar. */
	handles(operation: string): boolean {
		return (
			operation === "fs.readDir" ||
			isSingleCallFilesystemOperation(operation) ||
			isConvergedNetBridgeOperation(operation) ||
			isConvergedDgramBridgeOperation(operation) ||
			isConvergedPtyBridgeOperation(operation)
		);
	}

	handle(operation: string, args: readonly unknown[]): ConvergedSyncResponse {
		if (operation === "fs.readDir") {
			return this.readDir(String(args[0]));
		}
		if (isSingleCallFilesystemOperation(operation)) {
			const result = this.callFilesystem(
				convergedFilesystemRequestPayload(operation, args),
			);
			return convergedFilesystemSyncResponse(operation, result);
		}
		if (isConvergedNetBridgeOperation(operation)) {
			const result = this.callKernel(
				convergedNetRequestPayload(operation, args, this.executionId),
			);
			return convergedNetSyncResponse(result);
		}
		if (isConvergedDgramBridgeOperation(operation)) {
			if (!dgramOperationUsesKernel(operation)) {
				return convergedDgramInlineResponse(operation);
			}
			const result = this.callKernel(
				convergedDgramRequestPayload(operation, args, this.executionId),
			);
			return convergedDgramSyncResponse(operation, decodeKernelJson(result));
		}
		if (isConvergedPtyBridgeOperation(operation)) {
			const result = this.callKernel(
				convergedPtyRequestPayload(operation, args, this.executionId),
			);
			return convergedPtySyncResponse(result);
		}
		throw new Error(
			`converged sync bridge handler does not service ${operation}`,
		);
	}

	private readDir(path: string): ConvergedSyncResponse {
		// The wire `read_dir` carries each child's type, so one round-trip
		// recovers Dirent kinds for `readdir(withFileTypes)` (no per-entry lstat).
		const listing = this.callFilesystem({
			type: "guest_filesystem_call",
			operation: "read_dir",
			path,
		});
		const entries = (listing.entries ?? []).map((entry) => ({
			name: entry.name,
			isDirectory: entry.isDirectory,
			isSymbolicLink: entry.isSymbolicLink,
		}));
		return { kind: SYNC_BRIDGE_KIND_JSON, value: entries };
	}

	private callFilesystem(payload: LiveRequestPayload): GuestFilesystemResult {
		const response = this.transport.sendRequest(payload);
		if (response.type !== "guest_filesystem_result") {
			throw new Error(
				`expected guest_filesystem_result, got ${response.type}`,
			);
		}
		return response;
	}

	private callKernel(payload: LiveRequestPayload): GuestKernelResult {
		const response = this.transport.sendRequest(payload);
		if (response.type !== "guest_kernel_result") {
			throw new Error(`expected guest_kernel_result, got ${response.type}`);
		}
		return response;
	}
}

function decodeKernelJson(result: GuestKernelResult): unknown {
	if (result.payload.byteLength === 0) {
		return null;
	}
	return JSON.parse(new TextDecoder().decode(new Uint8Array(result.payload)));
}
