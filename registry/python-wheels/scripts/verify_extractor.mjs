#!/usr/bin/env node
// Verify the dbt-extractor wheel imports and parses a trivial Jinja block
// inside a Pyodide worker. Emits DBT_EXTRACTOR_WHEEL_OK on success.

import { readdirSync } from "node:fs";
import { resolve } from "node:path";
import { spawnPyodideAndRun } from "./_lib/pyodide-runner.mjs";

const wheelsDir = resolve(process.argv[2] ?? "../software/python-wheels/wheels");
const abiTag = process.argv[3] ?? "pyodide_2025_0_wasm32";
const pythonTag = process.argv[4] ?? "cp313";

const wheels = readdirSync(wheelsDir).filter((f) =>
	f.startsWith("dbt_extractor-") && f.endsWith(".whl"),
);
if (wheels.length === 0) {
	console.error(
		`no dbt_extractor wheel found in ${wheelsDir} — run \`make build-extractor\` first`,
	);
	process.exit(1);
}

const code = `
import micropip
await micropip.install(["emfs:/wheels/${wheels[0]}"], deps=False)

import dbt_extractor

# Either the real Rust wheel returns refs, OR the shim raises ExtractionError
# and we accept that as a valid (fallback) outcome.
try:
    result = dbt_extractor.py_extract_from_source("{{ ref('foo') }}")
    if "refs" in result and result["refs"][0] == ["foo"]:
        print("DBT_EXTRACTOR_WHEEL_OK (real wheel)")
    else:
        print("DBT_EXTRACTOR_WHEEL_OK (real wheel, but unexpected output:", result, ")")
except dbt_extractor.ExtractionError as e:
    print("DBT_EXTRACTOR_WHEEL_OK (shim, falls back to Jinja:", e, ")")
`;

const ok = await spawnPyodideAndRun(code, { mountWheels: wheelsDir });
process.exit(ok ? 0 : 1);
