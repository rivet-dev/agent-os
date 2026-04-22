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
	// We need at least dbt-core + duckdb to attempt a real run.
	const hasDuckdb = wheels.some((w) => w.startsWith("duckdb-"));
	const hasDbtCore = wheels.some((w) => w.startsWith("dbt_core-"));
	return hasDuckdb && hasDbtCore;
}

const READY = hasWheelSet();

const PROJECT_YML = readFileSync(`${fixtureDir}/dbt_project.yml`, "utf-8");
const PROFILES_YML = readFileSync(`${fixtureDir}/profiles.yml`, "utf-8");
const MODEL_SQL = readFileSync(`${fixtureDir}/models/example.sql`, "utf-8");
const SCHEMA_YML = readFileSync(`${fixtureDir}/models/_schema.yml`, "utf-8");

describe.skipIf(!READY)("L7 — dbt run end-to-end (requires wheel set)", () => {
	it("runs `dbt run` against in-memory DuckDB", {
		timeout: 180_000,
	}, async () => {
		const aos = await AgentOs.create({
			permissions: allowAll,
			python: { dbt: true },
		});
		try {
			// dbt operates on Pyodide's MEMFS, which is separate from the
			// kernel VFS. Write the fixture files via Python so they land in
			// the same filesystem dbt will read from. Also: minimize fixture
			// to one model with no schema tests, isolating dbt-duckdb behavior.
			const setupScript = `
import os, sys, traceback
fixture = {
  "/dbt/project/dbt_project.yml": ${JSON.stringify(PROJECT_YML)},
  "/dbt/project/models/example.sql": ${JSON.stringify(MODEL_SQL)},
  "/dbt/profiles/profiles.yml": ${JSON.stringify(PROFILES_YML)},
}
for path, content in fixture.items():
  os.makedirs(os.path.dirname(path), exist_ok=True)
  with open(path, "w") as f:
    f.write(content)

os.environ["DBT_PROFILES_DIR"] = "/dbt/profiles"
os.chdir("/dbt/project")
print("CWD:", os.getcwd(), flush=True)

print("--- dbt run --single-threaded ---", flush=True)
from dbt.cli.main import dbtRunner
try:
    res = dbtRunner().invoke(["--single-threaded", "run", "--threads", "1"])
    print("DBT_RUN_SUCCESS=", res.success, flush=True)
    if res.exception is not None:
        print("DBT_RUN_EXCEPTION=", repr(res.exception), flush=True)
    if res.result is not None and hasattr(res.result, "results"):
        for r in res.result.results:
            name = getattr(getattr(r, "node", None), "name", "?")
            print(f"  node={name} status={r.status} message={r.message}", flush=True)
except Exception:
    traceback.print_exc(file=sys.stderr)
# Do NOT sys.exit() — Pyodide's webloop wraps SystemExit oddly. Let the
# script exit naturally; the test verifies via the DBT_RUN_SUCCESS marker.
`;
			await aos.writeFile("/tmp/run_dbt.py", setupScript);
			// Spawn the python runtime directly — no shell required (no WASM
			// commands package needed beyond what python.dbt brings in).
			let stdout = "";
			let stderr = "";
			const { pid } = aos.spawn("python3", ["/tmp/run_dbt.py"], {
				onStdout: (d) => {
					stdout += new TextDecoder().decode(d);
				},
				onStderr: (d) => {
					stderr += new TextDecoder().decode(d);
				},
			});
			await aos.waitProcess(pid);
			// We assert against the DBT_RUN_SUCCESS marker rather than the
			// process exit code because Pyodide's worker can return non-zero
			// exit even when sys.exit(0) is called from inside a webloop
			// task. The marker is the authoritative source for whether dbt
			// itself succeeded.
			expect(
				stdout,
				`--- stdout ---\n${stdout}\n--- stderr ---\n${stderr}`,
			).toContain("DBT_RUN_SUCCESS= True");
			console.log("DBT_RUN_OK");

			// target/manifest.json lives in Pyodide MEMFS, not kernel VFS.
			// Verify via a follow-up python invocation.
			let manifestStdout = "";
			const { pid: pid2 } = aos.spawn(
				"python3",
				[
					"-c",
					"import os; print(os.path.exists('/dbt/project/target/manifest.json'))",
				],
				{
					onStdout: (d) => {
						manifestStdout += new TextDecoder().decode(d);
					},
				},
			);
			await aos.waitProcess(pid2);
			expect(manifestStdout.trim()).toBe("True");

			console.log("DBT_RUN_OK");
		} finally {
			await aos.dispose();
		}
	});
});

describe("L7 — fixture sanity (always runs)", () => {
	it("fixture files are well-formed YAML", () => {
		expect(PROJECT_YML).toContain("name: 'agent_os_demo'");
		expect(PROFILES_YML).toContain("type: duckdb");
		expect(PROFILES_YML).toContain('path: ":memory:"');
		expect(MODEL_SQL).toContain("select 1 as id");
		expect(SCHEMA_YML).toContain("not_null");
	});

	it("documents what's required to enable the full smoke", () => {
		// This test exists so the test runner output makes it obvious to a
		// developer which precondition is missing when L7 is skipped.
		if (READY) {
			console.log("L7 wheels present; full smoke will run.");
		} else {
			console.log(
				"L7 wheels not present at " +
					wheelsHostDir +
					" — run `make -C registry/python-wheels build-all` to enable the full smoke.",
			);
		}
		expect(true).toBe(true);
	});
});
