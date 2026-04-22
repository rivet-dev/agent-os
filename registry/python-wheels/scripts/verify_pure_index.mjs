#!/usr/bin/env node
// Verify the pure-Python wheel index installs and the dbt stack imports.
// Emits PURE_PY_INDEX_OK on success.

import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnPyodideAndRun } from "./_lib/pyodide-runner.mjs";

const wheelsDir = resolve(process.argv[2] ?? "../software/python-wheels/wheels");
const lockPath = resolve(wheelsDir, "lockfile.json");
if (!existsSync(lockPath)) {
	console.error(`lockfile not found: ${lockPath}`);
	console.error("Run `make build-pure-py-index` first.");
	process.exit(1);
}

const lock = JSON.parse(readFileSync(lockPath, "utf-8"));
const wheelUrls = lock.wheels.map((w) => `emfs:/wheels/${w.filename}`);

const code = `
import micropip
await micropip.install(${JSON.stringify(wheelUrls)}, deps=False)

# Import the heavy hitters; transitive imports follow.
import dbt
import dbt_common
import dbt_adapters
import dbt_semantic_interfaces
import dbt_extractor
import agate
import mashumaro

print("dbt version available:", getattr(dbt, "__version__", "unknown"))
print("PURE_PY_INDEX_OK")
`;

const ok = await spawnPyodideAndRun(code, { mountWheels: wheelsDir });
process.exit(ok ? 0 : 1);
