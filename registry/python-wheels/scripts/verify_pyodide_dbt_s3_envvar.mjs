#!/usr/bin/env node
/**
 * End-to-end smoke: dbt-on-Pyodide + s3:// + env_var('BUCKET') + SigV4
 * against a real MinIO at http://localhost:9000.
 *
 * This is the closest-to-production smoke we have. It exercises the
 * exact path the playground takes:
 *
 *   1. Upload a tiny CSV to MinIO at data-sources/<sourceId>/<name>.csv
 *      (the canonical key shape finalize-upload writes to).
 *   2. Spawn a Pyodide worker w/ SAB-fetch bridge + pyodide-httpfs +
 *      dbt closure. Pass BUCKET / BUCKET_REGION / BUCKET_ACCESS_KEY_ID /
 *      BUCKET_SECRET_ACCESS_KEY through to os.environ — same shape
 *      workspace.runDbt's env injection produces.
 *   3. Build a tiny dbt project whose sources.yml uses
 *      `external_location: "s3://{{ env_var('BUCKET') }}/data-sources/
 *      <sourceId>/<name>.csv"` — exactly what the playground AGENTS.md
 *      tells Pi to write.
 *   4. Run dbt build. The flow:
 *        - dbt parse resolves env_var('BUCKET') to "layerr-dev" via os.environ
 *        - dbt-duckdb's pyodide_httpfs plugin loads and registers fsspec
 *        - read_csv_auto('s3://layerr-dev/...') walks the fsspec FS
 *        - Each chunk fetch fires the SAB+side-worker primitive
 *        - Side worker reads BUCKET_ACCESS_KEY_ID + BUCKET_SECRET_ACCESS_KEY
 *          from process.env (Node-level), SigV4-signs, GETs MinIO
 *        - DuckDB ingests bytes, materializes the staging model
 *
 * Pass: stdout contains DBT_S3_BUILD_OK with row_count > 0.
 *
 * Requires:
 *   - MinIO running at http://localhost:9000 with creds layerr/localdev123
 *   - Bucket "layerr-dev" pre-created
 *
 * Setup is bun-driven so we can use Bun.S3Client for the upload step.
 */
import { Worker } from "node:worker_threads";
import { dirname } from "node:path";
import { mkdirSync, writeFileSync, rmSync } from "node:fs";
import { S3Client } from "bun";

// ───────────────────────────────────────────────────────────────────
// MinIO env — must match Layerr playground .env defaults so the
// SAB side worker reads the same creds.
// ───────────────────────────────────────────────────────────────────
const MINIO_BUCKET = "layerr-dev";
const MINIO_ACCESS = "layerr";
const MINIO_SECRET = "localdev123";
const MINIO_ENDPOINT = "http://localhost:9000";
const MINIO_REGION = "us-east-1";

process.env.BUCKET = MINIO_BUCKET;
process.env.BUCKET_ACCESS_KEY_ID = MINIO_ACCESS;
process.env.BUCKET_SECRET_ACCESS_KEY = MINIO_SECRET;
process.env.BUCKET_ENDPOINT = MINIO_ENDPOINT;
process.env.BUCKET_REGION = MINIO_REGION;

// ───────────────────────────────────────────────────────────────────
// Step 1: upload a 3-row CSV to MinIO.
// ───────────────────────────────────────────────────────────────────
const SOURCE_ID = "test-dbt-s3-" + Date.now();
const FILE_NAME = "sample.csv";
const KEY = `data-sources/${SOURCE_ID}/${FILE_NAME}`;
const CSV_BODY = "id,name,country\n1,alice,US\n2,bob,UK\n3,charlie,US\n";

const s3 = new S3Client({
  accessKeyId: MINIO_ACCESS,
  secretAccessKey: MINIO_SECRET,
  bucket: MINIO_BUCKET,
  endpoint: MINIO_ENDPOINT,
  region: MINIO_REGION,
});
await s3.write(KEY, CSV_BODY, { type: "text/csv" });
console.log(`uploaded s3://${MINIO_BUCKET}/${KEY} (${CSV_BODY.length} bytes)`);

