import { describe, expect, test } from "vitest";
import { bigIntToSafeNumber } from "../src/numbers.js";

describe("number helpers", () => {
	test("converts safe bigint values", () => {
		expect(bigIntToSafeNumber(42n, "value")).toBe(42);
		expect(bigIntToSafeNumber(BigInt(Number.MIN_SAFE_INTEGER), "value")).toBe(
			Number.MIN_SAFE_INTEGER,
		);
	});

	test("rejects values outside JavaScript safe integer range", () => {
		expect(() =>
			bigIntToSafeNumber(BigInt(Number.MAX_SAFE_INTEGER) + 1n, "value"),
		).toThrow("value exceeds JavaScript safe integer range");
		expect(() =>
			bigIntToSafeNumber(BigInt(Number.MIN_SAFE_INTEGER) - 1n, "value"),
		).toThrow("value exceeds JavaScript safe integer range");
	});
});
