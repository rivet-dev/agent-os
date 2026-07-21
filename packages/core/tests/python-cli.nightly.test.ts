// Nightly: projects the complete registry command bundle.
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";
import { REGISTRY_SOFTWARE } from "./helpers/registry-commands.js";

// End-to-end coverage for the Pyodide-powered `python` / `python3` CLI exposed
// by agentos, driven through the public AgentOs API. `python` resolves as a
// runtime command (like `node`): `vm.exec()` / `vm.execArgv()` / `spawn()` route
// it directly to the embedded Pyodide runtime, and a `/bin/python` stub also
// makes it resolvable on the guest shell `PATH`. The Python runtime bridges the
// whole VM filesystem into Pyodide, so scripts and file I/O work anywhere on the
// VM (these tests use `/tmp`).
describe("python CLI (Pyodide runtime)", () => {
	let vm: AgentOs;

	beforeAll(async () => {
		vm = await AgentOs.create();
	}, 120_000);

	afterAll(async () => {
		await vm?.dispose();
	});

	test(
		"python -c runs inline code",
		async () => {
			const result = await vm.execArgv("python", ["-c", "print(1 + 1)"]);
			expect(result.exitCode, result.stderr).toBe(0);
			expect(result.stdout.trim()).toBe("2");
		},
		120_000,
	);

	test(
		"python via vm.exec (agent command path) runs inline code",
		async () => {
			const result = await vm.exec('python -c "print(2 + 3)"');
			expect(result.exitCode, result.stderr).toBe(0);
			expect(result.stdout.trim()).toBe("5");
		},
		120_000,
	);

	test(
		"python runs a script file anywhere on the VM filesystem with sys.argv",
		async () => {
			// /tmp is bridged into Python via the whole-root mount (not just /workspace).
			await vm.writeFile(
				"/tmp/argv.py",
				"import sys\nprint(','.join(sys.argv))\n",
			);
			const result = await vm.execArgv("python", [
				"/tmp/argv.py",
				"alpha",
				"beta",
			]);
			expect(result.exitCode, result.stderr).toBe(0);
			expect(result.stdout.trim()).toBe("/tmp/argv.py,alpha,beta");
		},
		120_000,
	);

	test(
		"python writes to the VM filesystem, visible to the host (cross-runtime)",
		async () => {
			const write = await vm.execArgv("python", [
				"-c",
				"open('/tmp/from-python.txt','w').write('written-by-python')",
			]);
			expect(write.exitCode, write.stderr).toBe(0);
			// The write landed in the kernel VFS, so the host sees it too.
			const contents = await vm.readFile("/tmp/from-python.txt");
			expect(Buffer.from(contents).toString("utf8")).toBe("written-by-python");
		},
		120_000,
	);

	test(
		"python -m runs a module",
		async () => {
			const result = await vm.execArgv("python", ["-m", "this"]);
			expect(result.exitCode, result.stderr).toBe(0);
			expect(result.stdout).toContain("Beautiful is better than ugly");
		},
		120_000,
	);

	test(
		"python3 alias runs inline code",
		async () => {
			const result = await vm.execArgv("python3", ["-c", "print(6 * 7)"]);
			expect(result.exitCode, result.stderr).toBe(0);
			expect(result.stdout.trim()).toBe("42");
		},
		120_000,
	);

	test(
		"python - reads the program from stdin",
		async () => {
			const chunks: string[] = [];
			const errors: string[] = [];
			const { pid } = vm.spawn("python", ["-"], {
				onStdout: (data) => chunks.push(Buffer.from(data).toString("utf8")),
				onStderr: (data) => errors.push(Buffer.from(data).toString("utf8")),
			});
			await vm.writeProcessStdin(pid, "print('from stdin program')\n");
			await vm.closeProcessStdin(pid);
			const exitCode = await vm.waitProcess(pid);
			// Native-sidecar process_output can lag the exit notification by a turn.
			await new Promise((resolve) => setTimeout(resolve, 0));
			expect(exitCode, errors.join("")).toBe(0);
			expect(chunks.join("")).toContain("from stdin program");
		},
		120_000,
	);

	// The guest-shell path needs the WASM `sh` from the registry. `python` resolves
	// on the shell PATH via its `/bin/python` stub, so `sh -c`/pipelines work.
	test(
		"python runs through the guest shell and pipelines",
		async () => {
			const shellVm = await AgentOs.create({ software: REGISTRY_SOFTWARE });
			try {
				const direct = await shellVm.exec('sh -c "python -c \'print(2 + 3)\'"');
				expect(direct.exitCode, direct.stderr).toBe(0);
				expect(direct.stdout.trim()).toBe("5");

				const piped = await shellVm.exec('echo "print(6 * 7)" | python -');
				expect(piped.exitCode, piped.stderr).toBe(0);
				expect(piped.stdout.trim()).toBe("42");
			} finally {
				await shellVm.dispose();
			}
		},
		120_000,
	);
});
