import { expect, test } from "@playwright/test";

// M4b gate: a COMPLETE pi turn in the browser — initialize → session/new →
// session/prompt → a model answer (AGENTOS-WEB-ASYNC-AGENTS.md §10 M4b). The real full
// pi SDK (adapter + pi-coding-agent + pi-agent-core + pi-ai) runs in the converged
// executor; pi-ai's global fetch reaches the test server's /v1/messages, which answers
// with an Anthropic SSE carrying a sentinel (the mock chrome-llm; the in-sandbox proxy
// guest + host-callback is the production refinement, proven by async-proxy). pi parses
// the SSE and returns the assistant message as the prompt content.

test("a full pi turn in the browser answers session/prompt via the model (chrome-llm sentinel)", async ({ page }) => {
	test.setTimeout(120_000); // booting the full 16MB pi SDK + a model round-trip
	await page.goto("/pi-prompt.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const bundlePresent = await page.evaluate(() =>
		fetch("/pi-adapter.bundle.cjs").then((r) => r.ok).catch(() => false),
	);
	test.skip(!bundlePresent, "pi adapter bundle not built");

	const result = await page.evaluate(() => window.__piPrompt!.run());
	if (result.error) throw new Error(`pi prompt errored: ${result.error}\nstdout: ${result.stdout}`);
	expect(result.answer).toContain("PONG_FROM_PI_IN_BROWSER");
});

declare global {
	interface Window {
		__piPrompt?: { run(): Promise<{ answer?: string; stdout: string; error?: string }> };
	}
}
