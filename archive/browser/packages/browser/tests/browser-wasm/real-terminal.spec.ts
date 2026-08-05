import { expect, test } from "@playwright/test";

// R3 xterm gate: a visible @xterm/xterm terminal is wired to the real kernel
// PTY master. The user keystrokes below go through xterm's DOM input path into
// the real browser brush shell, and shell output is rendered back in xterm.

test("xterm drives the real browser brush shell through the PTY master", async ({
	page,
}) => {
	await page.goto("/real-terminal.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const opened = await page.evaluate(() => window.__realTerminal!.start());
	expect(opened.masterFd).toBeGreaterThan(0);
	expect(opened.slaveFd).toBeGreaterThan(0);
	await expect(page.locator("#status")).toHaveText("running", {
		timeout: 20_000,
	});
	await page.waitForFunction(() =>
		window.__realTerminal!.screen().includes("sh-0.4$ "),
	);

	await page.locator(".xterm").click();
	await page.keyboard.type("/bin/echo xterm-real-ui-ok");
	await page.keyboard.press("Enter");

	await page.waitForFunction(() =>
		window.__realTerminal!.screen().includes("xterm-real-ui-ok"),
	);
	const screen = await page.evaluate(() => window.__realTerminal!.screen());
	expect(screen).toContain("/bin/echo xterm-real-ui-ok");
	expect(screen).toContain("xterm-real-ui-ok");
	expect(screen).toContain("sh-0.4$ ");
	await page.evaluate(() => window.__realTerminal!.dispose());
});

declare global {
	interface Window {
		__realTerminal?: {
			start(): Promise<{ masterFd: number; slaveFd: number }>;
			screen(): string;
			dispose(): Promise<void>;
		};
	}
}
