import {
	chmodSync,
	mkdirSync,
	mkdtempSync,
	rmSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

/**
 * End-to-end Phase-1 proof: a package is projected into the single `/opt/agentos`
 * tree and its `bin/` command resolves through a real `$PATH` walk + header
 * dispatch.
 *
 * The package is hand-built (no npm) so the test is deterministic; transition
 * package dirs carry `agentos-package.json` (name + version) + `bin/<cmd>`.
 * The sidecar projects `/opt/agentos/pkgs/<name>/<version>` +
 * `pkgs/<name>/current` + `bin/<cmd>` leaf mounts.
 */
describe("agentos package projection (VM)", () => {
	let vm: AgentOs;
	let root: string;

	beforeAll(async () => {
		root = mkdtempSync(join(tmpdir(), "agentos-pkg-vm-"));
		// Build a transition package dir. Packed package fixtures use `.aospkg`;
		// raw `package.tar` dirs are no longer a sidecar input shape.
		const pkgDir = join(root, "pkg");
		mkdirSync(join(pkgDir, "bin"), { recursive: true });
		writeFileSync(
			join(pkgDir, "agentos-package.json"),
			JSON.stringify({ name: "hello-cmd", version: "1.0.0" }, null, 2),
		);
		writeFileSync(
			join(pkgDir, "package.json"),
			JSON.stringify({ name: "hello-cmd", version: "1.0.0", type: "module" }),
		);
		const commandDir = join(pkgDir, "node_modules", "hello-cmd");
		mkdirSync(commandDir, { recursive: true });
		const commandPath = join(commandDir, "index.js");
		writeFileSync(
			commandPath,
			"#!/usr/bin/env node\nimport { realpathSync } from 'fs';\nimport { cwd as importedCwd } from 'process';\nprocess.stdout.write(`hello from agentos package\\ncwd=${process.cwd()}\\nimportedCwd=${importedCwd()}\\nrealCwd=${realpathSync(importedCwd())}\\n`);\n",
		);
		// Commands must be executable (Linux x-bit) — a non-executable PATH match
		// is skipped (ENOENT) and a direct non-executable path is denied (EACCES).
		chmodSync(commandPath, 0o755);
		const binPath = join(pkgDir, "bin", "hello-cmd");
		symlinkSync("../node_modules/hello-cmd/index.js", binPath);
		const parentBinPath = join(pkgDir, "bin", "hello-parent");
		writeFileSync(
			parentBinPath,
			"#!/usr/bin/env node\nimport { spawnSync } from 'child_process';\nconst child = spawnSync('/opt/agentos/bin/hello-cmd', [], { cwd: '/workspace', encoding: 'utf8' });\nprocess.stdout.write(JSON.stringify({ status: child.status, stdout: child.stdout, stderr: child.stderr }));\n",
		);
		chmodSync(parentBinPath, 0o755);
		const asyncParentBinPath = join(pkgDir, "bin", "hello-parent-async");
		writeFileSync(
			asyncParentBinPath,
			"#!/usr/bin/env node\nimport { spawn } from 'child_process';\nconst child = spawn('/opt/agentos/bin/hello-cmd', [], { cwd: '/workspace', stdio: 'pipe' });\nlet stdout = '';\nlet stderr = '';\nchild.stdout.on('data', chunk => { stdout += chunk; });\nchild.stderr.on('data', chunk => { stderr += chunk; });\nchild.on('error', error => { throw error; });\nchild.on('close', status => { process.stdout.write(JSON.stringify({ status, stdout, stderr })); });\n",
		);
		chmodSync(asyncParentBinPath, 0o755);
		const nodeParentBinPath = join(pkgDir, "bin", "hello-parent-node");
		writeFileSync(
			nodeParentBinPath,
			"#!/usr/bin/env node\nimport { spawn } from 'child_process';\nconst child = spawn(process.execPath, ['/opt/agentos/bin/hello-cmd'], { cwd: '/workspace', stdio: 'pipe' });\nlet stdout = '';\nlet stderr = '';\nchild.stdout.on('data', chunk => { stdout += chunk; });\nchild.stderr.on('data', chunk => { stderr += chunk; });\nchild.on('error', error => { throw error; });\nchild.on('close', status => { process.stdout.write(JSON.stringify({ execPath: process.execPath, status, stdout, stderr })); });\n",
		);
		chmodSync(nodeParentBinPath, 0o755);
		const packageNodeParentBinPath = join(
			pkgDir,
			"bin",
			"hello-parent-node-package-path",
		);
		writeFileSync(
			packageNodeParentBinPath,
			"#!/usr/bin/env node\nimport { spawn } from 'child_process';\nconst child = spawn(process.execPath, ['/opt/agentos/pkgs/hello-cmd/1.0.0/node_modules/hello-cmd/index.js'], { cwd: '/workspace', stdio: 'pipe' });\nlet stdout = '';\nlet stderr = '';\nchild.stdout.on('data', chunk => { stdout += chunk; });\nchild.stderr.on('data', chunk => { stderr += chunk; });\nchild.on('error', error => { throw error; });\nchild.on('close', status => { process.stdout.write(JSON.stringify({ status, stdout, stderr })); });\n",
		);
		chmodSync(packageNodeParentBinPath, 0o755);
		const workspaceDir = join(root, "workspace");
		mkdirSync(workspaceDir, { recursive: true });
		vm = await AgentOs.create({
			defaultSoftware: false,
			software: [pkgDir],
			mounts: [
				{
					path: "/workspace",
					plugin: {
						id: "host_dir",
						config: { hostPath: workspaceDir, readOnly: false },
					},
					readOnly: false,
				},
			],
		});
	}, 60_000);

	afterAll(async () => {
		await vm?.dispose();
		if (root) rmSync(root, { recursive: true, force: true });
	});

	test("projects the package tree under /opt/agentos", async () => {
		expect(
			await vm.exists("/opt/agentos/pkgs/hello-cmd/1.0.0/bin/hello-cmd"),
		).toBe(true);
		expect(await vm.exists("/opt/agentos/bin/hello-cmd")).toBe(true);
	});

	// `exec` would route through `sh -c`; spawn the command directly so the test
	// isolates the package's $PATH resolution + header dispatch (no shell needed).
	async function runCommand(
		command: string,
		cwd?: string,
	): Promise<{ code: number; out: string; err: string }> {
		let out = "";
		let err = "";
		const { pid } = vm.spawn(command, [], {
			cwd,
			onStdout: (data) => {
				out += new TextDecoder().decode(data);
			},
			onStderr: (data) => {
				err += new TextDecoder().decode(data);
			},
		});
		const code = await vm.waitProcess(pid);
		// Native-sidecar process_output events can arrive a few turns after the
		// exit notification; poll briefly until output lands (tiny stdout is the
		// first thing to get lost if snapshotted immediately).
		for (let i = 0; i < 20 && out === "" && err === ""; i++) {
			await new Promise((resolve) => setTimeout(resolve, 25));
		}
		return { code, out, err };
	}

	test("resolves the command via $PATH and dispatches by header", async () => {
		const { code, out, err } = await runCommand("hello-cmd");
		expect(err, `stderr: ${err}`).not.toContain("Error");
		expect(out).toContain("hello from agentos package");
		expect(code).toBe(0);
	});

	test("resolves the command by absolute path too", async () => {
		const { code, out, err } = await runCommand("/opt/agentos/bin/hello-cmd");
		expect(err, `stderr: ${err}`).not.toContain("Error");
		expect(out).toContain("hello from agentos package");
		expect(code).toBe(0);
	});

	test("starts JavaScript package commands in the requested cwd", async () => {
		const { code, out, err } = await runCommand("hello-cmd", "/workspace");
		expect({ code, err }).toEqual({ code: 0, err: "" });
		expect(out).toContain("cwd=/workspace");
		expect(out).toContain("importedCwd=/workspace");
		expect(out).toContain("realCwd=/workspace");
	});

	test("starts nested JavaScript package commands in the requested cwd", async () => {
		const { code, out, err } = await runCommand("hello-parent");
		expect({ code, err }).toEqual({ code: 0, err: "" });
		expect(JSON.parse(out)).toMatchObject({
			status: 0,
			stderr: "",
			stdout: expect.stringContaining("cwd=/workspace"),
		});
	});

	test("starts asynchronously nested JavaScript package commands in the requested cwd", async () => {
		const { code, out, err } = await runCommand("hello-parent-async");
		expect({ code, err }).toEqual({ code: 0, err: "" });
		expect(JSON.parse(out)).toMatchObject({
			status: 0,
			stderr: "",
			stdout: expect.stringContaining("cwd=/workspace"),
		});
	});

	test("starts Node CLI entrypoints in the requested cwd", async () => {
		const { code, out, err } = await runCommand("hello-parent-node");
		expect({ code, err }).toEqual({ code: 0, err: "" });
		expect(JSON.parse(out)).toMatchObject({
			execPath: "/usr/bin/node",
			status: 0,
			stderr: "",
			stdout: expect.stringContaining("cwd=/workspace"),
		});
	});

	test("starts package-path Node CLI entrypoints in the requested cwd", async () => {
		const { code, out, err } = await runCommand(
			"hello-parent-node-package-path",
		);
		expect({ code, err }).toEqual({ code: 0, err: "" });
		expect(JSON.parse(out)).toMatchObject({
			status: 0,
			stderr: "",
			stdout: expect.stringContaining("cwd=/workspace"),
		});
	});

	test("runs repeatedly (no shebang corruption across executions)", async () => {
		// Re-executing a shebang command must not corrupt its on-disk source: the
		// JS import-cache write previously clobbered the read-only mount, stripping
		// the `#!` so the 2nd exec failed with ENOEXEC.
		for (let i = 0; i < 3; i++) {
			const { code, out, err } = await runCommand("hello-cmd");
			expect(err, `iteration ${i} stderr: ${err}`).not.toContain("Error");
			expect(out, `iteration ${i}`).toContain("hello from agentos package");
			expect(code).toBe(0);
		}
	});
});
