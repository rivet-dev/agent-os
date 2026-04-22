#!/usr/bin/env npx tsx
/**
 * Master test harness for the dbt-on-agent-os ralph plan.
 *
 * Usage:
 *   pnpm test:dbt-pyodide <subcommand>
 *
 * Subcommands:
 *   unit         T1  Run per-loop unit verifications.
 *   smoke        T2  Cross-loop integration smoke (trivial dbt project).
 *   features     T3  dbt subcommand feature matrix.
 *   jaffle       T4  jaffle_shop end-to-end against in-memory DuckDB.
 *   perf         T5  Cold/warm/jaffle perf budgets.
 *   negative     T6  Sandbox properties don't regress when L4 widens.
 *   fresh        T7  Docker reproduction in clean environment.
 *   all          TF  Run T1..T6 sequentially; emit ACCEPTANCE_GREEN on pass.
 *   --check          Sanity check: harness loads and lists subcommands.
 *
 * Logs land in .agent/test-runs/<timestamp>/.
 */

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

// --- Constants -----------------------------------------------------------

const ROOT = resolve(import.meta.dirname, "..");
const RUNS_DIR = join(ROOT, ".agent/test-runs");
const WHEELS_DIR = join(ROOT, "registry/software/python-wheels/wheels");

const RUN_ID = new Date().toISOString().replace(/[:.]/g, "-").replace(/Z$/, "Z");
const RUN_DIR = join(RUNS_DIR, RUN_ID);

// --- Layer registry ------------------------------------------------------

interface LayerResult {
	id: string;
	promise: string;
	ok: boolean;
	durationMs: number;
	logPath: string;
	skipped?: boolean;
	skipReason?: string;
}

type LayerFn = (logPath: string) => Promise<{
	ok: boolean;
	skipped?: boolean;
	skipReason?: string;
}>;

const LAYERS: Record<string, { id: string; promise: string; fn: LayerFn }> = {
	unit: { id: "T1", promise: "UNIT_LAYER_GREEN", fn: runUnitLayer },
	smoke: { id: "T2", promise: "SMOKE_LAYER_GREEN", fn: runSmokeLayer },
	features: { id: "T3", promise: "FEATURE_MATRIX_GREEN", fn: runFeatureLayer },
	jaffle: { id: "T4", promise: "JAFFLE_SHOP_GREEN", fn: runJaffleLayer },
	perf: { id: "T5", promise: "PERF_BUDGET_OK", fn: runPerfLayer },
	negative: { id: "T6", promise: "NEGATIVE_LAYER_GREEN", fn: runNegativeLayer },
	fresh: { id: "T7", promise: "FRESH_REPRO_OK", fn: runFreshReproLayer },
};

// --- Helpers -------------------------------------------------------------

function ensureRunDir(): void {
	if (!existsSync(RUN_DIR)) {
		mkdirSync(RUN_DIR, { recursive: true });
	}
}

function hasWheelSet(): boolean {
	if (!existsSync(WHEELS_DIR)) return false;
	const wheels = readdirSync(WHEELS_DIR).filter((f) => f.endsWith(".whl"));
	const hasDuckdb = wheels.some((w) => w.startsWith("duckdb-"));
	const hasDbtCore = wheels.some((w) => w.startsWith("dbt_core-"));
	return hasDuckdb && hasDbtCore;
}

function runShell(
	cmd: string,
	args: string[],
	logPath: string,
	opts?: { cwd?: string; env?: Record<string, string> },
): Promise<{ code: number; stdout: string; stderr: string }> {
	return new Promise((resolveP) => {
		const child = spawn(cmd, args, {
			cwd: opts?.cwd ?? ROOT,
			env: { ...process.env, ...(opts?.env ?? {}) },
			stdio: ["ignore", "pipe", "pipe"],
		});
		let stdout = "";
		let stderr = "";
		child.stdout.on("data", (d) => {
			const s = d.toString();
			stdout += s;
			process.stdout.write(s);
		});
		child.stderr.on("data", (d) => {
			const s = d.toString();
			stderr += s;
			process.stderr.write(s);
		});
		child.on("close", (code) => {
			writeFileSync(logPath, `STDOUT:\n${stdout}\n\nSTDERR:\n${stderr}\n`);
			resolveP({ code: code ?? -1, stdout, stderr });
		});
	});
}

function fmt(ms: number): string {
	if (ms < 1000) return `${ms}ms`;
	const s = ms / 1000;
	if (s < 60) return `${s.toFixed(1)}s`;
	const m = Math.floor(s / 60);
	return `${m}m${Math.round(s - m * 60)}s`;
}

// --- Layer implementations -----------------------------------------------

