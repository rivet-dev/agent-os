import { readFileSync } from "node:fs";
import { describe, expect, test } from "vitest";

describe("durable session permission surface", () => {
	test("keeps legacy callbacks, expiry, and duplicate event APIs removed", () => {
		const sources = [
			readFileSync(new URL("../src/agent-os.ts", import.meta.url), "utf8"),
			readFileSync(new URL("../src/session-api.ts", import.meta.url), "utf8"),
			readFileSync(
				new URL("../src/sidecar/agentos-acp-protocol.ts", import.meta.url),
				"utf8",
			),
		];
		for (const removed of [
			"onPermissionRequest",
			"AcpPermissionCallback",
			"AcpPermissionRequestEvent",
			"permission_result",
			"expiresAt",
		]) {
			expect(sources.every((source) => !source.includes(removed))).toBe(true);
		}
	});
});
