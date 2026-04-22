// Run dbt-core + dbt-duckdb inside the VM, against an in-memory DuckDB.
//
// Requires:
//   - the @rivet-dev/agent-os-python-wheels package to have its wheels/
//     directory populated. Build via:
//       make -C ../../registry/python-wheels build-all
//
// What this does:
//   1. Boots an AgentOs with python.dbt: true, which mounts the vendored
//      Pyodide wheels at /wheels and pre-installs dbt + DuckDB.
//   2. Writes a tiny project (one model, one schema test) into the VM.
//   3. Invokes dbt-core programmatically via dbtRunner inside Pyodide.
//   4. Reads back target/manifest.json to confirm the run completed.

import { AgentOs } from "@rivet-dev/agent-os-core";

const PROJECT_YML = `name: 'demo'
version: '1.0.0'
config-version: 2
profile: 'demo'
model-paths: ["models"]
target-path: "target"
`;

const PROFILES_YML = `demo:
  target: dev
  outputs:
    dev:
      type: duckdb
      path: ":memory:"
      threads: 1
`;

const MODEL_SQL = `{{ config(materialized='table') }}
select 1 as id, 'hello' as name
union all
select 2 as id, 'world' as name
`;

const SCHEMA_YML = `version: 2
models:
  - name: example
    columns:
      - name: id
        tests:
          - not_null
          - unique
`;

const vm = await AgentOs.create({
	python: { dbt: true },
});

try {
	await vm.writeFiles([
		{ path: "/root/dbt-projects/demo/dbt_project.yml", content: PROJECT_YML },
		{ path: "/root/dbt-projects/demo/models/example.sql", content: MODEL_SQL },
		{ path: "/root/dbt-projects/demo/models/_schema.yml", content: SCHEMA_YML },
		{ path: "/root/.dbt/profiles.yml", content: PROFILES_YML },
	]);

	await vm.writeFile(
		"/tmp/run_dbt.py",
		`
import os
os.chdir("/root/dbt-projects/demo")
from dbt.cli.main import dbtRunner
print("--- dbt run ---")
res = dbtRunner().invoke(["run", "--threads", "1"])
print("success=", res.success)
if res.exception is not None:
    print("exception=", repr(res.exception))
print("--- dbt test ---")
res2 = dbtRunner().invoke(["test", "--threads", "1"])
print("success=", res2.success)
`,
	);

	const result = await vm.exec("python /tmp/run_dbt.py");
	console.log(result.stdout);
	if (result.exitCode !== 0) {
		console.error("Exit code:", result.exitCode);
		console.error(result.stderr);
		process.exit(1);
	}

	const manifestExists = await vm.exists(
		"/root/dbt-projects/demo/target/manifest.json",
	);
	console.log("manifest.json present:", manifestExists);
} finally {
	await vm.dispose();
}
