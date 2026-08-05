import { expect, test } from "@playwright/test";
import { createRuntime, disposeAllRuntimes, execRuntime } from "./harness.js";

// Runs conformance scenarios through the SAME harness API the legacy conformance
// suite uses, but against the converged conformance harness (every runtime uses
// the wasm kernel). This is the bridge to repointing the full conformance suite
// at the converged path (item 2).

test.afterEach(async ({ page }) => {
	await disposeAllRuntimes(page);
});

test("runs fs + module + stdio + exit through the converged kernel harness", async ({
	page,
}) => {
	await page.goto("/frontend/converged-conformance-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const { runtimeId } = await createRuntime(page);
	const code = [
		"const fs = require('fs');",
		"fs.mkdirSync('/c', { recursive: true });",
		"fs.writeFileSync('/c/m.js', 'module.exports = 99;');",
		"const m = require('/c/m.js');",
		"process.stdout.write('val=' + m);",
	].join("\n");

	const response = (await execRuntime(page, runtimeId, code)) as {
		result: { code?: number };
		stdio: Array<{ channel?: string; message?: unknown }>;
	};
	const stdout = response.stdio
		.filter((event) => event.channel === "stdout")
		.map((event) => (typeof event.message === "string" ? event.message : ""))
		.join("");

	expect(stdout).toBe("val=99");
	expect(response.result.code).toBe(0);
});

test("captures stderr and a non-zero exit code through the converged harness", async ({
	page,
}) => {
	await page.goto("/frontend/converged-conformance-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const { runtimeId } = await createRuntime(page);
	const code = [
		"process.stderr.write('boom');",
		"process.exit(3);",
	].join("\n");

	const response = (await execRuntime(page, runtimeId, code)) as {
		result: { code?: number };
		stdio: Array<{ channel?: string; message?: unknown }>;
	};
	const stderr = response.stdio
		.filter((event) => event.channel === "stderr")
		.map((event) => (typeof event.message === "string" ? event.message : ""))
		.join("");

	expect(stderr).toBe("boom");
	expect(response.result.code).toBe(3);
});

test("reuses one converged runtime across sequential executions", async ({
	page,
}) => {
	await page.goto("/frontend/converged-conformance-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const { runtimeId } = await createRuntime(page);
	await execRuntime(
		page,
		runtimeId,
		"require('fs').writeFileSync('/state.txt', 'persisted');",
	);
	const response = (await execRuntime(
		page,
		runtimeId,
		"process.stdout.write(require('fs').readFileSync('/state.txt', 'utf8'));",
	)) as {
		result: { code?: number };
		stdio: Array<{ channel?: string; message?: unknown }>;
	};
	const stdout = response.stdio
		.filter((event) => event.channel === "stdout")
		.map((event) => (typeof event.message === "string" ? event.message : ""))
		.join("");

	expect(stdout).toBe("persisted");
	expect(response.result.code).toBe(0);
});

test("runs child_process via the host executor under the converged kernel", async ({
	page,
}) => {
	await page.goto("/frontend/converged-conformance-harness.html");
	await expect(page.locator("#harness-status")).toHaveText("ready");

	const { runtimeId } = await createRuntime(page, { commandExecutor: "echo" });
	const code = [
		"const cp = require('child_process');",
		"const r = cp.spawnSync('echo', ['hi-cp']);",
		"process.stdout.write(new TextDecoder().decode(r.stdout));",
	].join("\n");

	const response = (await execRuntime(page, runtimeId, code)) as {
		result: { code?: number };
		stdio: Array<{ channel?: string; message?: unknown }>;
	};
	const stdout = response.stdio
		.filter((event) => event.channel === "stdout")
		.map((event) => (typeof event.message === "string" ? event.message : ""))
		.join("");

	expect(stdout.trim()).toBe("hi-cp");
	expect(response.result.code).toBe(0);
});
