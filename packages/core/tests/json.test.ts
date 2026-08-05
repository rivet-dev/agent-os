import { describe, expect, test } from "vitest";
import { parseJsonUtf8, stringifyJsonUtf8 } from "../src/json.js";

describe("json utf8 helpers", () => {
	test("stringifies and parses JSON payloads", () => {
		const encoded = stringifyJsonUtf8({ ok: true }, "payload");

		expect(encoded).toBe('{"ok":true}');
		expect(parseJsonUtf8(encoded, "payload")).toEqual({ ok: true });
	});

	test("rejects non-serializable values", () => {
		expect(() => stringifyJsonUtf8(undefined, "payload")).toThrow(
			"payload must be JSON-serializable",
		);
	});

	test("adds context to parse errors", () => {
		expect(() => parseJsonUtf8("{", "payload")).toThrow(
			"invalid payload JSON payload",
		);
	});
});
