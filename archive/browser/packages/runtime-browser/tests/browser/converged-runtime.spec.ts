import { expect, test } from "@playwright/test";

// End-to-end proof of the LIVE converged executor: a real BrowserRuntimeDriver
// with the converged sidecar option runs a real guest whose filesystem syscalls
// are serviced by the wasm kernel (not the legacy in-process TS kernel), in real
// Chromium.

test("runs a real guest whose fs syscalls hit the wasm kernel", async ({
	page,
}) => {
	await page.goto("/frontend/converged-runtime-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedRuntimeHarness;
		if (!harness) {
			throw new Error("converged runtime harness not installed");
		}
		return harness.run();
	});

	expect(result.error).toBeUndefined();
	expect(result.stdout).toBe("converged-live");
	expect(result.exitCode).toBe(0);
});

test("runs a real guest that require()s a module resolved by the wasm kernel", async ({
	page,
}) => {
	await page.goto("/frontend/converged-runtime-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedRuntimeHarness;
		if (!harness) {
			throw new Error("converged runtime harness not installed");
		}
		return harness.runRequire();
	});

	expect(result.error).toBeUndefined();
	expect(result.stdout).toBe("42");
	expect(result.exitCode).toBe(0);
});

test("runs a real guest doing dgram UDP loopback through the wasm kernel", async ({
	page,
}) => {
	await page.goto("/frontend/converged-runtime-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedRuntimeHarness;
		if (!harness) {
			throw new Error("converged runtime harness not installed");
		}
		return harness.runDgram();
	});

	expect(result.error).toBeUndefined();
	expect(result.stdout).toBe("live-dgram");
	expect(result.exitCode).toBe(0);
});

test("runs a real guest exercising a broad fs surface through the wasm kernel", async ({
	page,
}) => {
	await page.goto("/frontend/converged-runtime-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedRuntimeHarness;
		if (!harness) {
			throw new Error("converged runtime harness not installed");
		}
		return harness.runBroadFs();
	});

	expect(result.error).toBeUndefined();
	expect(result.exitCode).toBe(0);
	const parsed = JSON.parse(result.stdout);
	expect(parsed.content).toBe("hello world");
	expect(parsed.size).toBe(11);
	expect(parsed.entries).toEqual(["b.txt"]);
	expect(parsed.exists).toBe(true);
});
