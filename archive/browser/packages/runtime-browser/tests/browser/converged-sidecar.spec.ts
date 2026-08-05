import { expect, test } from "@playwright/test";

// Drives the converged TypeScript stack against the REAL web-target wasm kernel
// loaded on the main thread of a real Chromium page — the in-browser counterpart
// to the Node integration test, proving guest filesystem syscalls route through
// the wasm kernel in the actual target environment.

test("routes guest filesystem syscalls through the wasm kernel in Chromium", async ({
	page,
}) => {
	await page.goto("/frontend/converged-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedHarness;
		if (!harness) {
			throw new Error("converged harness not installed");
		}
		return harness.runFilesystem();
	});

	expect(result.readText).toBe("browser-converged");
	expect(result.stat.size).toBe(17);
	expect(result.stat.isDirectory).toBe(false);
	const sorted = [...result.dir].sort((a, b) => a.name.localeCompare(b.name));
	expect(sorted).toEqual([
		{ name: "a.txt", isDirectory: false, isSymbolicLink: false },
		{ name: "sub", isDirectory: true, isSymbolicLink: false },
	]);
});

test("resolves modules through the wasm kernel filesystem in Chromium", async ({
	page,
}) => {
	await page.goto("/frontend/converged-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedHarness;
		if (!harness) {
			throw new Error("converged harness not installed");
		}
		return harness.runModuleResolution();
	});

	expect(result.relative).toBe("/app/util.js");
	expect(result.barePackage).toBe("/app/node_modules/left-pad/index.js");
});

test("routes guest TCP loopback through the wasm kernel in Chromium", async ({
	page,
}) => {
	await page.goto("/frontend/converged-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedHarness;
		if (!harness) {
			throw new Error("converged harness not installed");
		}
		return harness.runNetLoopback();
	});

	expect(result.received).toBe("ping-loopback");
});

test("routes guest UDP loopback through the wasm kernel in Chromium", async ({
	page,
}) => {
	await page.goto("/frontend/converged-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedHarness;
		if (!harness) {
			throw new Error("converged harness not installed");
		}
		return harness.runUdpLoopback();
	});

	expect(result.received).toBe("udp-datagram");
	expect(result.remotePort).toBe(45612);
});

test("routes guest dgram.* socket ops through the wasm kernel in Chromium", async ({
	page,
}) => {
	await page.goto("/frontend/converged-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedHarness;
		if (!harness) {
			throw new Error("converged harness not installed");
		}
		return harness.runDgramSocket();
	});

	expect(result.received).toBe("dgram-bridge");
});

test("enforces a declarative deny-fs-read policy in the wasm kernel", async ({
	page,
}) => {
	await page.goto("/frontend/converged-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedHarness;
		if (!harness) {
			throw new Error("converged harness not installed");
		}
		return harness.runFsReadDenied();
	});

	expect(result.wrote).toBe(true);
	expect(result.readDenied).toBe(true);
});

test("enforces a declarative deny-network-port policy in the wasm kernel", async ({
	page,
}) => {
	await page.goto("/frontend/converged-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const result = await page.evaluate(async () => {
		const harness = window.__convergedHarness;
		if (!harness) {
			throw new Error("converged harness not installed");
		}
		return harness.runNetPortDenied();
	});

	expect(result.deniedPortListen).toBe(true);
	expect(result.allowedPortListen).toBe(true);
});
