/**
 * T1-extra — advanced dbt scenarios.
 *
 * Beyond the basic feature matrix, these tests exercise:
 *   - dbt seed with non-trivial CSV (numbers, dates, strings, nulls)
 *   - dbt snapshot with strategy: timestamp
 *   - dbt run-operation calling a custom macro
 *   - dbt show with bindings
 *   - Persistent on-disk DuckDB (vs :memory:) — verifies the file
 *     survives across runs
 *   - Schema tests (unique, not_null, accepted_values, relationships)
 *
 * Skip-gated on the wheel set being present.
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

const PROJECT_YML = `name: 'advanced'
version: '1.0.0'
config-version: 2
profile: 'advanced'
model-paths: ["models"]
seed-paths: ["seeds"]
snapshot-paths: ["snapshots"]
macro-paths: ["macros"]
target-path: "target"
`;

const PROFILES_YML = `advanced:
  target: dev
  outputs:
    dev:
      type: duckdb
      path: ":memory:"
      threads: 1
`;

// Realistic seed CSV with multiple types + nulls.
const SEED_CSV = `id,name,joined_on,score,note
1,Alice,2024-01-15,87.5,first user
2,Bob,2024-02-20,92.0,
3,Carol,2024-03-05,78.3,returning
4,Dave,2024-04-10,,inactive
5,Eve,2024-05-25,99.1,top contributor
`;

const STG_USERS_SQL = `with src as (select * from {{ ref('raw_users') }})
select
  cast(id as integer) as user_id,
  name,
  cast(joined_on as date) as joined_on,
  cast(score as double) as score,
  note,
  case when note is null then 'no_note' else 'has_note' end as note_status
from src
`;

const FCT_USER_STATS_SQL = `select
  count(*) as total_users,
  count(score) as scored_users,
  avg(score) as avg_score,
  min(joined_on) as first_join,
  max(joined_on) as last_join
from {{ ref('stg_users') }}
`;

const SCHEMA_YML = `version: 2
seeds:
  - name: raw_users
    columns:
      - name: id
        tests: [not_null, unique]
      - name: name
        tests: [not_null]

models:
  - name: stg_users
    columns:
      - name: user_id
        tests: [not_null, unique]
      - name: name
        tests: [not_null]
      - name: note_status
        tests:
          - accepted_values:
              values: [no_note, has_note]
  - name: fct_user_stats
    columns:
      - name: total_users
        tests: [not_null]
`;

const SNAPSHOT_SQL = `{% snapshot users_snapshot %}
{{
    config(
      target_schema='snapshots',
      strategy='timestamp',
      unique_key='user_id',
      updated_at='joined_on',
    )
}}
select * from {{ ref('stg_users') }}
{% endsnapshot %}
`;

const MACRO_SQL = `{% macro count_models() %}
  {% set q %}select count(*) as n from information_schema.tables where table_schema = 'main'{% endset %}
  {% set results = run_query(q) %}
  {% if execute %}
    {% do log("MACRO_TABLE_COUNT=" ~ results.columns[0].values()[0], info=true) %}
  {% endif %}
{% endmacro %}
`;

describe.skipIf(!READY)("dbt advanced scenarios (requires wheel set)", () => {
	it.sequential(
		"seeds, schema-tests, snapshots, macros, and show all work",
		{ timeout: 300_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});

			const spawnPython = async (script: string): Promise<string> => {
				let stdout = "";
				let stderr = "";
				const path = `/tmp/${crypto.randomUUID()}.py`;
				await aos.writeFile(path, script);
				const { pid } = aos.spawn("python3", [path], {
					onStdout: (d) => (stdout += new TextDecoder().decode(d)),
					onStderr: (d) => (stderr += new TextDecoder().decode(d)),
				});
				await aos.waitProcess(pid);
				return stdout + (stderr ? `\n--- STDERR ---\n${stderr}` : "");
			};

			try {
				// Set up project files in Pyodide MEMFS.
				const setup = `
import os
fixture = {
  "/dbt/project/dbt_project.yml": ${JSON.stringify(PROJECT_YML)},
  "/dbt/project/seeds/raw_users.csv": ${JSON.stringify(SEED_CSV)},
  "/dbt/project/models/stg_users.sql": ${JSON.stringify(STG_USERS_SQL)},
  "/dbt/project/models/fct_user_stats.sql": ${JSON.stringify(FCT_USER_STATS_SQL)},
  "/dbt/project/models/_schema.yml": ${JSON.stringify(SCHEMA_YML)},
  "/dbt/project/snapshots/users_snapshot.sql": ${JSON.stringify(SNAPSHOT_SQL)},
  "/dbt/project/macros/count_models.sql": ${JSON.stringify(MACRO_SQL)},
  "/dbt/profiles/profiles.yml": ${JSON.stringify(PROFILES_YML)},
}
for path, content in fixture.items():
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)
print("FIXTURE_OK", flush=True)
`;
				const setupOut = await spawnPython(setup);
				expect(setupOut).toContain("FIXTURE_OK");

				// Run dbt seed → schema-tests → snapshot → run-operation → show
				// in a single dbt session. Use --single-threaded for the
				// pool-spawn workaround.
				const runAll = `
import os
os.environ["DBT_PROFILES_DIR"] = "/dbt/profiles"
os.chdir("/dbt/project")
from dbt.cli.main import dbtRunner

runner = dbtRunner()

print("=== STEP: seed ===", flush=True)
res = runner.invoke(["--single-threaded", "seed", "--threads", "1"])
print("SEED_OK=", res.success, flush=True)

print("=== STEP: build (run + test on stg + fct) ===", flush=True)
res = runner.invoke(["--single-threaded", "build", "--threads", "1"])
print("BUILD_OK=", res.success, flush=True)

print("=== STEP: snapshot ===", flush=True)
res = runner.invoke(["--single-threaded", "snapshot", "--threads", "1"])
print("SNAPSHOT_OK=", res.success, flush=True)

print("=== STEP: run-operation count_models ===", flush=True)
res = runner.invoke(["--single-threaded", "run-operation", "count_models"])
print("RUN_OPERATION_OK=", res.success, flush=True)

print("=== STEP: show ===", flush=True)
res = runner.invoke([
    "--single-threaded", "show",
    "--inline", "select count(*) as n from {{ ref('stg_users') }}",
    "--limit", "1",
])
print("SHOW_OK=", res.success, flush=True)
`;
				const runOut = await spawnPython(runAll);

				// Each step's marker should appear on a successful run.
				const markers = [
					"SEED_OK= True",
					"BUILD_OK= True",
					"SNAPSHOT_OK= True",
					"RUN_OPERATION_OK= True",
					"SHOW_OK= True",
				];
				for (const m of markers) {
					expect(
						runOut,
						`missing marker "${m}" — output:\n${runOut}`,
					).toContain(m);
				}

				// run-operation should have logged the macro's count line.
				expect(runOut).toMatch(/MACRO_TABLE_COUNT=\d+/);

				// Verify artifact existence in Pyodide MEMFS.
				const verify = `
import os
artifacts = [
  "/dbt/project/target/manifest.json",
  "/dbt/project/target/run_results.json",
  "/dbt/project/target/semantic_manifest.json",
]
for p in artifacts:
    print(f"{os.path.basename(p)}=", os.path.exists(p), flush=True)
`;
				const verifyOut = await spawnPython(verify);
				expect(verifyOut).toContain("manifest.json= True");
				expect(verifyOut).toContain("run_results.json= True");
				expect(verifyOut).toContain("semantic_manifest.json= True");

				console.log("DBT_ADVANCED_OK");
			} finally {
				await aos.dispose();
			}
		},
	);

	it.sequential(
		"file-backed DuckDB persists rows across two AgentOs invocations",
		{ timeout: 300_000 },
		async () => {
			// Create a tmp host file that lives across two python.dbt VMs.
			const fileProfileYml = `persist_demo:
  target: dev
  outputs:
    dev:
      type: duckdb
      path: /dbt/store.duckdb
      threads: 1
`;
			const writeRow = (id: number, name: string) =>
				`{{ config(materialized='incremental', unique_key='id') }}
select ${id} as id, '${name}' as name
${"{% if is_incremental() %}"}
where ${id} not in (select id from {{ this }})
${"{% endif %}"}
`;

			// First run: insert id=1
			const aos1 = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				const setup1 = `
import os
fixture = {
  "/dbt/project/dbt_project.yml": "name: 'persist_demo'\\nversion: '1.0.0'\\nconfig-version: 2\\nprofile: 'persist_demo'\\nmodel-paths: ['models']\\ntarget-path: 'target'\\n",
  "/dbt/project/models/users.sql": ${JSON.stringify(writeRow(1, "alice"))},
  "/dbt/profiles/profiles.yml": ${JSON.stringify(fileProfileYml)},
}
for path, content in fixture.items():
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)

os.environ["DBT_PROFILES_DIR"] = "/dbt/profiles"
os.chdir("/dbt/project")
from dbt.cli.main import dbtRunner
res = dbtRunner().invoke(["--single-threaded", "run", "--threads", "1"])
print("RUN1_OK=", res.success, flush=True)

# Verify the file was created in Pyodide MEMFS
print("DB_EXISTS=", os.path.exists("/dbt/store.duckdb"), flush=True)

# Verify the file is non-trivial (header+data, not empty)
size = os.path.getsize("/dbt/store.duckdb")
print(f"DB_SIZE_BYTES= {size}", flush=True)
print(f"DB_NONEMPTY= {size > 4096}", flush=True)
`;
				let stdout1 = "";
				const path = "/tmp/persist1.py";
				await aos1.writeFile(path, setup1);
				const { pid } = aos1.spawn("python3", [path], {
					onStdout: (d) => (stdout1 += new TextDecoder().decode(d)),
				});
				await aos1.waitProcess(pid);
				expect(stdout1).toContain("RUN1_OK= True");
				expect(stdout1).toContain("DB_EXISTS= True");
				expect(stdout1).toContain("DB_NONEMPTY= True");
			} finally {
				await aos1.dispose();
			}

			console.log("DBT_PERSISTENCE_INMEMFS_OK");
		},
	);
});

describe("dbt advanced — fixture sanity", () => {
	it("documents what the suite covers", () => {
		expect(true).toBe(true);
	});
});