/** T1: per-loop unit verifications via existing test files + verify scripts. */
async function runUnitLayer(logPath: string) {
	const checks: Array<{ name: string; cmd: string[]; needsWheels?: boolean }> = [
		{
			name: "L0 toolchain (skipped — no Pyodide cross-build env in harness)",
			cmd: ["true"],
		},
		{
			name: "L4+L5 driver + bootstrap unit tests",
			cmd: [
				"pnpm",
				"--filter",
				"@rivet-dev/agent-os-python",
				"test",
				"--",
				"wheel-preload",
			],
		},
		{
			name: "L6 dbt-config tests",
			cmd: [
				"pnpm",
				"--filter",
				"@rivet-dev/agent-os-core",
				"test",
				"--silent",
				"dbt-config",
			],
		},
		{
			name: "L7 dbt-smoke fixture sanity (full smoke skip-gated)",
			cmd: [
				"pnpm",
				"--filter",
				"@rivet-dev/agent-os-core",
				"test",
				"--silent",
				"dbt-smoke",
			],
		},
	];

	const log: string[] = [];
	let allOk = true;
	for (const check of checks) {
		log.push(`--- ${check.name} ---`);
		const start = Date.now();
		const result = await runShell(check.cmd[0], check.cmd.slice(1), logPath + ".sub");
		const ms = Date.now() - start;
		const ok = result.code === 0;
		log.push(`  exit=${result.code} duration=${fmt(ms)} ok=${ok}`);
		if (!ok) allOk = false;
	}
	writeFileSync(logPath, log.join("\n") + "\n");
	return { ok: allOk };
}

/** T2: integration smoke. Runs the L7 vitest test (which skips when wheels absent). */
async function runSmokeLayer(logPath: string) {
	if (!hasWheelSet()) {
		writeFileSync(
			logPath,
			"SKIPPED: no wheels at " + WHEELS_DIR + "\n" +
				"Run `make -C registry/python-wheels build-all` to populate.\n",
		);
		return {
			ok: true,
			skipped: true,
			skipReason: "wheels not built",
		};
	}
	const result = await runShell(
		"pnpm",
		[
			"--filter",
			"@rivet-dev/agent-os-core",
			"test",
			"--silent",
			"dbt-smoke",
		],
		logPath,
	);
	return { ok: result.code === 0 };
}

/** T3: dbt subcommand feature matrix. */
async function runFeatureLayer(logPath: string) {
	if (!hasWheelSet()) {
		writeFileSync(logPath, "SKIPPED: requires wheels\n");
		return { ok: true, skipped: true, skipReason: "wheels not built" };
	}
	const result = await runShell(
		"pnpm",
		[
			"--filter",
			"@rivet-dev/agent-os-core",
			"test",
			"--silent",
			"dbt-features",
		],
		logPath,
	);
	return { ok: result.code === 0 };
}

/** T4: jaffle_shop end-to-end. */
async function runJaffleLayer(logPath: string) {
	if (!hasWheelSet()) {
		writeFileSync(logPath, "SKIPPED: requires wheels\n");
		return { ok: true, skipped: true, skipReason: "wheels not built" };
	}
	const result = await runShell(
		"pnpm",
		[
			"--filter",
			"@rivet-dev/agent-os-core",
			"test",
			"--silent",
			"dbt-jaffle",
		],
		logPath,
	);
	return { ok: result.code === 0 };
}

/** T5: perf budgets. */
async function runPerfLayer(logPath: string) {
	if (!hasWheelSet()) {
		writeFileSync(logPath, "SKIPPED: requires wheels\n");
		return { ok: true, skipped: true, skipReason: "wheels not built" };
	}
	const result = await runShell(
		"pnpm",
		[
			"--filter",
			"@rivet-dev/agent-os-core",
			"test",
			"--silent",
			"dbt-perf",
		],
		logPath,
	);
	return { ok: result.code === 0 };
}

/** T6: negative tests — exercise the BLOCK paths. Runs without wheels. */
async function runNegativeLayer(logPath: string) {
	const result = await runShell(
		"pnpm",
		[
			"--filter",
			"@rivet-dev/agent-os-python",
			"test",
			"--",
			"wheel-preload",
		],
		logPath,
	);
	return { ok: result.code === 0 };
}

/** T7: fresh-checkout in Docker. */
async function runFreshReproLayer(logPath: string) {
	const dockerScript = join(ROOT, "scripts/test-dbt-pyodide-docker.sh");
	if (!existsSync(dockerScript)) {
		writeFileSync(logPath, `SKIPPED: ${dockerScript} not found\n`);
		return { ok: false };
	}
	const result = await runShell("bash", [dockerScript], logPath);
	return { ok: result.code === 0 && result.stdout.includes("ACCEPTANCE_GREEN") };
}

// --- TF: orchestrator ----------------------------------------------------

