#!/usr/bin/env node
/**
 * Smoke test for the SAB-fetch wiring inside `kernel-runtime.ts` — the path
 * agent-os actually uses for `python: { dbt: true }` Pi VMs in production.
 *
 * The previous smoke test (`verify_pyodide_httpfs_dbt.mjs`) built its own
 * Worker source string with SAB inlined explicitly. That proved the parts
 * compose, but said nothing about whether `createPythonRuntime` (used by
 * Pi VMs) registers `_pyodide_httpfs_host`.
 *
 * This test:
 *   1. Mounts `createPythonRuntime` into a kernel.
 *   2. Runs Python that imports `_pyodide_httpfs_host` and calls .fetch().
 *   3. Asserts the response status is 2xx and a body is returned.
 *
 * Pass criterion: stdout contains "SAB_KERNEL_OK".
 */
import { createKernel, createInMemoryFileSystem } from "@secure-exec/core";
import { createPythonRuntime } from "@rivet-dev/agent-os-python";

const kernel = createKernel({ filesystem: createInMemoryFileSystem() });
await kernel.mount(createPythonRuntime());

const code = String.raw`
import _pyodide_httpfs_host as _host
import json

# Minimal HEAD probe against a public, range-friendly URL. Avoids any
# auth path so we test the bare fetch wiring; SigV4 is exercised by the
# end-to-end smoke test against MinIO.
init = {"method": "GET", "headers": {"range": "bytes=0-1023"}}
res = _host.fetch("https://github.com/duckdb/duckdb/raw/main/data/parquet-testing/userdata1.parquet", json.dumps(init))

# fetch returns a JsProxy. Cross JS-null/Python-None boundary safely:
# only str() the error if it's truthy under JS semantics.
status = int(res.status)
err_raw = res.error
err = str(err_raw) if err_raw else None
body_len = len(res.body) if res.body is not None else 0

print(f"status={status} body_len={body_len} err={err}")
if status >= 200 and status < 400 and err is None and body_len > 0:
    print("SAB_KERNEL_OK")
else:
    raise SystemExit(1)
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
if (exitCode !== 0 || !stdout.includes("SAB_KERNEL_OK")) {
  console.error(`FAIL exit=${exitCode}`);
  process.exit(1);
}
console.log("\nverify_kernel_runtime_sab: PASS");
