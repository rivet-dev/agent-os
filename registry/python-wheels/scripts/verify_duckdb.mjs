#!/usr/bin/env node
// Verify the DuckDB Pyodide wheel imports and runs a trivial query.
// Emits DUCKDB_WHEEL_OK on success.

import { readdirSync } from "node:fs";
import { resolve } from "node:path";
import { spawnPyodideAndRun } from "./_lib/pyodide-runner.mjs";

const wheelsDir = resolve(process.argv[2] ?? "../software/python-wheels/wheels");

const wheels = readdirSync(wheelsDir).filter(
	(f) => f.startsWith("duckdb-") && f.endsWith(".whl"),
);
if (wheels.length === 0) {
	console.error(
		`no duckdb wheel found in ${wheelsDir} — run \`make fetch-duckdb\` first`,
	);
	process.exit(1);
}

const code = `
import micropip
await micropip.install(["emfs:/wheels/${wheels[0]}"], deps=False)

import duckdb
con = duckdb.connect(":memory:")
assert con.execute("SELECT 42 AS x").fetchall() == [(42,)]
print("duckdb version:", con.execute("SELECT version()").fetchone()[0])

con.execute("CREATE TABLE t AS SELECT * FROM range(0, 1000)")
n = con.execute("SELECT count(*) FROM t").fetchone()[0]
assert n == 1000, f"expected 1000 rows, got {n}"

extensions = con.execute("SELECT extension_name FROM duckdb_extensions() WHERE installed = true").fetchall()
print("loaded extensions:", [e[0] for e in extensions])

con.close()
print("DUCKDB_WHEEL_OK")
`;

const ok = await spawnPyodideAndRun(code, { mountWheels: wheelsDir });
process.exit(ok ? 0 : 1);
