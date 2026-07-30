import { describe, expect, test } from "vitest";
import {
	fromGeneratedExtEnvelope,
	toGeneratedExtEnvelope,
} from "../src/ext.js";

describe("ext envelope helpers", () => {
	test("converts live envelopes to generated envelopes with exact payload buffers", () => {
		const source = new Uint8Array([9, 1, 2, 3, 9]);
		const generated = toGeneratedExtEnvelope({
			namespace: "dev.test",
			payload: source.subarray(1, 4),
		});

		expect(generated.namespace).toBe("dev.test");
		expect(generated.payload.byteLength).toBe(3);
		expect(Array.from(new Uint8Array(generated.payload))).toEqual([1, 2, 3]);
	});

	test("converts generated envelopes back to live envelopes", () => {
		const live = fromGeneratedExtEnvelope({
			namespace: "dev.test",
			payload: new Uint8Array([4, 5, 6]).buffer,
		});

		expect(live.namespace).toBe("dev.test");
		expect(Array.from(live.payload)).toEqual([4, 5, 6]);
	});
});
