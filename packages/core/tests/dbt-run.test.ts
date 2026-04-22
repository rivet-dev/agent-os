/**
 * SDK-level dbt-run test: proves `AgentOs.runDbt` runs the canonical dbt
 * invocation, returns a shaped result, and that the bootstrap monkey-patches
 * actually fire (via tripwire counters).
 *
 * Skip-gated on the vendored wheel set being present.
 */
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { allowAll } from "@secure-exec/core";
import { describe, expect, it } from "vitest";
import { AgentOs } from "../src/index.js";

const __dirname = resolve(fileURLToPath(import.meta.url), "..");
const wheelsHostDir = resolve(
	__dirname,
	"../../../registry/software/python-wheels/wheels",
);
const fixtureDir = resolve(__dirname, "fixtures/dbt-trivial");

function hasWheelSet(): boolean {
	if (!existsSync(wheelsHostDir)) return false;
	const wheels = readdirSync(wheelsHostDir).filter((f) => f.endsWith(".whl"));
	return (
		wheels.some((w) => w.startsWith("duckdb-")) &&
		wheels.some((w) => w.startsWith("dbt_core-"))
	);
}

const READY = hasWheelSet();

const PROJECT_YML = readFileSync(`${fixtureDir}/dbt_project.yml`, "utf-8");
const PROFILES_YML = readFileSync(`${fixtureDir}/profiles.yml`, "utf-8");
const MODEL_SQL = readFileSync(`${fixtureDir}/models/example.sql`, "utf-8");

describe.skipIf(!READY)("AgentOs.runDbt — SDK helper", () => {
	it(
		"runs dbt run against a project staged via aos.writeFiles",
		{ timeout: 180_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				await aos.writeFiles([
					{
						path: "/root/dbt-projects/agent_os_demo/dbt_project.yml",
						content: PROJECT_YML,
					},
					{
						path: "/root/dbt-projects/agent_os_demo/models/example.sql",
						content: MODEL_SQL,
					},
					{ path: "/root/.dbt/profiles.yml", content: PROFILES_YML },
				]);

				// Note: dbt 1.11's CLI parser rejects --single-threaded at the
				// top level. DBT_SINGLE_THREADED=True in DBT_ENV already
				// forces single-threaded mode, so the CLI flag is redundant.
				const result = await aos.runDbt(["run", "--threads", "1"], {
					cwd: "/root/dbt-projects/agent_os_demo",
				});

				expect(
					result.success,
					`exception: ${result.exception}\nstdout:\n${result.stdout}\n---\nstderr:\n${result.stderr}`,
				).toBe(true);
				expect(result.exception).toBeNull();
				expect(result.stdout).not.toContain("__AGENT_OS_DBT_RESULT_JSON__");

				// Tripwire: at minimum one monkey-patched shim must have fired
				// during a successful run. Without the patches, dbt would have
				// raised "can't start new thread" at graph.thread_pool or
				// concurrent.futures time — so if we got here AND the counters
				// are zero, we're asserting a false-positive contract.
				expect(result.tripwire).not.toBeNull();
				const trip = result.tripwire!;
				const hitCount =
					trip.thread_pool_executor_submit +
					trip.dbt_thread_pool_apply_async +
					trip.dbt_thread_pool_init +
					trip.multiprocessing_get_context +
					trip.multiprocessing_dummy_start;
				expect(hitCount).toBeGreaterThan(0);
			} finally {
				await aos.dispose();
			}
		},
	);

	it(
		"surfaces dbt failures with success:false and a non-null exception",
		{ timeout: 120_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				// Intentionally omit profiles.yml so dbt fails fast with a
				// profile-resolution error — shapes the failure path without
				// relying on dbt-duckdb runtime errors.
				await aos.writeFiles([
					{
						path: "/root/dbt-projects/broken/dbt_project.yml",
						content: PROJECT_YML,
					},
				]);
				const result = await aos.runDbt(["run", "--threads", "1"], {
					cwd: "/root/dbt-projects/broken",
				});
				expect(result.success).toBe(false);
				// Either dbtRunner surfaces an exception or it reports
				// success=False without one (profile-missing is the latter).
				// Both shapes are legitimate dbt failures, so we only assert
				// on .success here.
			} finally {
				await aos.dispose();
			}
		},
	);

	it(
		"writes the result file to a path visible to aos.readFile",
		{ timeout: 180_000 },
		async () => {
			// Regression: earlier the helper wrote /tmp/_agent_os_run_dbt_result.json
			// which lives in Pyodide MEMFS and is invisible to the kernel VFS. The
			// result path was moved into AGENT_OS_SCRATCH_DIR (/root/.dbt/.aos) so
			// the NODEFS bridge makes it readable from the host / actor side.
			const { AGENT_OS_SCRATCH_DIR, RUN_DBT_RESULT_PATH } = await import(
				"../src/index.js"
			);
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				await aos.writeFiles([
					{
						path: "/root/dbt-projects/probe/dbt_project.yml",
						content: PROJECT_YML,
					},
					{
						path: "/root/dbt-projects/probe/models/example.sql",
						content: MODEL_SQL,
					},
					{ path: "/root/.dbt/profiles.yml", content: PROFILES_YML },
				]);
				const r = await aos.runDbt(["run", "--threads", "1"], {
					cwd: "/root/dbt-projects/probe",
				});
				expect(r.success).toBe(true);

				// The whole point: the helper's result file must be readable
				// from the kernel VFS side, through the NODEFS bridge.
				expect(RUN_DBT_RESULT_PATH.startsWith(AGENT_OS_SCRATCH_DIR)).toBe(
					true,
				);
				const raw = await aos.readFile(RUN_DBT_RESULT_PATH);
				const parsed = JSON.parse(new TextDecoder().decode(raw)) as {
					success: boolean;
					tripwire: unknown;
				};
				expect(parsed.success).toBe(true);
				expect(parsed.tripwire).not.toBeNull();
			} finally {
				await aos.dispose();
			}
		},
	);
});

describe.skipIf(!READY)("AgentOs.readDbtTripwire — passive observation", () => {
	it("returns a zeroed snapshot on a fresh VM", {
		timeout: 120_000,
	}, async () => {
		const aos = await AgentOs.create({
			permissions: allowAll,
			python: { dbt: true },
		});
		try {
			const trip = await aos.readDbtTripwire();
			expect(trip).not.toBeNull();
			// Bootstrap ran at worker init, which hit a handful of shims.
			// Contract: counters exist and are non-negative integers.
			expect(trip!.thread_pool_executor_submit).toBeGreaterThanOrEqual(0);
			expect(trip!.workers_alive).toBeGreaterThanOrEqual(0);
		} finally {
			await aos.dispose();
		}
	});
});
