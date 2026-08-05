#!/usr/bin/env node
// M5 verify CLI (AGENTOS-WEB-ASYNC-AGENTS.md §6): drive the browser-agent demo in a
// REAL browser via the agent-browser CLI and assert the agent answered through the
// proven proxy + Chrome-inference path. This is the end-to-end gate for the leaner-
// agent demo: it boots the kernel + executor in a real tab, runs an ACP turn that
// reaches on-device inference through the in-sandbox OpenAI proxy over loopback, and
// checks the model's reply carried the real prompt back.
//
// Tiers (the spec's "mock gates CI, real Nano best-effort"):
//  - offline-mock: agent-browser's bundled Chromium has no Gemini Nano, so the demo
//    falls back to a deterministic offline reply that echoes the prompt. The default,
//    deterministic gate: we assert the prompt round-tripped through the whole stack.
//  - chrome-local: point agent-browser at a real Chrome with Nano via
//    AGENT_BROWSER_EXECUTABLE_PATH (+ the on-device-model flags); we then assert a
//    non-empty model answer. Best-effort.
//
// Usage: node scripts/verify-demo.mjs ["prompt text"]
// Env:   AGENT_BROWSER_BIN (default: agent-browser), DEMO_PORT (default: 43185).

import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(here, "..");
const AB = process.env.AGENT_BROWSER_BIN ?? "agent-browser";
const PORT = Number(process.env.DEMO_PORT ?? 43185);
const SESSION = "agentos-demo-verify";
const URL = `http://localhost:${PORT}/agent-demo.html`;
const PROMPT = process.argv[2] ?? "Reply with the single word PONG.";

function ab(args, { timeout = 90_000 } = {}) {
	const result = spawnSync(AB, ["--session", SESSION, ...args], { encoding: "utf8", timeout });
	if (result.error) throw result.error;
	return { code: result.status ?? 1, stdout: result.stdout ?? "", stderr: result.stderr ?? "" };
}

/** Run a JS expression in the page and return its (awaited) value, via agent-browser's
 * `--json eval` envelope `{success, data:{result}, error}`. */
function evalJs(expression, opts) {
	const out = ab(["--json", "eval", expression], opts);
	let parsed;
	try {
		parsed = JSON.parse(out.stdout.trim());
	} catch {
		throw new Error(`eval did not return JSON: ${out.stdout || out.stderr}`);
	}
	if (!parsed.success) throw new Error(`eval failed: ${parsed.error ?? "unknown"}`);
	return parsed.data?.result;
}

function fail(message) {
	console.error(`\n❌ DEMO VERIFY FAILED: ${message}`);
	try {
		ab(["close"], { timeout: 15_000 });
	} catch {}
	if (serve) serve.kill();
	process.exit(1);
}

let serve;
async function main() {
	// 1. Build the demo assets (idempotent) and serve them with COOP/COEP (for SAB).
	console.log("• building demo assets…");
	const build = spawnSync("node", [path.join(here, "build-wasm-test-assets.mjs")], { stdio: "inherit" });
	if (build.status !== 0) fail("asset build failed");
	const demoBundle = path.join(packageRoot, "tests", "browser-wasm", "agent-demo.bundle.js");
	if (!existsSync(demoBundle)) fail(`demo bundle missing: ${demoBundle}`);

	console.log(`• serving on :${PORT}…`);
	serve = spawn("node", [path.join(packageRoot, "tests", "browser-wasm", "serve.mjs")], {
		env: { ...process.env, PORT: String(PORT) },
		stdio: "ignore",
	});
	await waitForPort(PORT, 15_000);

	// 2. Open the demo in a real browser and wait for it to boot.
	console.log(`• opening ${URL} in agent-browser…`);
	const opened = ab(["open", URL], { timeout: 60_000 });
	if (opened.code !== 0) fail(`agent-browser open failed: ${opened.stderr || opened.stdout}`);
	const status = ab(["get", "text", "#status"], { timeout: 30_000 }).stdout.trim();
	if (!/ready/.test(status)) fail(`demo did not reach ready (status: ${status || "<none>"})`);

	// 3. Run an ACP turn end-to-end through the proxy + inference path.
	console.log(`• asking the in-browser agent: ${JSON.stringify(PROMPT)}`);
	const result = evalJs(
		`(async () => await window.__agentDemo.run(${JSON.stringify(PROMPT)}))()`,
		{ timeout: 90_000 },
	);
	if (!result || typeof result !== "object") fail(`demo returned no result (${JSON.stringify(result)})`);
	if (result.error) fail(`demo errored: ${result.error}`);

	const { tier, answer } = result;
	console.log(`• inference tier: ${tier}`);
	console.log(`• answer: ${JSON.stringify(answer)}`);

	// 4. Assert per tier.
	if (!answer || typeof answer !== "string" || answer.length === 0) fail("empty answer");
	if (tier === "offline-mock") {
		// Deterministic: the offline model echoes the prompt, so the real question must
		// appear in the answer — proving it traversed kernel → proxy → inference → back.
		if (!answer.includes(PROMPT)) fail(`offline answer did not carry the prompt: ${answer}`);
	} else if (tier === "chrome-local") {
		console.log("• real Chrome on-device model answered (best-effort tier)");
	} else {
		fail(`unexpected tier: ${tier}`);
	}

	console.log(`\n✅ DEMO VERIFIED (${tier}): the in-browser ACP agent answered via the proxy + Chrome-inference path.`);
	try {
		ab(["close"], { timeout: 15_000 });
	} catch {}
	serve.kill();
	process.exit(0);
}

function waitForPort(port, timeoutMs) {
	return new Promise((resolve, reject) => {
		const deadline = Date.now() + timeoutMs;
		const tick = () => {
			const probe = spawnSync(process.execPath, [
				"-e",
				`require("node:http").get("http://localhost:${port}/agent-demo.html",r=>{r.resume();process.exit(r.statusCode?0:1)}).on("error",()=>process.exit(1))`,
			]);
			if (probe.status === 0) return resolve();
			if (Date.now() > deadline) return reject(new Error(`server did not come up on :${port}`));
			setTimeout(tick, 250);
		};
		tick();
	});
}

main().catch((error) => fail(error instanceof Error ? error.message : String(error)));