async function runAll(): Promise<number> {
	ensureRunDir();
	const order = ["unit", "negative", "smoke", "features", "jaffle", "perf"];
	const results: LayerResult[] = [];

	for (const name of order) {
		const layer = LAYERS[name];
		const logPath = join(RUN_DIR, `${layer.id}-${name}.log`);
		const start = Date.now();
		console.log(`\n=== ${layer.id} ${name} ===`);
		const result = await layer.fn(logPath);
		const ms = Date.now() - start;
		results.push({
			id: layer.id,
			promise: layer.promise,
			ok: result.ok,
			durationMs: ms,
			logPath,
			skipped: result.skipped,
			skipReason: result.skipReason,
		});
		const status = result.skipped
			? `SKIPPED (${result.skipReason})`
			: result.ok
				? layer.promise
				: `FAILED  promise=${layer.promise}`;
		console.log(`[${layer.id}] ${status}  duration=${fmt(ms)}`);
		if (!result.ok && !result.skipped) break;
	}

	console.log("");
	console.log("=== Summary ===");
	for (const r of results) {
		const status = r.skipped ? "SKIP" : r.ok ? "PASS" : "FAIL";
		console.log(`  [${r.id}] ${status}  ${r.promise}  ${fmt(r.durationMs)}`);
	}

	const allOk = results.every((r) => r.ok);
	const anySkipped = results.some((r) => r.skipped);
	const fullCoverage = !anySkipped;

	const reportPath = join(RUN_DIR, "acceptance.md");
	writeAcceptanceReport(reportPath, results, allOk, fullCoverage);
	console.log(`\nreport: ${reportPath}`);

	if (allOk && fullCoverage) {
		console.log("\n=== ACCEPTANCE_GREEN ===");
		return 0;
	}
	if (allOk && anySkipped) {
		console.log(
			"\n=== ACCEPTANCE_PARTIAL ===  (some layers skipped pending wheels)",
		);
		return 0;
	}
	const failed = results.find((r) => !r.ok && !r.skipped);
	console.log(`\n=== ACCEPTANCE_FAILED ===  layer=${failed?.id}`);
	return 1;
}

function writeAcceptanceReport(
	path: string,
	results: LayerResult[],
	allOk: boolean,
	fullCoverage: boolean,
): void {
	const lines: string[] = [];
	lines.push(`# Acceptance report — ${RUN_ID}\n`);
	lines.push(
		allOk
			? fullCoverage
				? "Status: **ACCEPTANCE_GREEN**\n"
				: "Status: **ACCEPTANCE_PARTIAL** (wheels not yet built)\n"
			: "Status: **ACCEPTANCE_FAILED**\n",
	);
	lines.push("## Layer results\n");
	lines.push("| Layer | Promise | Status | Duration | Log |");
	lines.push("|---|---|---|---|---|");
	for (const r of results) {
		const status = r.skipped
			? `SKIP (${r.skipReason})`
			: r.ok
				? "PASS"
				: "FAIL";
		lines.push(
			`| ${r.id} | ${r.promise} | ${status} | ${fmt(r.durationMs)} | \`${r.logPath}\` |`,
		);
	}
	lines.push("\n## Wheel manifest\n");
	if (existsSync(join(WHEELS_DIR, "lockfile.json"))) {
		try {
			const lock = JSON.parse(
				readFileSync(join(WHEELS_DIR, "lockfile.json"), "utf-8"),
			);
			lines.push(`- generatedAt: ${lock.generatedAt}`);
			lines.push(`- pyodideAbi: ${lock.pyodideAbi}`);
			lines.push(`- pythonTag: ${lock.pythonTag}`);
			lines.push(`- wheels: ${lock.wheels.length} entries`);
		} catch {
			lines.push("- (lockfile present but unparseable)");
		}
	} else {
		lines.push("- No lockfile present (wheels not built).");
	}
	lines.push("\n## Environment\n");
	lines.push(`- node: ${process.version}`);
	lines.push(`- platform: ${process.platform}`);
	lines.push(`- cwd: ${ROOT}`);
	writeFileSync(path, lines.join("\n") + "\n");
}

// --- Main ----------------------------------------------------------------

async function main(): Promise<number> {
	const sub = process.argv[2];

	if (!sub || sub === "--help" || sub === "-h") {
		console.log("Subcommands: unit | smoke | features | jaffle | perf | negative | fresh | all");
		console.log("              --check (sanity)");
		return 0;
	}

	if (sub === "--check") {
		console.log("HARNESS_READY");
		console.log("Subcommands:", Object.keys(LAYERS).join(", "), "all");
		return 0;
	}

	if (sub === "all") {
		return runAll();
	}

	const layer = LAYERS[sub];
	if (!layer) {
		console.error(`Unknown subcommand: ${sub}`);
		return 1;
	}
	ensureRunDir();
	const logPath = join(RUN_DIR, `${layer.id}-${sub}.log`);
	const start = Date.now();
	console.log(`=== ${layer.id} ${sub} ===`);
	const result = await layer.fn(logPath);
	const ms = Date.now() - start;
	const status = result.skipped
		? `SKIPPED (${result.skipReason})`
		: result.ok
			? layer.promise
			: `FAILED  promise=${layer.promise}`;
	console.log(`[${layer.id}] ${status}  duration=${fmt(ms)}`);
	console.log(`log: ${logPath}`);
	return result.ok ? 0 : 1;
}

main()
	.then((code) => process.exit(code))
	.catch((err) => {
		console.error(err);
		process.exit(1);
	});
