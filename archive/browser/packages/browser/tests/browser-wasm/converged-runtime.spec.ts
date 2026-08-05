import { expect, test } from "@playwright/test";

// End-to-end converged runtime proof in real Chromium: a real Agent OS guest runs
// in the @rivet-dev/agentos-runtime-browser worker, and its fs.* syscalls are serviced over the
// converged SharedArrayBuffer sync-bridge by the Agent OS wasm kernel (plugged in
// via createAgentOsConvergedSidecar). This exercises the full converged path the
// DoD requires — guest syscalls routed to the wasm kernel, kernel as sole
// enforcement point — not just the raw ACP wire round-trip.

test.describe("agentos converged runtime in Chromium", () => {
	test.beforeEach(async ({ page }) => {
		await page.goto("/converged.html");
		await expect(page.locator("#status")).toHaveText("ready", {
			timeout: 20_000,
		});
	});

	test("a real guest does filesystem I/O through the converged wasm kernel", async ({
		page,
	}) => {
		const result = await page.evaluate(() =>
			window.__agentosConvergedRuntime!.runFs(),
		);
		expect(result.error ?? "").toBe("");
		expect(result.exitCode).toBe(0);
		expect(result.stdout).toBe("agentos-converged");
	});

	test("a real guest require()s a kernel-fs module through the converged kernel", async ({
		page,
	}) => {
		const result = await page.evaluate(() =>
			window.__agentosConvergedRuntime!.runRequire(),
		);
		expect(result.error ?? "").toBe("");
		expect(result.exitCode).toBe(0);
		expect(result.stdout).toBe("42");
	});
});

declare global {
	interface Window {
		__agentosConvergedRuntime?: {
			runFs(): Promise<{ stdout: string; exitCode: number; error?: string }>;
			runRequire(): Promise<{
				stdout: string;
				exitCode: number;
				error?: string;
			}>;
		};
	}
}
