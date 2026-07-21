import { expect, test } from "@playwright/test";

// Proves the converged Agent OS wasm sidecar (agentos-sidecar-browser, which links
// the agentos converged kernel + the ACP extension) loads and runs in REAL
// Chromium, and processes wire frames.

test("agentos wasm sidecar boots in Chromium and reports its identity", async ({
	page,
}) => {
	await page.goto("/");
	await expect(page.locator("#status")).toHaveText("ready");
	const sidecarId = await page.evaluate(() => window.__agentosWasm.bootId());
	expect(sidecarId).toBe("agentos-native-sidecar-browser");
});

test("agentos wasm sidecar processes wire frames in Chromium", async ({
	page,
}) => {
	await page.goto("/");
	await expect(page.locator("#status")).toHaveText("ready");
	const result = await page.evaluate(() => window.__agentosWasm.pushInvalidFrame());
	// Either a structured error response frame or a thrown decode error — both prove
	// the wasm sidecar executed the wire-frame path in the browser.
	expect(result.threw || result.isBytes).toBe(true);
});

declare global {
	interface Window {
		__agentosWasm: {
			bootId(): Promise<string>;
			pushInvalidFrame(): Promise<{
				threw: boolean;
				isBytes?: boolean;
				len?: number;
				message?: string;
			}>;
		};
	}
}
