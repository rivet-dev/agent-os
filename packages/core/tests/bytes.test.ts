import { describe, expect, test } from "vitest";
import { toExactArrayBuffer, toExactUint8Array } from "../src/bytes.js";

describe("byte normalization helpers", () => {
	test("copies sliced Uint8Array views into exact ArrayBuffers", () => {
		const source = new Uint8Array([9, 1, 2, 3, 9]);
		const view = source.subarray(1, 4);
		const exact = toExactArrayBuffer(view);

		expect(exact.byteLength).toBe(3);
		expect(Array.from(new Uint8Array(exact))).toEqual([1, 2, 3]);
	});

	test("copies sliced Uint8Array views into exact Uint8Arrays", () => {
		const source = new Uint8Array([9, 4, 5, 6, 9]);
		const view = source.subarray(1, 4);
		const exact = toExactUint8Array(view);

		expect(exact.byteOffset).toBe(0);
		expect(Array.from(exact)).toEqual([4, 5, 6]);
	});
});
