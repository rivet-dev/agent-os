import { homedir } from "node:os";
import path from "node:path";
import { expect, test } from "@playwright/test";

// M4b end-to-end demo + screenshot capture: the real pi agent answering prompts entirely
// in the browser (AGENTOS-WEB-ASYNC-AGENTS.md §6/M4b). Drives the visible pi-demo UI
// through a full turn for two prompts, asserts the answers, and saves screenshots of the
// whole flow to ~/tmp/web-agent/ (the artifacts requested for the working e2e demo).

const SHOTS = path.join(homedir(), "tmp", "web-agent");

test("the pi browser demo answers prompts end-to-end (with screenshots)", async ({ page }) => {
	test.setTimeout(120_000);
	await page.goto("/pi-demo.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });
	await page.screenshot({ path: path.join(SHOTS, "01-demo-loaded.png") });

	const bundlePresent = await page.evaluate(() =>
		fetch("/pi-adapter.bundle.cjs").then((r) => r.ok).catch(() => false),
	);
	test.skip(!bundlePresent, "pi adapter bundle not built");

	// Prompt 1: a question with a known mock answer.
	await page.fill("#prompt", "What is 2+2?");
	await page.click("#run");
	// Capture an in-progress frame (best-effort; the boot+turn takes a moment).
	await page.waitForTimeout(900);
	await page.screenshot({ path: path.join(SHOTS, "02-running.png") });
	await expect(page.locator("#answer")).toContainText("4", { timeout: 90_000 });
	await expect(page.locator("#answer")).toContainText("2 + 2 = 4");
	await page.screenshot({ path: path.join(SHOTS, "03-answer-math.png") });

	// Prompt 2: a free-form question (the mock echoes it, proving the full round-trip).
	await page.fill("#prompt", "Who are you?");
	await page.click("#run");
	await expect(page.locator("#answer")).toContainText("pi", { timeout: 90_000 });
	await expect(page.locator("#answer")).toContainText("browser");
	await page.screenshot({ path: path.join(SHOTS, "04-answer-whoareyou.png") });

	// Final full-page shot of the completed flow.
	await page.screenshot({ path: path.join(SHOTS, "05-final-fullpage.png"), fullPage: true });
});
