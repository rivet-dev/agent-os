import type { LiveSidecarEventSelector } from "./event-buffer.js";
import type { LiveOwnershipScope } from "./ownership.js";
import type {
	LiveEventFrame,
	LiveResponseFrame,
	LiveSidecarRequestHandler,
} from "./protocol-frames.js";
import type { LiveRequestPayload } from "./request-payloads.js";

export interface SidecarProcessTransport {
	setSidecarRequestHandler(handler: LiveSidecarRequestHandler | null): void;
	onEvent(handler: (event: LiveEventFrame) => void): () => void;
	sendRequest(input: {
		ownership: LiveOwnershipScope;
		payload: LiveRequestPayload;
	}): Promise<LiveResponseFrame>;
	waitForEvent(
		matcher:
			| LiveSidecarEventSelector
			| ((event: LiveEventFrame) => boolean),
		timeoutMs?: number,
		options?: {
			signal?: AbortSignal;
		},
	): Promise<LiveEventFrame>;
	failPermanently(error: Error): void;
	dispose(): Promise<void>;
}
