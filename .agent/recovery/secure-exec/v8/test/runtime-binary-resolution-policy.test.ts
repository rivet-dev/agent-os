import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

describe("runtime binary resolution policy", () => {
	it("prefers local native/v8-runtime builds before packaged binaries", () => {
		const source = readFileSync(new URL("../src/runtime.ts", import.meta.url), "utf8");

		const releaseIndex = source.indexOf("../../../native/v8-runtime/target/release/secure-exec-v8");
		const platformIndex = source.indexOf("// 2. Try platform-specific npm package");

		expect(releaseIndex).toBeGreaterThanOrEqual(0);
		expect(platformIndex).toBeGreaterThanOrEqual(0);
		expect(releaseIndex).toBeLessThan(platformIndex);
	});
});
