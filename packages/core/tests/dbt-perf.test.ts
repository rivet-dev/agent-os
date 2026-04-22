/**
 * T5 — perf budgets.
 *
 * Measures cold-start, warm-call, and trivial-project dbt run latencies
 * against the documented budgets in the plan. Skip-gated on wheels.
 *
 * Writes results to .agent/test-runs/<ts>/perf.json so we can track
 * regressions across runs.
 */
import { existsSync, mkdirSync, readdirSync, writeFileSync } from "node:fs";
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

function hasWheelSet(): boolean {
	if (!existsSync(wheelsHostDir)) return false;
	const wheels = readdirSync(wheelsHostDir).filter((f) => f.endsWith(".whl"));
	return (
		wheels.some((w) => w.startsWith("duckdb-")) &&
		wheels.some((w) => w.startsWith("dbt_core-"))
	);
}

const READY = hasWheelSet();

const BUDGETS = {
	coldStartMs: 30_000,
	warmRunMs: 5_000,
	trivialDbtRunMs: 15_000,
	memCeilingBytes: 2 * 1024 * 1024 * 1024,
};

const PROJECT_YML = `name: 'perf'
version: '1.0.0'
config-version: 2
profile: 'perf'
model-paths: ["models"]
target-path: "target"
`;

const PROFILES_YML = `perf:
  target: dev
  outputs:
    dev:
      type: duckdb
      path: ":memory:"
      threads: 1
`;

const M1 = "{{ config(materialized='table') }}\nselect 1 as id\n";
const M2 = "{{ config(materialized='table') }}\nselect id+1 as id from {{ ref('m1') }}\n";
const M3 = "{{ config(materialized='table') }}\nselect id+1 as id from {{ ref('m2') }}\n";

function ms(start: bigint): number {
	return Number(process.hrtime.bigint() - start) / 1e6;
}

async function sampleTreeRss(): Promise<number> {
	const { spawn: spawnP } = await import("node:child_process");
	return new Promise<number>((resolveP) => {
		const ps = spawnP("ps", ["-o", "rss=", "-g", String(process.pid)]);
		let out = "";
		ps.stdout.on("data", (d) => (out += d));
		ps.on("close", () => {
			const lines = out
				.trim()
				.split("\n")
				.map((l) => parseInt(l.trim(), 10))
				.filter((n) => Number.isFinite(n));
			const totalKb = lines.reduce((a, b) => a + b, 0);
			resolveP(totalKb * 1024);
		});
		ps.on("error", () => resolveP(process.memoryUsage().rss));
	});
}

async function spawnPython(aos: AgentOs, scriptPath: string): Promise<string> {
	let stdout = "";
	const { pid } = aos.spawn("python3", [scriptPath], {
		onStdout: (d) => (stdout += new TextDecoder().decode(d)),
	});
	await aos.waitProcess(pid);
	return stdout;
}

describe.skipIf(!READY)("T5 — perf budgets (requires wheels)", () => {
	it("respects cold/warm/trivial budgets", { timeout: 240_000 }, async () => {
		const samples: Record<string, number[]> = {
			cold: [],
			warm: [],
			trivial: [],
			peakRssBytes: [],
		};

		for (let i = 0; i < 2; i++) {
			const t0 = process.hrtime.bigint();
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				await aos.writeFile(
					"/tmp/probe.py",
					"import dbt\nprint('ok', flush=True)",
				);
				const out1 = await spawnPython(aos, "/tmp/probe.py");
				expect(out1).toContain("ok");
				samples.cold.push(ms(t0));

				const t1 = process.hrtime.bigint();
				const out2 = await spawnPython(aos, "/tmp/probe.py");
				expect(out2).toContain("ok");
				samples.warm.push(ms(t1));

				const setup = `
import os
fixture = {
  "/dbt/project/dbt_project.yml": ${JSON.stringify(PROJECT_YML)},
  "/dbt/project/models/m1.sql": ${JSON.stringify(M1)},
  "/dbt/project/models/m2.sql": ${JSON.stringify(M2)},
  "/dbt/project/models/m3.sql": ${JSON.stringify(M3)},
  "/dbt/profiles/profiles.yml": ${JSON.stringify(PROFILES_YML)},
}
for path, content in fixture.items():
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)
print("FIXTURE_OK", flush=True)
`;
				await aos.writeFile("/tmp/setup_perf.py", setup);
				await spawnPython(aos, "/tmp/setup_perf.py");

				await aos.writeFile(
					"/tmp/run_dbt.py",
					`
import os
os.environ["DBT_PROFILES_DIR"] = "/dbt/profiles"
os.chdir("/dbt/project")
from dbt.cli.main import dbtRunner
res = dbtRunner().invoke(["--single-threaded", "run", "--threads", "1"])
print("DBT_RUN_SUCCESS=", res.success, flush=True)
`,
				);
				const t2 = process.hrtime.bigint();
				const out3 = await spawnPython(aos, "/tmp/run_dbt.py");
				samples.trivial.push(ms(t2));
				expect(out3).toContain("DBT_RUN_SUCCESS= True");

				samples.peakRssBytes.push(await sampleTreeRss());
			} finally {
				await aos.dispose();
			}
		}

		const median = (xs: number[]) =>
			xs.slice().sort((a, b) => a - b)[Math.floor(xs.length / 2)];
		const summary = {
			coldMedianMs: median(samples.cold),
			warmMedianMs: median(samples.warm),
			trivialMedianMs: median(samples.trivial),
			rssMedianBytes: median(samples.peakRssBytes),
			budgets: BUDGETS,
			samples,
		};

		const outDir = resolve(__dirname, "../../../.agent/test-runs/perf");
		if (!existsSync(outDir)) mkdirSync(outDir, { recursive: true });
		const outFile = resolve(outDir, `${Date.now()}.json`);
		writeFileSync(outFile, JSON.stringify(summary, null, 2));
		console.log(`perf summary written: ${outFile}`);
		console.log(JSON.stringify(summary, null, 2));

		expect(summary.coldMedianMs).toBeLessThan(BUDGETS.coldStartMs);
		expect(summary.warmMedianMs).toBeLessThan(BUDGETS.warmRunMs);
		expect(summary.trivialMedianMs).toBeLessThan(BUDGETS.trivialDbtRunMs);
		expect(summary.rssMedianBytes).toBeLessThan(BUDGETS.memCeilingBytes);
		console.log("PERF_BUDGET_OK");
	});
});

describe("T5 — budgets sanity (always runs)", () => {
	it("documents the budget constants", () => {
		expect(BUDGETS.coldStartMs).toBeGreaterThan(0);
		expect(BUDGETS.warmRunMs).toBeLessThan(BUDGETS.coldStartMs);
	});
});
