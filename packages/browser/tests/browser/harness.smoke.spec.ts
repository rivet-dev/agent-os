import { expect, test } from "@playwright/test";
import { openHarnessPage, smokeHarness } from "./harness.js";

test("playground harness boots a real browser runtime in Chromium", async ({
	page,
}) => {
	await openHarnessPage(page);

	const result = await smokeHarness(page);

	expect(result.crossOriginIsolated).toBe(true);
	expect(result.workerUrl).toContain("/agent-os-worker.js");
	expect(result.result.code).toBe(0);
	expect(result.stdio).toEqual([
		{
			channel: "stdout",
			message: "harness-ready",
		},
	]);
});
