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
//   3. Invokes dbt via the namespaced API: `aos.dbt.build(...)` /
//      `aos.dbt.test(...)`. Both return structured DbtRunResults with
//      aggregate stats parsed from target/run_results.json.
//   4. Reads back the manifest and queries the warehouse via
//      `aos.duckdb.query(...)`.

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
      path: "/root/dbt-projects/demo/demo.duckdb"
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

const aos = await AgentOs.create({
	python: { dbt: true },
});

try {
	const projectCwd = "/root/dbt-projects/demo";
	const warehouse = `${projectCwd}/demo.duckdb`;

	await aos.writeFiles([
		{ path: `${projectCwd}/dbt_project.yml`, content: PROJECT_YML },
		{ path: `${projectCwd}/models/example.sql`, content: MODEL_SQL },
		{ path: `${projectCwd}/models/_schema.yml`, content: SCHEMA_YML },
		{ path: "/root/.dbt/profiles.yml", content: PROFILES_YML },
	]);

	console.log("--- dbt build ---");
	const build = await aos.dbt.build({
		cwd: projectCwd,
		onStdout: (chunk) => process.stdout.write(chunk),
		onStderr: (chunk) => process.stderr.write(chunk),
	});
	console.log(`\ndbt build: success=${build.success}`);
	if (build.stats) {
		console.log(
			`  models: ${build.stats.modelsPassed}/${build.stats.modelsRun} passed`,
		);
		console.log(
			`  tests:  ${build.stats.testsPassed}/${build.stats.testsRun} passed`,
		);
		console.log(`  elapsed: ${build.stats.totalElapsedMs} ms`);
	}
	if (!build.success) {
		console.error("build failed:", build.exception);
		process.exit(1);
	}

	const manifest = await aos.dbt.readManifest(projectCwd);
	console.log(`\nmanifest nodes: ${manifest?.nodes.length ?? 0}`);

	console.log("\n--- duckdb.query against the warehouse ---");
	const rows = await aos.duckdb.query("SELECT * FROM main.example ORDER BY id", {
		database: warehouse,
	});
	console.log(`columns: ${rows.columns.join(", ")}`);
	for (const row of rows.rows) console.log(" ", row.join(" | "));
} finally {
	await aos.dispose();
}
