/**
 * T3 — dbt subcommand feature matrix.
 *
 * Exercises every dbt subcommand the plan claims to support against a
 * trivial project. Skip-gated on the wheel set being present so the
 * harness can run in CI even before wheels are built.
 */
import { existsSync, readdirSync } from "node:fs";
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

const PROJECT_YML = `name: 'features_demo'
version: '1.0.0'
config-version: 2
profile: 'features_demo'
model-paths: ["models"]
seed-paths: ["seeds"]
snapshot-paths: ["snapshots"]
target-path: "target"
`;

const PROFILES_YML = `features_demo:
  target: dev
  outputs:
    dev:
      type: duckdb
      path: ":memory:"
      threads: 1
`;

const MODEL_A = `{{ config(materialized='table') }}
select 1 as id, 'a' as name union all select 2, 'b' union all select 3, 'c'
`;

const MODEL_B = `{{ config(materialized='view') }}
select id, name, name || '_v' as decorated from {{ ref('a') }}
`;

const MODEL_C = `{{ config(materialized='table') }}
select count(*) as n from {{ ref('b') }}
`;

const SCHEMA_YML = `version: 2
models:
  - name: a
    columns:
      - name: id
        tests: [not_null, unique]
      - name: name
        tests: [not_null]
`;

const SEED_CSV = `id,label
10,alpha
20,beta
30,gamma
`;

describe.skipIf(!READY)("T3 — dbt feature matrix (requires wheel set)", () => {
	let aos: AgentOs;

	const writeProjectInVm = async () => {
		// Write fixtures via Python so they land in Pyodide MEMFS (where
		// dbt will read them from) rather than the kernel VFS.
		const setup = `
import os
fixture = {
  "/dbt/project/dbt_project.yml": ${JSON.stringify(PROJECT_YML)},
  "/dbt/project/models/a.sql": ${JSON.stringify(MODEL_A)},
  "/dbt/project/models/b.sql": ${JSON.stringify(MODEL_B)},
  "/dbt/project/models/c.sql": ${JSON.stringify(MODEL_C)},
  "/dbt/project/models/_schema.yml": ${JSON.stringify(SCHEMA_YML)},
  "/dbt/project/seeds/labels.csv": ${JSON.stringify(SEED_CSV)},
  "/dbt/profiles/profiles.yml": ${JSON.stringify(PROFILES_YML)},
}
for path, content in fixture.items():
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)
print("FIXTURE_OK")
`;
		await aos.writeFile("/tmp/setup_features.py", setup);
		const { pid } = aos.spawn("python3", ["/tmp/setup_features.py"]);
		await aos.waitProcess(pid);
	};

	const runDbt = async (
		args: string[],
	): Promise<{ ok: boolean; stdout: string; stderr: string }> => {
		const py = `
import os, traceback
os.environ["DBT_PROFILES_DIR"] = "/dbt/profiles"
os.chdir("/dbt/project")
from dbt.cli.main import dbtRunner
try:
    res = dbtRunner().invoke(["--single-threaded"] + ${JSON.stringify(args)})
    print("DBT_RUN_SUCCESS=", res.success)
    if res.exception is not None:
        print("exception=", repr(res.exception))
except Exception:
    traceback.print_exc()
    print("DBT_RUN_SUCCESS= False")
`;
		const path = "/tmp/run_dbt.py";
		await aos.writeFile(path, py);
		let stdout = "";
		let stderr = "";
		const { pid } = aos.spawn("python3", [path], {
			onStdout: (d) => (stdout += new TextDecoder().decode(d)),
			onStderr: (d) => (stderr += new TextDecoder().decode(d)),
		});
		await aos.waitProcess(pid);
		return {
			ok: stdout.includes("DBT_RUN_SUCCESS= True"),
			stdout,
			stderr,
		};
	};

	it.sequential("sets up the project once and runs the matrix", {
		timeout: 300_000,
	}, async () => {
		aos = await AgentOs.create({
			permissions: allowAll,
			python: { dbt: true },
		});
		try {
			await writeProjectInVm();

			const subcommands: Array<[string, string[]]> = [
				["parse", ["parse"]],
				["seed", ["seed", "--threads", "1"]],
				["compile", ["compile", "--threads", "1"]],
				["run", ["run", "--threads", "1"]],
				["test", ["test", "--threads", "1"]],
				["build", ["build", "--threads", "1"]],
				["docs generate", ["docs", "generate"]],
				["list", ["list"]],
				["show", ["show", "--inline", "select 42 as v", "--limit", "1"]],
			];

			const results: Array<{
				name: string;
				ok: boolean;
				stdout: string;
				stderr: string;
			}> = [];
			for (const [name, args] of subcommands) {
				const r = await runDbt(args);
				results.push({ name, ok: r.ok, stdout: r.stdout, stderr: r.stderr });
			}

			const failed = results.filter((r) => !r.ok);
			const failureDetails = failed
				.map(
					(f) =>
						`\n=== ${f.name} FAILED ===\n--- stdout ---\n${f.stdout}\n--- stderr ---\n${f.stderr}`,
				)
				.join("\n");
			expect(
				failed.map((f) => f.name),
				`failed subcommands:${failureDetails}`,
			).toEqual([]);

			// Verify target/ artifacts via Python (they live in Pyodide MEMFS).
			let probeOut = "";
			const verifyPy = `
import os
for name in ("manifest.json", "run_results.json", "index.html"):
    path = "/dbt/project/target/" + name
    print(f"{name}=", os.path.exists(path))
`;
			await aos.writeFile("/tmp/verify.py", verifyPy);
			const { pid: vpid } = aos.spawn("python3", ["/tmp/verify.py"], {
				onStdout: (d) => (probeOut += new TextDecoder().decode(d)),
			});
			await aos.waitProcess(vpid);
			expect(probeOut).toContain("manifest.json= True");
			expect(probeOut).toContain("run_results.json= True");
			expect(probeOut).toContain("index.html= True");

			console.log("FEATURE_MATRIX_GREEN");
		} finally {
			await aos.dispose();
		}
	});
});

describe("T3 — fixture sanity (always runs)", () => {
	it("documents matrix coverage when skipped", () => {
		if (READY) {
			console.log("T3 wheels present; matrix will run.");
		} else {
			console.log(
				"T3 skipped — populate " +
					wheelsHostDir +
					" to enable the full matrix.",
			);
		}
		expect(true).toBe(true);
	});
});
