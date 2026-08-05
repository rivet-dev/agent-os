#!/usr/bin/env node
// R5/R6 verifier for AGENTOS-WEB-REAL-TERMINAL.md: require the REAL pi TUI
// to answer a typed prompt through Chrome's REAL `window.LanguageModel`.

import { spawnSync } from "node:child_process";

const port = process.env.AGENTOS_WASM_TEST_PORT ?? "43397";
const result = spawnSync(
	"pnpm",
	[
		"--filter",
		"@rivet-dev/agentos-browser",
		"exec",
		"playwright",
		"test",
		"--config=playwright.wasm.config.ts",
		"tests/browser-wasm/pi-tui.spec.ts",
		"--reporter=line",
	],
	{
		stdio: "inherit",
		env: {
			...process.env,
			AGENTOS_WASM_TEST_PORT: port,
			AGENTOS_REQUIRE_REAL_PI_TUI: "1",
			AGENTOS_REQUIRE_REAL_PI_MODEL: "1",
		},
	},
);

if (result.error) throw result.error;
process.exit(result.status ?? 1);
