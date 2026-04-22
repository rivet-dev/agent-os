// Run dbt-core + dbt-duckdb inside the VM, against an in-memory DuckDB.
//
// Requires:
//   - the @rivet-dev/agent-os-python-wheels package to have its wheels/
//     directory populated. Build via:
//       make -C ../../registry/python-wheels build-all
//
// What this does:
//   1. Boots an AgentOs with python.dbt: true. This mounts the vendored
//      Pyodide wheels, pre-installs the dbt + DuckDB stack, auto-creates
//      /root/.dbt/ and /root/dbt-projects/, and applies the dbt-bootstrap
//      monkey-patches for Pyodide's single-threaded runtime.
//   2. Writes a tiny project (one model, one schema test) into the VM.
//   3. Invokes dbt via `vm.runDbt(...)` — the canonical high-level API
//      that spawns python3, streams stdout/stderr, and returns a
//      structured DbtRunResult.
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

	const projectCwd = "/root/dbt-projects/demo";

	console.log("--- dbt run ---");
	const runResult = await vm.runDbt(["run", "--threads", "1"], {
		cwd: projectCwd,
		onStdout: (chunk) => process.stdout.write(chunk),
		onStderr: (chunk) => process.stderr.write(chunk),
	});
	console.log(`\ndbt run: success=${runResult.success}`);
	if (runResult.exception) console.error("exception:", runResult.exception);
	if (!runResult.success) {
		console.error("dbt run failed");
		process.exit(1);
	}

	console.log("\n--- dbt test ---");
	const testResult = await vm.runDbt(["test", "--threads", "1"], {
		cwd: projectCwd,
		onStdout: (chunk) => process.stdout.write(chunk),
		onStderr: (chunk) => process.stderr.write(chunk),
	});
	console.log(`\ndbt test: success=${testResult.success}`);
	if (testResult.exception) console.error("exception:", testResult.exception);

	const manifestExists = await vm.exists(`${projectCwd}/target/manifest.json`);
	console.log("\nmanifest.json present:", manifestExists);
} finally {
	await vm.dispose();
}
