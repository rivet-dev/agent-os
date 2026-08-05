import { expect, test } from "@playwright/test";

// R4 real-model gate. This never injects a mock model and never falls back to an
// offline answer. The always-on test verifies honest reporting; set
// AGENTOS_REQUIRE_REAL_LANGUAGE_MODEL=1 for the red/green real Chrome Nano gate.

test("reports real Chrome LanguageModel availability without a mock fallback", async ({
	page,
}) => {
	await page.goto("/real-language-model.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const result = await page.evaluate(() =>
		window.__realLanguageModel!.run("Reply with one short sentence."),
	);

	if (result.ok) {
		expect(result.usedRealLanguageModel).toBe(true);
		expect(result.availability).toBe("available");
		expect(result.answer?.length).toBeGreaterThan(0);
	} else {
		expect(result.usedRealLanguageModel).toBe(false);
		expect(result.error).toContain("Chrome LanguageModel is not available");
		expect(result.availability).toBeTruthy();
	}
});

test("requires a real Chrome LanguageModel answer when explicitly enabled", async ({
	page,
}) => {
	test.setTimeout(10 * 60_000);
	test.skip(
		process.env.AGENTOS_REQUIRE_REAL_LANGUAGE_MODEL !== "1",
		"Set AGENTOS_REQUIRE_REAL_LANGUAGE_MODEL=1 to require real Chrome LanguageModel",
	);

	await page.goto("/real-language-model.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	await page.evaluate(() =>
		window.__realLanguageModel!.prepareUserActivatedRun(
			"Say hello from Chrome's built-in model.",
		),
	);
	await page.locator("#run-language-model").click();
	const result = await page.evaluate(() =>
		window.__realLanguageModel!.userActivatedResult(),
	);

	if (!result.ok) {
		throw new Error(
			`${result.error ?? "real Chrome LanguageModel gate failed"} downloadProgress=${JSON.stringify(result.downloadProgress ?? [])}`,
		);
	}
	expect(result.usedRealLanguageModel).toBe(true);
	expect(result.availability).toBe("available");
	expect(result.answer?.length).toBeGreaterThan(0);
});

declare global {
	interface Window {
		__realLanguageModel?: {
			run(prompt: string): Promise<{
				ok: boolean;
				usedRealLanguageModel: boolean;
				availability?: string;
				answer?: string;
				error?: string;
				downloadProgress?: number[];
			}>;
			prepareUserActivatedRun(prompt: string): void;
			userActivatedResult(): Promise<{
				ok: boolean;
				usedRealLanguageModel: boolean;
				availability?: string;
				answer?: string;
				error?: string;
				downloadProgress?: number[];
			}>;
		};
	}
}
