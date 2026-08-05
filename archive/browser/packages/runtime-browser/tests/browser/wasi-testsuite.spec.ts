// WASI preview1 conformance subset (browser backend).
//
// Runs a vendored subset of the official WebAssembly/wasi-testsuite preview1
// modules through the SHARED WASI runner on the browser backend, asserting the
// upstream spec's exit code + stdout. The identical manifest is run on the
// native backend by `crates/executor-conformance/tests/wasm.rs` (wasi_testsuite_subset), so
// the one shared runner is conformance-checked on both backends.

import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { createRuntime, disposeAllRuntimes, execRuntime, openHarnessPage } from "./harness.js";

interface WasiTestsuiteCase {
	name: string;
	exitCode: number;
	stdout?: string;
	wasmBase64: string;
}

const manifest = JSON.parse(
	readFileSync(
		new URL(
			"../../../../tests/fixtures/wasi-testsuite-subset.json",
			import.meta.url,
		),
		"utf8",
	),
) as { cases: WasiTestsuiteCase[] };

test.beforeEach(async ({ page }) => {
	await openHarnessPage(page);
});

test.afterEach(async ({ page }) => {
	await disposeAllRuntimes(page);
});

for (const testCase of manifest.cases) {
	test(`wasi-testsuite preview1: ${testCase.name}`, async ({ page }) => {
		const { runtimeId } = await createRuntime(page);
		const result = await execRuntime(
			page,
			runtimeId,
			`
				(async () => {
					const { WASI } = require("node:wasi");
					const bytes = Uint8Array.from(atob(${JSON.stringify(testCase.wasmBase64)}), (char) => char.charCodeAt(0));
					const wasi = new WASI({ returnOnExit: true });
					const { instance } = await WebAssembly.instantiate(bytes, {
						wasi_snapshot_preview1: wasi.wasiImport,
					});
					const exitCode = wasi.start(instance);
					console.log("wasi-exit:" + exitCode);
				})();
			`,
		);

		// The guest JS itself always completes (it catches the WASI exit via
		// returnOnExit), so the execution exit is 0; the WASI exit code is logged.
		expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
		expect(result.stdio).toContainEqual({
			channel: "stdout",
			message: `wasi-exit:${testCase.exitCode}\n`,
		});
		if (typeof testCase.stdout === "string" && testCase.stdout.length > 0) {
			const stdout = result.stdio
				.filter((entry) => entry.channel === "stdout")
				.map((entry) => entry.message)
				.join("");
			expect(stdout).toContain(testCase.stdout);
		}
	});
}
