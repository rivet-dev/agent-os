import { expect, test } from "@playwright/test";

// M4a gate: the real pi ACP adapter boots inside the browser converged executor and
// answers `initialize` (AGENTOS-WEB-ASYNC-AGENTS.md §10 M4a). Proves the actual
// @agentos-software/pi adapter — the same one the native sidecar launches — runs as a
// guest in the kernel-backed node-stdlib executor in real Chromium and speaks ACP.
//
// Requires the pi adapter bundle (built by scripts/build-wasm-test-assets when the
// software/pi dist + @mariozechner/pi-agent-core are present locally; skipped in
// CI where they are absent). Runs the adapter in the executor's persistent-execution
// mode (ExecOptions.persistent) so its async WHATWG-stream stdin pump can produce the
// reply before the run completes.

test("the real pi adapter boots in the converged executor and answers ACP initialize", async ({ page }) => {
	await page.goto("/pi-boot.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	// Skip when the pi adapter bundle was not built (CI without a local pi dist).
	const bundlePresent = await page.evaluate(() =>
		fetch("/pi-adapter.bundle.cjs").then((r) => r.ok).catch(() => false),
	);
	test.skip(!bundlePresent, "pi adapter bundle not built (software/pi dist absent)");

	const result = await page.evaluate(() => window.__piBoot!.run());

	if (result.error) throw new Error(`pi boot errored: ${result.error}\nstdout: ${result.stdout}`);
	// pi answered the ACP initialize handshake with its agent identity.
	expect(result.acpId).toBe(1);
	expect(result.agentName).toBe("pi-sdk-acp");
});
