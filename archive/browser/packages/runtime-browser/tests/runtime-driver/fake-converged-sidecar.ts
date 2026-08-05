// A fake in-process wasm sidecar for white-box driver tests that need the
// converged path bootstrapped but only exercise host-capability families
// (child_process.* / process.signal_state) which route through the converged
// router's legacy fallback, never touching the kernel. Mirrors the fake used by
// converged-driver-setup.test.ts; speaks the real JSON frame codec.

import type { LiveResponsePayload } from "@rivet-dev/agentos-runtime-core/protocol-frames";
import {
	decodeProtocolFramePayload,
	encodeProtocolFramePayload,
	type LiveProtocolFrame,
} from "@rivet-dev/agentos-runtime-core/protocol-frames";
import type { LiveRequestPayload } from "@rivet-dev/agentos-runtime-core/request-payloads";
import { SIDECAR_PROTOCOL_SCHEMA } from "@rivet-dev/agentos-runtime-core/protocol-schema";
import type { CreateVmConfig } from "@rivet-dev/agentos-runtime-core/vm-config";
import type {
	ConvergedSidecarFactoryOptions,
	ConvergedSidecarHandle,
} from "../../src/runtime-driver.js";

export const FAKE_CONVERGED_CONFIG = {
	permissions: { fs: "allow" },
} as unknown as CreateVmConfig;

/** A synchronous `pushFrame` that answers the bootstrap handshake (JSON codec). */
export function fakeConvergedPushFrame(): (frame: Uint8Array) => Uint8Array {
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

/** Driver factory options wiring the fake converged sidecar. */
export function fakeConvergedFactoryOptions(): {
	convergedSidecar: ConvergedSidecarFactoryOptions;
} {
	return {
		convergedSidecar: {
			loadSidecar: async (): Promise<ConvergedSidecarHandle> => ({
				pushFrame: fakeConvergedPushFrame(),
			}),
			config: FAKE_CONVERGED_CONFIG,
			codec: "json",
		},
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
					return ok({ content: files.get(request.path) ?? "", encoding: "utf8" });
				default:
					return ok({});
			}
		}
		default:
			return { type: "rejected", code: "x", message: request.type };
	}
}
