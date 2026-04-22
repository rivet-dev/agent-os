/**
 * Diagnostic test that isolates the AgentOs.create + python.dbt boot path
 * from any subsequent Python execution. Helps localize failures when the
 * full smoke test fails silently.
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

describe.skipIf(!READY)("AgentOs init with python.dbt", () => {
	it(
		"boots without throwing and runs trivial python -c",
		{ timeout: 180_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				let stdout = "";
				let stderr = "";
				const probe = `
import sys, traceback
print("python:", sys.version_info, flush=True)
print("--- duckdb cursor lifecycle hypothesis ---", flush=True)
import duckdb
print(f"  duckdb version: {duckdb.__version__}", flush=True)
con = duckdb.connect(":memory:")
print("  con ok", flush=True)
# Create cursor, use it, close it. Does the parent conn still work?
cur1 = con.cursor()
cur1.execute("SELECT 1").fetchall()
print("  cur1 query ok", flush=True)
cur1.close()
print("  cur1 closed", flush=True)
try:
    con.execute("SELECT 2").fetchall()
    print("  con.execute after cur1.close() = OK (cursor close DOES NOT cascade)", flush=True)
except Exception as e:
    print(f"  con.execute after cur1.close() = FAIL: {e!r} (cursor close DOES cascade)", flush=True)
# Try multiple cursors from the same conn
try:
    cur2 = con.cursor()
    cur2.execute("BEGIN")
    cur2.execute("CREATE TABLE t (x INT)")
    cur2.execute("COMMIT")
    cur2.close()
    cur3 = con.cursor()
    cur3.execute("BEGIN")
    cur3.execute("INSERT INTO t VALUES (42)")
    cur3.execute("COMMIT")
    rows = cur3.execute("SELECT * FROM t").fetchall()
    print(f"  multi-cursor BEGIN/COMMIT works, rows={rows}", flush=True)
    cur3.close()
except Exception as e:
    print(f"  multi-cursor failed: {e!r}", flush=True)
con.close()
print("  done", flush=True)
`;
				await aos.writeFile("/tmp/probe.py", probe);
				const { pid } = aos.spawn("python3", ["/tmp/probe.py"], {
					onStdout: (d) => (stdout += new TextDecoder().decode(d)),
					onStderr: (d) => (stderr += new TextDecoder().decode(d)),
				});
				const exit = await aos.waitProcess(pid);
				console.error("--- stdout ---");
				console.error(stdout);
				console.error("--- stderr ---");
				console.error(stderr);
				expect(exit, `stdout=${stdout} stderr=${stderr}`).toBe(0);
				expect(stdout).toContain("python:");
			} finally {
				await aos.dispose();
			}
		},
	);
});
