import { expect, test } from "@playwright/test";

// Drives the human-facing demo page (demo.html) the same way a person would:
// click "Run demo" and assert the live transcript shows a real ACP round-trip
// through the converged agentos wasm sidecar. Keeps the demo honest (it is
// verified in CI, not just openable by hand).

test("demo page runs a live ACP round-trip through the wasm sidecar in Chromium", async ({
	page,
}) => {
	await page.goto("/demo.html");

	await page.getByRole("button", { name: "Run demo" }).click();

	await expect(page.locator("#status")).toHaveText("done", { timeout: 15_000 });
	await expect(page.locator("#sidecar")).toContainText("sidecarId:");

	const transcript = await page.locator("#out").textContent();
	expect(transcript ?? "").toContain("authenticated");
	expect(transcript ?? "").toContain("ext_result");
	expect(transcript ?? "").toContain("AcpErrorResponse");
	expect(transcript ?? "").toContain("unknown ACP session");
});
