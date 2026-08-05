import { describe, expect, test } from "vitest";
import type { SidecarProcessTransport } from "../src/sidecar-client.js";
import { SidecarProcess } from "../src/sidecar-process.js";
import type {
	LiveEventFrame,
	LiveResponseFrame,
	LiveSidecarRequestHandler,
} from "../src/protocol-frames.js";
import { SIDECAR_PROTOCOL_SCHEMA } from "../src/protocol-schema.js";
import type { LiveOwnershipScope } from "../src/ownership.js";
import type { LiveRequestPayload } from "../src/request-payloads.js";

class MemorySidecarTransport implements SidecarProcessTransport {
	readonly requests: Array<{
		ownership: LiveOwnershipScope;
		payload: LiveRequestPayload;
	}> = [];
	disposed = false;
	failed: Error | null = null;
	private sidecarRequestHandler: LiveSidecarRequestHandler | null = null;
	private readonly eventListeners = new Set<(event: LiveEventFrame) => void>();

	setSidecarRequestHandler(handler: LiveSidecarRequestHandler | null): void {
		this.sidecarRequestHandler = handler;
	}

	onEvent(handler: (event: LiveEventFrame) => void): () => void {
		this.eventListeners.add(handler);
		return () => {
			this.eventListeners.delete(handler);
		};
	}

	async sendRequest(input: {
		ownership: LiveOwnershipScope;
		payload: LiveRequestPayload;
	}): Promise<LiveResponseFrame> {
		this.requests.push(input);
		if (input.payload.type !== "create_layer") {
			throw new Error(`unexpected request ${input.payload.type}`);
		}
		return {
			frame_type: "response",
			schema: SIDECAR_PROTOCOL_SCHEMA,
			request_id: this.requests.length,
			ownership: input.ownership,
			payload: { type: "layer_created", layer_id: "layer-from-memory" },
		};
	}

	async waitForEvent(): Promise<LiveEventFrame> {
		throw new Error("waitForEvent not implemented for this test");
	}

	failPermanently(error: Error): void {
		this.failed = error;
	}

	async dispose(): Promise<void> {
		this.disposed = true;
	}
}

describe("sidecar process transport injection", () => {
	test("runs high-level process operations over an injected transport", async () => {
		const transport = new MemorySidecarTransport();
		const process = SidecarProcess.fromClient(transport);

		const layerId = await process.createLayer(
			{ connectionId: "conn", sessionId: "session" },
			{ vmId: "vm" },
		);
		await process.dispose();

		expect(layerId).toBe("layer-from-memory");
		expect(transport.requests).toMatchObject([
			{
				ownership: {
					scope: "vm",
					connection_id: "conn",
					session_id: "session",
					vm_id: "vm",
				},
				payload: { type: "create_layer" },
			},
		]);
		expect(transport.disposed).toBe(true);
	});
});
