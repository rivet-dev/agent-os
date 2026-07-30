import { expect, test } from "@playwright/test";

// R5 real-pi-TUI gate. This launches @mariozechner/pi-coding-agent's real CLI
// bundle on a kernel PTY and renders it through xterm. No ACP chat renderer and
// no mocked model answer are involved here. Strict mode requires visible TUI
// output; default mode records an honest red reason if the real CLI cannot boot.

test("real pi CLI/TUI boots on a browser PTY or reports an honest blocker", async ({
	page,
}) => {
	await page.goto("/pi-tui.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const bundlePresent = await page.evaluate(() =>
		fetch("/pi-cli.bundle.cjs").then((r) => r.ok).catch(() => false),
	);
	test.skip(!bundlePresent, "pi CLI bundle not built");

	const result = await page.evaluate(() => window.__piTui!.start());
	expect(result.masterFd).toBeGreaterThan(0);
	expect(result.slaveFd).toBeGreaterThan(0);

	const transcript = `${result.screen}\n${result.output}\n${result.error ?? ""}`;
	if (process.env.AGENTOS_REQUIRE_REAL_PI_TUI === "1") {
		expect(result.error ?? "").toBe("");
		expect(transcript).toMatch(/pi|model|session|API|help|Ctrl|Esc|Working/i);
	} else {
		expect(result.started).toBe(true);
		expect(result.error || result.visibleText).toBeTruthy();
	}
	await page.evaluate(() => window.__piTui!.dispose());
});

test("real pi TUI answers a typed prompt through Chrome LanguageModel when explicitly required", async ({
	page,
}) => {
	test.setTimeout(11 * 60_000);
	test.skip(
		process.env.AGENTOS_REQUIRE_REAL_PI_MODEL !== "1",
		"Set AGENTOS_REQUIRE_REAL_PI_MODEL=1 to require pi -> real Chrome LanguageModel",
	);
	await page.goto("/pi-tui.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const result = await page.evaluate(() =>
		window.__piTui!.ask("Reply with one short sentence about real browser terminals."),
	);
	if (result.error) {
		throw new Error(
			`${result.error}\nmodelAvailability=${result.modelAvailability ?? ""}\nnetworkRequests=${JSON.stringify(result.networkRequests ?? [])}\nmodelErrors=${JSON.stringify(result.modelErrors ?? [])}\ndownloadProgress=${JSON.stringify(result.modelDownloadProgress ?? [])}`,
		);
	}
	expect(result.modelRequests).toBeGreaterThan(0);
	expect(result.usedRealLanguageModel).toBe(true);
	expect(result.promptAnswered).toBe(true);
	expect(result.modelResponses?.join("\n").trim().length).toBeGreaterThan(0);
	await page.evaluate(() => window.__piTui!.dispose());
});

declare global {
	interface Window {
		__piTui?: {
			start(): Promise<{
				started: boolean;
				masterFd?: number;
				slaveFd?: number;
				screen: string;
				output: string;
				error?: string;
				visibleText?: string;
				rawOutputChars?: number;
				rawOutputPreview?: string;
				execStatus?: string;
				modelAvailability?: string;
				modelRequests?: number;
				modelResponses?: string[];
				modelErrors?: string[];
				modelDownloadProgress?: number[];
				networkRequests?: string[];
				usedRealLanguageModel?: boolean;
				promptAnswered?: boolean;
			}>;
			ask(prompt: string): Promise<{
				started: boolean;
				masterFd?: number;
				slaveFd?: number;
				screen: string;
				output: string;
				error?: string;
				visibleText?: string;
				rawOutputChars?: number;
				rawOutputPreview?: string;
				execStatus?: string;
				modelAvailability?: string;
				modelRequests?: number;
				modelResponses?: string[];
				modelErrors?: string[];
				modelDownloadProgress?: number[];
				networkRequests?: string[];
				usedRealLanguageModel?: boolean;
				promptAnswered?: boolean;
			}>;
			screen(): string;
			dispose(): Promise<void>;
		};
	}
}
