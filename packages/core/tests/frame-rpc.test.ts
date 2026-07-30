import { PassThrough } from "node:stream";
import { describe, expect, test } from "vitest";
import { FrameRpcTransport } from "../src/frame-rpc.js";
import { encodeLengthPrefixedPayload } from "../src/framing.js";

type TestFrame =
	| { frame_type: "request"; request_id: number; payload: string }
	| { frame_type: "response"; request_id: number; payload: string }
	| { frame_type: "event"; payload: string }
	| { frame_type: "sidecar_request"; request_id: number; payload: string };

function encodeTestFrame(frame: TestFrame): Uint8Array {
	return Buffer.from(JSON.stringify(frame), "utf8");
}

function decodeTestFrame(payload: Uint8Array): TestFrame {
	return JSON.parse(Buffer.from(payload).toString("utf8")) as TestFrame;
}

function createTransport() {
	const stdin = new PassThrough();
	const stdout = new PassThrough();
	const transport = new FrameRpcTransport<
		TestFrame,
		TestFrame,
		Extract<TestFrame, { frame_type: "response" }>,
		Extract<TestFrame, { frame_type: "event" }>,
		Extract<TestFrame, { frame_type: "sidecar_request" }>
	>({
		stdin,
		stdout,
		encodeFrame: encodeTestFrame,
		decodeFrame: decodeTestFrame,
		classifyFrame: (frame) => {
			switch (frame.frame_type) {
				case "response":
					return {
						kind: "response",
						requestId: frame.request_id,
						frame,
					};
				case "event":
					return { kind: "event", frame };
				case "sidecar_request":
					return { kind: "sidecarRequest", frame };
				case "request":
					throw new Error("unexpected request frame from sidecar");
			}
		},
	});
	return { stdin, stdout, transport };
}

function writeIncomingFrame(stdout: PassThrough, frame: TestFrame): void {
	stdout.write(encodeLengthPrefixedPayload(encodeTestFrame(frame)));
}

describe("frame RPC transport", () => {
	test("correlates responses by request id", async () => {
		const { stdin, stdout, transport } = createTransport();
		const written = new Promise<TestFrame>((resolve) => {
			stdin.once("data", (chunk: Buffer) => {
				const payloadLength = chunk.readUInt32BE(0);
				resolve(decodeTestFrame(chunk.subarray(4, 4 + payloadLength)));
			});
		});

		const response = transport.sendFrame(3, {
			frame_type: "request",
			request_id: 3,
			payload: "ping",
		});
		await expect(written).resolves.toEqual({
			frame_type: "request",
			request_id: 3,
			payload: "ping",
		});

		writeIncomingFrame(stdout, {
			frame_type: "response",
			request_id: 3,
			payload: "pong",
		});

		await expect(response).resolves.toEqual({
			frame_type: "response",
			request_id: 3,
			payload: "pong",
		});
		transport.dispose();
	});

	test("dispatches event frames", async () => {
		const { stdout, transport } = createTransport();
		const event = new Promise<Extract<TestFrame, { frame_type: "event" }>>(
			(resolve) => {
				transport.onEvent(resolve);
			},
		);

		writeIncomingFrame(stdout, { frame_type: "event", payload: "ready" });

		await expect(event).resolves.toEqual({
			frame_type: "event",
			payload: "ready",
		});
		transport.dispose();
	});

	test("dispatches sidecar request frames", async () => {
		const { stdout, transport } = createTransport();
		const request = new Promise<
			Extract<TestFrame, { frame_type: "sidecar_request" }>
		>((resolve) => {
			transport.onSidecarRequest(resolve);
		});

		writeIncomingFrame(stdout, {
			frame_type: "sidecar_request",
			request_id: 9,
			payload: "callback",
		});

		await expect(request).resolves.toEqual({
			frame_type: "sidecar_request",
			request_id: 9,
			payload: "callback",
		});
		transport.dispose();
	});
});