// ───────────────────────────────────────────────────────────────────
// Step 2: write the dbt project that references the s3:// URL via
// env_var('BUCKET'). This is byte-for-byte the shape the playground
// tells Pi to author.
// ───────────────────────────────────────────────────────────────────
const PROJECT_DIR = "/tmp/pyodide-dbt-s3-envvar";
rmSync(PROJECT_DIR, { recursive: true, force: true });
mkdirSync(`${PROJECT_DIR}/models`, { recursive: true });
writeFileSync(
  `${PROJECT_DIR}/dbt_project.yml`,
  `name: smoke
version: '1.0.0'
config-version: 2
profile: smoke
model-paths: ["models"]
target-path: target
clean-targets: [target]
`,
);
writeFileSync(
  `${PROJECT_DIR}/profiles.yml`,
  `smoke:
  target: dev
  outputs:
    dev:
      type: duckdb
      path: /dbt-project/warehouse.duckdb
      threads: 1
      plugins:
        - module: pyodide_httpfs.dbt_plugin
`,
);
writeFileSync(
  `${PROJECT_DIR}/models/sources.yml`,
  `version: 2
sources:
  - name: raw
    tables:
      - name: sample
        meta:
          external_location: "s3://{{ env_var('BUCKET') }}/data-sources/${SOURCE_ID}/${FILE_NAME}"
`,
);
writeFileSync(
  `${PROJECT_DIR}/models/stg_sample.sql`,
  `{{ config(materialized='table') }}
SELECT id, name, country FROM {{ source('raw', 'sample') }}
`,
);

// ───────────────────────────────────────────────────────────────────
// Step 3+4: spawn the Pyodide worker, install the dbt closure, run.
// ───────────────────────────────────────────────────────────────────
const PYODIDE_INDEX =
  "/Users/brittianwarner/goods/agent-os/node_modules/.pnpm/pyodide@0.29.3/node_modules/pyodide/pyodide.mjs";
const WHEELS_DIR =
  "/Users/brittianwarner/goods/agent-os/registry/software/python-wheels/wheels";

const { WORKER_SAB_FETCH_JS } = await import(
  "/Users/brittianwarner/goods/agent-os/packages/python/dist/sab-fetch-bootstrap.js"
);
const { DBT_BOOTSTRAP_SCRIPT } = await import(
  "/Users/brittianwarner/goods/agent-os/packages/python/dist/dbt-bootstrap.js"
);

const WORKER_SRC = `
const { parentPort, workerData, Worker } = require("node:worker_threads");

(async () => {
  try {
    ${WORKER_SAB_FETCH_JS}
    const sabFetch = startSabFetch();

    const { loadPyodide } = await import(workerData.pyodideMjsUrl);
    const py = await loadPyodide({
      indexURL: workerData.indexPath,
      env: workerData.pythonEnv,
      stdout: (m) => parentPort.postMessage({ type: "stdout", msg: m }),
      stderr: (m) => parentPort.postMessage({ type: "stderr", msg: m }),
    });

    registerSabFetchModule(py, sabFetch);
    py.FS.mkdirTree("/wheels");
    py.FS.mount(py.FS.filesystems.NODEFS, { root: workerData.wheelsDir }, "/wheels");
    py.FS.mkdirTree("/dbt-project");
    py.FS.mount(py.FS.filesystems.NODEFS, { root: workerData.projectDir }, "/dbt-project");

    await py.runPythonAsync(workerData.code.replace("__bootstrap_script__", workerData.dbtBootstrap));
    parentPort.postMessage({ type: "done", ok: true });
  } catch (err) {
    parentPort.postMessage({ type: "done", ok: false, error: err && err.message ? err.message : String(err), stack: err && err.stack });
  }
})();
`;

