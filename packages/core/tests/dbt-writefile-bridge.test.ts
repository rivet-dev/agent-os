/**
 * Regression: `python.dbt: true` must bridge `aos.writeFile` writes at the
 * canonical dbt paths (/root/dbt-projects, /root/.dbt) through to dbt
 * running inside Pyodide.
 *
 * The stack has two filesystems for these paths: the kernel VFS (which
 * aos.writeFile hits) and Pyodide MEMFS (which dbt's `open()` hits). The
 * dbt opt-in auto-creates host scratch dirs, mounts them as host-dir
 * backends in the kernel VFS, and also NODEFS-mounts them into Pyodide —
 * so a single physical file is visible through both pathways.
 *
 * This test exists because the earlier smoke tests bypass aos.writeFile
 * entirely (they write fixture content via inline Python), which wouldn't
 * catch a regression in the NODEFS bridge.
 */
import { existsSync, readdirSync, readFileSync } from "node:fs";
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
const fixtureDir = resolve(__dirname, "fixtures/dbt-trivial");

function hasWheelSet(): boolean {
	if (!existsSync(wheelsHostDir)) return false;
	const wheels = readdirSync(wheelsHostDir).filter((f) => f.endsWith(".whl"));
	return (
		wheels.some((w) => w.startsWith("duckdb-")) &&
		wheels.some((w) => w.startsWith("dbt_core-"))
	);
}

const READY = hasWheelSet();

const PROJECT_YML = readFileSync(`${fixtureDir}/dbt_project.yml`, "utf-8");
const PROFILES_YML = readFileSync(`${fixtureDir}/profiles.yml`, "utf-8");
const MODEL_SQL = readFileSync(`${fixtureDir}/models/example.sql`, "utf-8");

describe.skipIf(!READY)(
	"dbt — aos.writeFile -> dbt reads it (NODEFS bridge)",
	() => {
		it(
			"dbt run succeeds when the project is staged via aos.writeFile at the canonical paths",
			{ timeout: 180_000 },
			async () => {
				const aos = await AgentOs.create({
					permissions: allowAll,
					python: { dbt: true },
				});
				try {
					// Stage project + profile at the auto-mounted canonical paths.
					// No inline Python os.makedirs — this is exactly the flow the
					// quickstart promises.
					await aos.writeFiles([
						{
							path: "/root/dbt-projects/agent_os_demo/dbt_project.yml",
							content: PROJECT_YML,
						},
						{
							path: "/root/dbt-projects/agent_os_demo/models/example.sql",
							content: MODEL_SQL,
						},
						{ path: "/root/.dbt/profiles.yml", content: PROFILES_YML },
					]);

					// Confirm the NODEFS bridge is live: Python opens the file at
					// the canonical VM path and sees the exact bytes we wrote.
					const probe = `
import os
for path in ("/root/.dbt/profiles.yml", "/root/dbt-projects/agent_os_demo/dbt_project.yml"):
    with open(path) as f:
        print(f"READ_OK {path} len={len(f.read())}", flush=True)
`;
					await aos.writeFile("/tmp/probe.py", probe);
					let probeStdout = "";
					let probeStderr = "";
					const { pid: probePid } = aos.spawn("python3", ["/tmp/probe.py"], {
						onStdout: (d) => (probeStdout += new TextDecoder().decode(d)),
						onStderr: (d) => (probeStderr += new TextDecoder().decode(d)),
					});
					const probeExit = await aos.waitProcess(probePid);
					expect(
						probeExit,
						`probe stdout=${probeStdout}\nprobe stderr=${probeStderr}`,
					).toBe(0);
					expect(probeStdout).toContain("READ_OK /root/.dbt/profiles.yml");
					expect(probeStdout).toContain(
						"READ_OK /root/dbt-projects/agent_os_demo/dbt_project.yml",
					);

					// Now run dbt against the staged project.
					const run = `
import os, traceback
os.chdir("/root/dbt-projects/agent_os_demo")
from dbt.cli.main import dbtRunner
try:
    res = dbtRunner().invoke(["--single-threaded", "run", "--threads", "1"])
    print("DBT_RUN_SUCCESS=", res.success, flush=True)
    if res.exception is not None:
        print("DBT_RUN_EXCEPTION=", repr(res.exception), flush=True)
except Exception:
    traceback.print_exc()
    print("DBT_RUN_SUCCESS= False", flush=True)
`;
					await aos.writeFile("/tmp/run_dbt.py", run);
					let stdout = "";
					let stderr = "";
					const { pid } = aos.spawn("python3", ["/tmp/run_dbt.py"], {
						onStdout: (d) => (stdout += new TextDecoder().decode(d)),
						onStderr: (d) => (stderr += new TextDecoder().decode(d)),
					});
					await aos.waitProcess(pid);
					expect(
						stdout,
						`--- stdout ---\n${stdout}\n--- stderr ---\n${stderr}`,
					).toContain("DBT_RUN_SUCCESS= True");
				} finally {
					await aos.dispose();
				}
			},
		);
	},
);

describe("dbt — mount collision surfaces fast", () => {
	it("errors if a user mount collides with the dbt profilesDir", async () => {
		// We don't need the wheel set for this — the check happens before
		// any Pyodide work, so the error surfaces on any host.
		const { createHostDirBackend } = await import(
			"../src/backends/host-dir-backend.js"
		);
		const { mkdtempSync } = await import("node:fs");
		const { tmpdir } = await import("node:os");
		const { join } = await import("node:path");
		const conflicting = mkdtempSync(join(tmpdir(), "dbt-collision-test-"));

		await expect(
			AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
				mounts: [
					{
						path: "/root/.dbt",
						driver: createHostDirBackend({ hostPath: conflicting }),
					},
				],
			}),
		).rejects.toThrow(/collides with a user-declared mount/);
	});
});
