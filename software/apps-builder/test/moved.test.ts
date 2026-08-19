import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, test } from "vitest";

const execFileAsync = promisify(execFile);
const packageRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const moved = "moved to @rivet-dev/dynamic-apps-builder";

describe("moved package", () => {
	test("rejects the module entry point with the new package name", async () => {
		await expect(import("../src/index.js")).rejects.toThrow(moved);
	});

	test("rejects the historical CLI entry point", async () => {
		await expect(
			execFileAsync(process.execPath, [
				join(packageRoot, "cli", "apps-builder.mjs"),
			]),
		).rejects.toMatchObject({ stderr: expect.stringContaining(moved) });
	});
});
