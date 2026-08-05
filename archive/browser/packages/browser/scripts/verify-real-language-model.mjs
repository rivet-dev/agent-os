#!/usr/bin/env node
// R4 verifier for AGENTOS-WEB-REAL-TERMINAL.md: require a REAL Chrome
// `window.LanguageModel` answer. No mock/fallback tier is accepted here.

import { spawn, spawnSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { chromium } from "@playwright/test";

const port = process.env.AGENTOS_WASM_TEST_PORT ?? "43390";
const persistentUserDataDir = process.env.AGENTOS_CHROME_USER_DATA_DIR;

const MODEL_DOWNLOAD_DEFAULT_IGNORES = [
	"--disable-background-networking",
	"--disable-component-update",
	"--disable-features=AvoidUnnecessaryBeforeUnloadCheckSync,BoundaryEventDispatchTracksNodeRemoval,DestroyProfileOnBrowserClose,DialMediaRouteProvider,GlobalMediaControls,HttpsUpgrades,LensOverlay,MediaRouter,PaintHolding,ThirdPartyStoragePartitioning,Translate,AutoDeElevate,RenderDocument,OptimizationHints",
];

function chromeArgs() {
	return (process.env.AGENTOS_CHROME_ARGS ?? "")
		.split(/\s+/)
		.map((arg) => arg.trim())
		.filter(Boolean);
}

function ignoreDefaultArgs() {
	const fromEnv =
		process.env.AGENTOS_CHROME_IGNORE_DEFAULT_ARGS?.trim().startsWith("[")
			? JSON.parse(process.env.AGENTOS_CHROME_IGNORE_DEFAULT_ARGS)
			: (process.env.AGENTOS_CHROME_IGNORE_DEFAULT_ARGS ?? "")
					.split(/\n/)
					.map((arg) => arg.trim())
					.filter(Boolean);
	if (process.env.AGENTOS_CHROME_ALLOW_MODEL_DOWNLOAD === "1") {
		fromEnv.push(...MODEL_DOWNLOAD_DEFAULT_IGNORES);
	}
	return Array.from(new Set(fromEnv));
}

async function waitForServer(url, timeoutMs = 30_000) {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		try {
			const response = await fetch(url);
			if (response.ok) return;
		} catch {}
		await new Promise((resolve) => setTimeout(resolve, 250));
	}
	throw new Error(`timed out waiting for ${url}`);
}

async function waitForReady(page, timeoutMs = 20_000) {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		const text = await page.locator("#status").textContent().catch(() => "");
		if (text === "ready") return;
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	throw new Error("real-language-model page did not become ready");
}

async function runPersistentVerifier() {
	mkdirSync(persistentUserDataDir, { recursive: true });
	const build = spawnSync("node", ["./scripts/build-wasm-test-assets.mjs"], {
		stdio: "inherit",
		env: process.env,
	});
	if (build.error) throw build.error;
	if (build.status !== 0) process.exit(build.status ?? 1);

	const server = spawn("node", ["./tests/browser-wasm/serve.mjs"], {
		stdio: "inherit",
		env: { ...process.env, PORT: port },
	});
	let exitCode = 1;
	try {
		const baseURL = `http://localhost:${port}`;
		await waitForServer(`${baseURL}/real-language-model.html`);
		const context = await chromium.launchPersistentContext(persistentUserDataDir, {
			executablePath: process.env.AGENTOS_CHROME_EXECUTABLE_PATH,
			channel: process.env.AGENTOS_CHROME_CHANNEL,
			headless: process.env.AGENTOS_CHROME_HEADLESS === "0" ? false : true,
			args: chromeArgs(),
			ignoreDefaultArgs: ignoreDefaultArgs(),
		});
		try {
			context.setDefaultTimeout(10 * 60_000);
			const page = context.pages()[0] ?? (await context.newPage());
			await page.goto(`${baseURL}/real-language-model.html`);
			await waitForReady(page);
			await page.evaluate(() =>
				window.__realLanguageModel.prepareUserActivatedRun(
					"Say hello from Chrome's built-in model.",
				),
			);
			await page.locator("#run-language-model").click();
			const result = await page.evaluate(() =>
				window.__realLanguageModel.userActivatedResult(),
			);
			if (!result.ok) {
				console.error(
					`${result.error ?? "real Chrome LanguageModel gate failed"} downloadProgress=${JSON.stringify(result.downloadProgress ?? [])}`,
				);
				exitCode = 1;
			} else {
				console.log(
					`real Chrome LanguageModel PASS availability=${result.availability} answer=${JSON.stringify(result.answer)}`,
				);
				exitCode = 0;
			}
		} finally {
			await context.close();
		}
	} finally {
		server.kill("SIGTERM");
	}
	process.exit(exitCode);
}

if (persistentUserDataDir) {
	await runPersistentVerifier();
}

const result = spawnSync(
	"pnpm",
	[
		"--filter",
		"@rivet-dev/agentos-browser",
		"exec",
		"playwright",
		"test",
		"--config=playwright.wasm.config.ts",
		"tests/browser-wasm/real-language-model.spec.ts",
		"--reporter=line",
	],
	{
		stdio: "inherit",
		env: {
			...process.env,
			AGENTOS_WASM_TEST_PORT: port,
			AGENTOS_REQUIRE_REAL_LANGUAGE_MODEL: "1",
		},
	},
);

if (result.error) throw result.error;
process.exit(result.status ?? 1);