const code = `
import pyodide_js
await pyodide_js.loadPackage("micropip")
import micropip

# Pyodide-bundled deps (matches DBT_PYODIDE_BUNDLED_DEPS in agent-os).
# fsspec is required by pyodide_httpfs.
await pyodide_js.loadPackage([
    "jinja2", "markupsafe", "click", "jsonschema", "jsonschema-specifications",
    "msgpack", "networkx", "packaging", "protobuf", "pydantic", "pydantic-core",
    "pyyaml", "python-dateutil", "pytz", "referencing", "requests", "rpds-py",
    "more-itertools", "typing-extensions", "urllib3", "charset-normalizer",
    "certifi", "idna", "six", "attrs", "annotated-types",
    "fsspec",
])

import os, glob
wheels = sorted(glob.glob("/wheels/*.whl"))
urls = [f"emfs:/wheels/{os.path.basename(w)}" for w in wheels]
print(f"installing {len(urls)} wheels")
await micropip.install(urls, deps=False)

__bootstrap_script__
print("dbt bootstrap shim applied")

# Verify the BUCKET env var is visible to Python — this is what dbt's
# env_var('BUCKET') template reads via os.environ['BUCKET'].
bucket = os.environ.get("BUCKET")
print(f"os.environ['BUCKET'] = {bucket!r}")
if bucket != "${MINIO_BUCKET}":
    raise RuntimeError(
        f"env injection failed: expected BUCKET='${MINIO_BUCKET}', got {bucket!r}"
    )

import pyodide_httpfs
print(f"pyodide_httpfs OK: {pyodide_httpfs.__file__}")

os.environ["DBT_PROFILES_DIR"] = "/dbt-project"

from dbt.cli.main import dbtRunner
runner = dbtRunner()

print("\\n--- dbt parse (should resolve env_var('BUCKET')) ---")
res = runner.invoke(["parse", "--project-dir", "/dbt-project", "--profiles-dir", "/dbt-project"])
if not res.success:
    print(f"  parse FAILED: {res.exception}")
    raise SystemExit(1)
print("  parse OK")

print("\\n--- dbt build ---")
res = runner.invoke(["build", "--project-dir", "/dbt-project", "--profiles-dir", "/dbt-project"])
if not res.success:
    print(f"  build FAILED: {res.exception}")
    raise SystemExit(1)
print("  build OK")

# Materialization sanity-check via dbt show — uses dbt's already-active
# warehouse connection, no parallel-handle conflict.
print("\\n--- verify stg_sample row count ---")
res = runner.invoke([
    "show", "--inline", "SELECT COUNT(*) AS n FROM {{ ref('stg_sample') }}",
    "--project-dir", "/dbt-project", "--profiles-dir", "/dbt-project",
])
if not res.success:
    print(f"  show FAILED: {res.exception}")
    raise SystemExit(1)

warehouse_size = os.path.getsize("/dbt-project/warehouse.duckdb")
print(f"  warehouse.duckdb size: {warehouse_size} bytes")

if warehouse_size > 0:
    print("\\nDBT_S3_BUILD_OK")
else:
    print("\\nDBT_S3_BUILD_EMPTY")
    raise SystemExit(1)
`;

// Pass the BUCKET env into Pyodide's os.environ via the loadPyodide
// `env:` option — same channel workspace.runDbt's env injection
// reaches Python through.
const pythonEnv = {
  HOME: "/home/user",
  BUCKET: MINIO_BUCKET,
  BUCKET_ACCESS_KEY_ID: MINIO_ACCESS,
  BUCKET_SECRET_ACCESS_KEY: MINIO_SECRET,
  BUCKET_ENDPOINT: MINIO_ENDPOINT,
  BUCKET_REGION: MINIO_REGION,
};

const indexPath = `${dirname(PYODIDE_INDEX)}/`;
const w = new Worker(WORKER_SRC, {
  eval: true,
  workerData: {
    indexPath,
    pyodideMjsUrl: `file://${PYODIDE_INDEX}`,
    wheelsDir: WHEELS_DIR,
    projectDir: PROJECT_DIR,
    code,
    dbtBootstrap: DBT_BOOTSTRAP_SCRIPT,
    pythonEnv,
  },
});

let sawOk = false;
w.on("message", (m) => {
  if (m.type === "stdout") {
    process.stdout.write(`[py] ${m.msg}\n`);
    if (m.msg.includes("DBT_S3_BUILD_OK")) sawOk = true;
  } else if (m.type === "stderr") {
    process.stderr.write(`[py:err] ${m.msg}\n`);
  } else if (m.type === "done") {
    void w.terminate();
    if (!m.ok) {
      console.error("worker FAIL:", m.error);
      console.error(m.stack);
      process.exit(1);
    }
    if (!sawOk) {
      console.error("expected DBT_S3_BUILD_OK marker; not seen");
      process.exit(1);
    }
    console.log("\nverify_pyodide_dbt_s3_envvar: PASS");
    process.exit(0);
  }
});
w.on("error", (e) => {
  console.error(e);
  process.exit(1);
});
