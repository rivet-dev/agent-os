import { describe, expect, test } from "vitest";
import {
	decodeJsonFramePayload,
	encodeJsonFramePayload,
} from "../src/frame-payload-codec.js";

const textDecoder = new TextDecoder();
const textEncoder = new TextEncoder();

describe("frame payload codec helpers", () => {
	test("encodes Uint8Array values as number arrays for JSON frames", () => {
		const encoded = encodeJsonFramePayload({
			payload: {
				type: "process_output",
				chunk: new Uint8Array([1, 2, 3]),
			},
		});

		expect(JSON.parse(textDecoder.decode(encoded))).toEqual({
			payload: {
				type: "process_output",
				chunk: [1, 2, 3],
			},
		});
	});

	test("restores process output chunks to Uint8Array", () => {
		const decoded = decodeJsonFramePayload<{
			payload: { type: "process_output"; chunk: Uint8Array };
		}>(
			textEncoder.encode(
				JSON.stringify({
					payload: {
						type: "process_output",
						chunk: [4, 5, 6],
					},
				}),
			),
		);

		expect(decoded.payload.chunk).toBeInstanceOf(Uint8Array);
		expect(Array.from(decoded.payload.chunk)).toEqual([4, 5, 6]);
	});
});
