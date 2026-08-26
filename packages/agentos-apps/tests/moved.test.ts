import { describe, expect, test } from "vitest";

const moved = "Dynamic Apps moved to @rivet-dev/dynamic-apps";

describe("moved package", () => {
	test("rejects the root entry point with the new package name", async () => {
		await expect(import("../src/index.js")).rejects.toThrow(moved);
	});

	test("rejects the advanced entry point with the new package name", async () => {
		await expect(import("../src/advanced.js")).rejects.toThrow(moved);
	});
});
