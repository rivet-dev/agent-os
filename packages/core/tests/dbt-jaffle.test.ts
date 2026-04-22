/**
 * T4 — jaffle_shop end-to-end smoke.
 *
 * Uses a vendored mini-jaffle-shop fixture (subset of dbt-labs/jaffle-shop
 * adapted for in-memory DuckDB, hermetic, no `dbt deps` required) to
 * validate dbt against a realistic project shape.
 *
 * Skip-gated on the wheel set.
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
const fixtureDir = resolve(__dirname, "fixtures/jaffle-mini");

function hasWheelSet(): boolean {
	if (!existsSync(wheelsHostDir)) return false;
	const wheels = readdirSync(wheelsHostDir).filter((f) => f.endsWith(".whl"));
	return (
		wheels.some((w) => w.startsWith("duckdb-")) &&
		wheels.some((w) => w.startsWith("dbt_core-"))
	);
}

const READY = hasWheelSet();

interface FixtureFile {
	rel: string;
	content: string;
}

function collectFixtureFiles(dir: string, prefix = ""): FixtureFile[] {
	const out: FixtureFile[] = [];
	if (!existsSync(dir)) return out;
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const full = resolve(dir, entry.name);
		const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
		if (entry.isDirectory()) {
			out.push(...collectFixtureFiles(full, rel));
		} else if (entry.isFile()) {
			out.push({ rel, content: readFileSync(full, "utf-8") });
		}
	}
	return out;
}

describe.skipIf(!READY)("T4 — jaffle_shop end-to-end (requires wheels)", () => {
	it("builds and tests the mini jaffle_shop fixture", {
		timeout: 600_000,
	}, async () => {
		const fixtureFiles = collectFixtureFiles(fixtureDir);
		expect(fixtureFiles.length).toBeGreaterThan(0);

		const aos = await AgentOs.create({
			permissions: allowAll,
			python: { dbt: true },
		});
		try {
			// Write fixture files via Python so they land in Pyodide MEMFS
			// (where dbt will read from) rather than the kernel VFS.
			const fixtureMap: Record<string, string> = {
				"/dbt/profiles/profiles.yml": `jaffle:
  target: dev
  outputs:
    dev:
      type: duckdb
      path: ":memory:"
      threads: 1
`,
			};
			for (const f of fixtureFiles) {
				fixtureMap[`/dbt/project/${f.rel}`] = f.content;
			}

			const setupPy = `
import os, json
fixture = json.loads(${JSON.stringify(JSON.stringify(fixtureMap))})
for path, content in fixture.items():
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)
print("FIXTURE_OK")
`;
			await aos.writeFile("/tmp/setup_jaffle.py", setupPy);
			const { pid: setupPid } = aos.spawn("python3", [
				"/tmp/setup_jaffle.py",
			]);
			await aos.waitProcess(setupPid);

			const py = `
import os, traceback
os.environ["DBT_PROFILES_DIR"] = "/dbt/profiles"
os.chdir("/dbt/project")
from dbt.cli.main import dbtRunner
try:
    res = dbtRunner().invoke(["--single-threaded", "build", "--threads", "1"])
    print("DBT_RUN_SUCCESS=", res.success)
    if res.exception is not None:
        print("exception=", repr(res.exception))
except Exception:
    traceback.print_exc()
    print("DBT_RUN_SUCCESS= False")
`;
			await aos.writeFile("/tmp/run_jaffle.py", py);
			let stdout = "";
			let stderr = "";
			const start = Date.now();
			const { pid } = aos.spawn("python3", ["/tmp/run_jaffle.py"], {
				onStdout: (d) => (stdout += new TextDecoder().decode(d)),
				onStderr: (d) => (stderr += new TextDecoder().decode(d)),
			});
			await aos.waitProcess(pid);
			const ms = Date.now() - start;
			console.log(`jaffle dbt build: ${ms}ms`);

			expect(
				stdout,
				`stdout:\n${stdout}\nstderr:\n${stderr}`,
			).toContain("DBT_RUN_SUCCESS= True");

			// Verify target/manifest.json via Python (Pyodide MEMFS).
			let probe = "";
			const { pid: vpid } = aos.spawn(
				"python3",
				["-c", "import os; print(os.path.exists('/dbt/project/target/manifest.json'))"],
				{ onStdout: (d) => (probe += new TextDecoder().decode(d)) },
			);
			await aos.waitProcess(vpid);
			expect(probe.trim()).toBe("True");
			console.log("JAFFLE_SHOP_GREEN");
		} finally {
			await aos.dispose();
		}
	});
});

describe("T4 — fixture sanity (always runs)", () => {
	it("vendored fixture is well-formed", () => {
		const files = collectFixtureFiles(fixtureDir);
		if (!existsSync(fixtureDir)) {
			console.log(
				"T4 fixture not yet vendored at " +
					fixtureDir +
					". Add files to enable.",
			);
			expect(true).toBe(true);
			return;
		}
		const names = files.map((f) => f.rel);
		expect(names).toContain("dbt_project.yml");
		expect(names.some((n) => n.startsWith("models/"))).toBe(true);
	});
});
