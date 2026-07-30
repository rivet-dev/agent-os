import { describe, expect, it } from "vitest";
import {
	base64ToBytes,
	bytesToBase64,
	guestEncodingBootstrapCode,
	toUint8Array,
} from "../../src/encoding.js";

describe("browser encoding helpers", () => {
	it("encodes bytes to base64 without relying on large argument spreads", () => {
		const bytes = new Uint8Array(70_000);
		for (let index = 0; index < bytes.length; index++) {
			bytes[index] = index % 251;
		}

		expect(bytesToBase64(bytes)).toBe(Buffer.from(bytes).toString("base64"));
	});

	it("decodes base64 into bytes without relying on local decoder copies", () => {
		const bytes = new Uint8Array(70_000);
		for (let index = 0; index < bytes.length; index++) {
			bytes[index] = (index * 17) % 251;
		}

		expect([...base64ToBytes(Buffer.from(bytes).toString("base64"))]).toEqual([
			...bytes,
		]);
	});

	it("normalizes array buffers and strings to Uint8Array", () => {
		const bytes = new Uint8Array([65, 66, 67]);

		expect(toUint8Array(bytes)).toBe(bytes);
		expect([...toUint8Array(bytes.buffer)]).toEqual([65, 66, 67]);
		expect([...toUint8Array("ABC")]).toEqual([65, 66, 67]);
	});

	it("installs the guest encoding global from the shared bootstrap", () => {
		const previous = (
			globalThis as typeof globalThis & { __agentOSEncoding?: unknown }
		).__agentOSEncoding;
		delete (globalThis as typeof globalThis & { __agentOSEncoding?: unknown })
			.__agentOSEncoding;

		try {
			new Function(guestEncodingBootstrapCode())();
			const installed = (
				globalThis as typeof globalThis & {
					__agentOSEncoding: {
						base64ToBytes(value: string): Uint8Array;
						bytesToBase64(value: Uint8Array): string;
						decodeBytesPayload(value: unknown): Uint8Array;
						encodeBytesPayload(value: unknown): { base64: string } | null;
						toBytes(value: unknown, encoding?: string): Uint8Array;
					};
				}
			).__agentOSEncoding;

			expect([...installed.base64ToBytes("QUJD")]).toEqual([65, 66, 67]);
			expect(installed.bytesToBase64(new Uint8Array([65, 66, 67]))).toBe(
				"QUJD",
			);
			expect([...installed.toBytes("4142", "hex")]).toEqual([65, 66]);
			expect(installed.encodeBytesPayload("ABC")).toEqual({
				__agentOSType: "bytes",
				base64: "QUJD",
			});
			expect([
				...installed.decodeBytesPayload({
					__agentOSType: "bytes",
					base64: "QUJD",
				}),
			]).toEqual([65, 66, 67]);
		} finally {
			if (previous === undefined) {
				delete (
					globalThis as typeof globalThis & { __agentOSEncoding?: unknown }
				).__agentOSEncoding;
			} else {
				(
					globalThis as typeof globalThis & { __agentOSEncoding?: unknown }
				).__agentOSEncoding = previous;
			}
		}
	});
});
