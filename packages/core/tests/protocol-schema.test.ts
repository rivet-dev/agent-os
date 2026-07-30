import { describe, expect, it } from "vitest";
import {
	SIDECAR_PROTOCOL_SCHEMA,
	validateSidecarProtocolSchema,
} from "../src/protocol-schema.js";

describe("protocol schema", () => {
	it("returns the canonical schema for the supported sidecar protocol", () => {
		expect(
			validateSidecarProtocolSchema({
				name: "agentos-sidecar",
				version: 8,
			}),
		).toBe(SIDECAR_PROTOCOL_SCHEMA);
	});

	it("rejects unsupported schema versions with context", () => {
		expect(() =>
			validateSidecarProtocolSchema({
				name: "agentos-sidecar",
				version: 4,
			}),
		).toThrow("unsupported sidecar protocol schema agentos-sidecar@4");
	});
});
