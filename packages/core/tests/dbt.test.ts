/**
 * SDK-level tests for the `aos.dbt` namespace:
 *   - dbt.run executes the canonical invocation and returns a shaped
 *     DbtRunResult with a non-null tripwire snapshot.
 *   - dbt.run surfaces failures via `success: false` without throwing.
 *   - dbt.tripwire reads counters on a fresh VM.
 *   - dbt.build shortcut populates DbtRunResult.stats from
 *     target/run_results.json.
 *   - dbt.readManifest / dbt.readRunResults return typed shapes.
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

describe.skipIf(!READY)("aos.dbt.run", () => {
	it(
		"runs dbt run against a staged project and fires the bootstrap shims",
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

				const result = await aos.dbt.run(["run", "--threads", "1"], {
					cwd: "/root/dbt-projects/agent_os_demo",
				});

				expect(
					result.success,
					`exception: ${result.exception}\nstdout:\n${result.stdout}\n---\nstderr:\n${result.stderr}`,
				).toBe(true);
				expect(result.exception).toBeNull();

				// Tripwire: at least one monkey-patched shim must have fired. If
				// every counter is zero the helper ran but never hit Pyodide's
				// threading paths — that would mean the dbt-bootstrap contract
				// is silently broken.
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
		"surfaces dbt failures with success:false and no throw",
		{ timeout: 120_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				// Intentionally omit profiles.yml so dbt fails fast at profile
				// resolution — shapes the failure path without relying on
				// dbt-duckdb runtime errors.
				await aos.writeFiles([
					{
						path: "/root/dbt-projects/broken/dbt_project.yml",
						content: PROJECT_YML,
					},
				]);
				const result = await aos.dbt.run(["run", "--threads", "1"], {
					cwd: "/root/dbt-projects/broken",
				});
				expect(result.success).toBe(false);
			} finally {
				await aos.dispose();
			}
		},
	);
});

describe.skipIf(!READY)("aos.dbt.build + artifact readers", () => {
	it(
		"build runs to success and populates DbtRunResult.stats",
		{ timeout: 180_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				const cwd = "/root/dbt-projects/stats_demo";
				await aos.writeFiles([
					{ path: `${cwd}/dbt_project.yml`, content: PROJECT_YML },
					{ path: `${cwd}/models/example.sql`, content: MODEL_SQL },
					{ path: "/root/.dbt/profiles.yml", content: PROFILES_YML },
				]);

				const result = await aos.dbt.build({ cwd });
				expect(result.success).toBe(true);
				expect(result.stats).toBeDefined();
				const stats = result.stats!;
				expect(stats.modelsRun).toBeGreaterThan(0);
				expect(stats.modelsPassed).toBeGreaterThanOrEqual(stats.modelsRun);
				expect(stats.totalElapsedMs).toBeGreaterThanOrEqual(0);
			} finally {
				await aos.dispose();
			}
		},
	);

	it(
		"readManifest / readRunResults return typed shapes after a run",
		{ timeout: 180_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				const cwd = "/root/dbt-projects/artifacts_demo";
				await aos.writeFiles([
					{ path: `${cwd}/dbt_project.yml`, content: PROJECT_YML },
					{ path: `${cwd}/models/example.sql`, content: MODEL_SQL },
					{ path: "/root/.dbt/profiles.yml", content: PROFILES_YML },
				]);

				const r = await aos.dbt.run(["run", "--threads", "1"], { cwd });
				expect(r.success).toBe(true);

				const manifest = await aos.dbt.readManifest(cwd);
				expect(manifest).not.toBeNull();
				expect(manifest!.nodes.length).toBeGreaterThan(0);
				const modelNode = manifest!.nodes.find(
					(n) => n.resourceType === "model",
				);
				expect(modelNode).toBeDefined();
				expect(modelNode!.name).toBe("example");

				const runResults = await aos.dbt.readRunResults(cwd);
				expect(runResults).not.toBeNull();
				expect(runResults!.results.length).toBeGreaterThan(0);
				const firstRow = runResults!.results[0];
				expect(typeof firstRow.uniqueId).toBe("string");
				expect(typeof firstRow.status).toBe("string");
			} finally {
				await aos.dispose();
			}
		},
	);

	it(
		"readManifest returns null when target/ hasn't been written yet",
		{ timeout: 60_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				await aos.writeFile(
					"/root/dbt-projects/empty/dbt_project.yml",
					PROJECT_YML,
				);
				const manifest = await aos.dbt.readManifest(
					"/root/dbt-projects/empty",
				);
				expect(manifest).toBeNull();
			} finally {
				await aos.dispose();
			}
		},
	);
});

describe.skipIf(!READY)("aos.dbt.tripwire", () => {
	it(
		"returns a non-null snapshot with non-negative counters",
		{ timeout: 120_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				const trip = await aos.dbt.tripwire();
				expect(trip).not.toBeNull();
				// Bootstrap ran at worker init, which hits a few shims. Every
				// counter is non-negative by construction.
				expect(trip!.thread_pool_executor_submit).toBeGreaterThanOrEqual(0);
				expect(trip!.workers_alive).toBeGreaterThanOrEqual(0);
			} finally {
				await aos.dispose();
			}
		},
	);
});
