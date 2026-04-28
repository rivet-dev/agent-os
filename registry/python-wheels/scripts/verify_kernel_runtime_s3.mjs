#!/usr/bin/env node
/**
 * End-to-end smoke test for the playground's actual production path:
 *
 *   1. Spawn createPythonRuntime (the runtime Pi VMs use).
 *   2. micropip-install pyodide-httpfs from the vendored wheel set.
 *   3. Open an in-memory DuckDB connection.
 *   4. Register pyodide_httpfs with that connection.
 *   5. Read s3://layerr-dev/test-sab/sample.csv via SAB+SigV4 → MinIO.
 *   6. Verify row count matches.
 *
 * Requires:
 *   - MinIO running at http://localhost:9000 (matches .env defaults).
 *   - Test CSV uploaded by `bun /tmp/setup_minio_csv.mjs` first.
 *
 * Pass criterion: stdout contains "S3_KERNEL_OK row_count=3".
 */
import { createKernel, createInMemoryFileSystem } from "@secure-exec/core";
import { createPythonRuntime } from "@rivet-dev/agent-os-python";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve, dirname } from "node:path";
import { readdirSync } from "node:fs";

// ───────────────────────────────────────────────────────────────────
// MinIO env: must match .env so the SAB side worker reads the same.
// ───────────────────────────────────────────────────────────────────
process.env.BUCKET = "layerr-dev";
process.env.BUCKET_ACCESS_KEY_ID = "layerr";
process.env.BUCKET_SECRET_ACCESS_KEY = "localdev123";
process.env.BUCKET_ENDPOINT = "http://localhost:9000";
process.env.BUCKET_REGION = "us-east-1";

// ───────────────────────────────────────────────────────────────────
// Mount the vendored wheel set on the host so the kernel-runtime's
// wheelPreload can install pyodide-httpfs + dbt-duckdb closure. Same
// fixture the playground uses (agent-os-python-wheels package).
// ───────────────────────────────────────────────────────────────────
const WHEELS_DIR = "/Users/brittianwarner/goods/agent-os/registry/software/python-wheels/wheels";

// Collect every .whl filename — the kernel-runtime will iterate this
// list and pip-install one by one.
const wheels = readdirSync(WHEELS_DIR)
  .filter((f) => f.endsWith(".whl"))
  .sort();
if (wheels.length === 0) {
  console.error("FAIL: no wheels in", WHEELS_DIR);
  process.exit(1);
}

const PYODIDE_BUNDLED_DEPS = [
  "jinja2", "markupsafe", "click", "jsonschema", "jsonschema-specifications",
  "msgpack", "networkx", "packaging", "protobuf", "pydantic", "pydantic-core",
  "pyyaml", "python-dateutil", "pytz", "referencing", "requests", "rpds-py",
  "more-itertools", "typing-extensions", "urllib3", "charset-normalizer",
  "certifi", "idna", "six", "attrs", "annotated-types", "fsspec",
];

const kernel = createKernel({ filesystem: createInMemoryFileSystem() });
await kernel.mount(
  createPythonRuntime({
    wheelPreload: {
      mountPath: "/wheels",
      hostDir: WHEELS_DIR,
      wheels,
      pyodidePackages: PYODIDE_BUNDLED_DEPS,
      bootstrapScript: "",
    },
  }),
);

const code = String.raw`
import duckdb
from pyodide_httpfs import register_with_duckdb

con = duckdb.connect()
register_with_duckdb(con)

# DuckDB's read_csv_auto walks the fsspec FS for the s3:// scheme.
# Each chunk read fires a SAB-fetch that the side worker SigV4-signs
# and routes to MinIO at the BUCKET_ENDPOINT.
url = "s3://layerr-dev/test-sab/sample.csv"
rows = con.execute(f"SELECT count(*) FROM read_csv_auto('{url}')").fetchone()
print("S3_KERNEL_OK row_count=" + str(rows[0]))
`;

const chunks = [];
const errChunks = [];
const proc = kernel.spawn("python", ["-c", code], {
  onStdout: (b) => chunks.push(b),
  onStderr: (b) => errChunks.push(b),
});
const exitCode = await proc.wait();
const stdout = chunks.map((c) => new TextDecoder().decode(c)).join("");
const stderr = errChunks.map((c) => new TextDecoder().decode(c)).join("");
process.stdout.write(stdout);
if (stderr) process.stderr.write(`[stderr] ${stderr}`);
await kernel.dispose();
if (exitCode !== 0 || !stdout.includes("S3_KERNEL_OK row_count=3")) {
  console.error(`FAIL exit=${exitCode}`);
  process.exit(1);
}
console.log("\nverify_kernel_runtime_s3: PASS");
