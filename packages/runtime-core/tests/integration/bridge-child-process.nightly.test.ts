/**
 * Integration tests: Node bridge child_process routing through kernel.
 *
 * Verifies that child_process.spawn/execSync/spawnSync calls from Node
 * isolate code route through the kernel's command registry to the
 * appropriate runtime driver (WasmVM for shell commands).
 *
 * Gracefully skipped when the WASM binary is not built.
 */

import {
	chmodSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	rmSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { IntegrationKernelResult } from "@rivet-dev/agentos-vm-test-harness";
import {
	COMMANDS_DIR,
	createIntegrationKernel,
	createKernel,
	createNodeRuntime,
	createWasmVmRuntime,
	describeIf,
	NodeFileSystem,
} from "@rivet-dev/agentos-vm-test-harness";
import { afterEach, describe, expect, it, vi } from "vitest";

// Each case boots a debug V8 sidecar and one or more WASM children. Five
// seconds is below normal completion time on a contended self-hosted runner;
// operation-level deadlines still catch actual bridge hangs.
vi.setConfig({ testTimeout: 15_000 });

const __dirname = dirname(fileURLToPath(import.meta.url));
const PACKAGED_COREUTILS_COMMANDS_DIR = resolve(
	__dirname,
	"../../software/coreutils/wasm",
);
const BRIDGE_COMMAND_DIRS = [
	COMMANDS_DIR,
	PACKAGED_COREUTILS_COMMANDS_DIR,
].filter((commandDir, index, allDirs) => {
	return (
		existsSync(join(commandDir, "sh")) && allDirs.indexOf(commandDir) === index
	);
});
const skipReason =
	BRIDGE_COMMAND_DIRS.length === 0
		? `WASM shell command not found at ${COMMANDS_DIR} or ${PACKAGED_COREUTILS_COMMANDS_DIR}`
		: false;

function createBridgeIntegrationKernel(): Promise<IntegrationKernelResult> {
	return createIntegrationKernel({
		runtimes: ["wasmvm", "node"],
		commandDirs: BRIDGE_COMMAND_DIRS,
	});
}

describeIf(
	!skipReason,
	"bridge child_process → kernel routing",
	{ timeout: 60_000 },
	() => {
		let ctx: IntegrationKernelResult;
		const cleanupPaths: string[] = [];

		afterEach(async () => {
			if (ctx) await ctx.dispose();
			for (const cleanupPath of cleanupPaths.splice(0)) {
				rmSync(cleanupPath, { recursive: true, force: true });
			}
		});

		it('execSync("echo hello") routes through kernel to WasmVM shell', async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execSync } = require('child_process');
      const result = execSync('echo hello', { encoding: 'utf-8' });
      console.log(result.trim());
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
				},
			);

			const code = await proc.wait();
			expect(code).toBe(0);

			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			expect(output).toContain("hello");
		});

		it('child_process.spawn("ls") resolves to WasmVM runtime', async () => {
			ctx = await createBridgeIntegrationKernel();
			await ctx.vfs.writeFile("/tmp/test-file.txt", "content");

			const chunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execSync } = require('child_process');
      const result = execSync('ls /tmp', { encoding: 'utf-8' });
      console.log(result.trim());
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
				},
			);

			const code = await proc.wait();
			expect(code).toBe(0);

			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			expect(output).toContain("test-file.txt");
		});

		it("spawned processes get proper PIDs from kernel process table", async () => {
			ctx = await createBridgeIntegrationKernel();

			// The Node process itself gets a PID from the kernel
			const proc = ctx.kernel.spawn("node", ["-e", 'console.log("pid-test")']);
			expect(proc.pid).toBeGreaterThan(0);

			await proc.wait();
		});

		it("stdout from spawned child processes pipes back to Node caller", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execSync } = require('child_process');
      const result = execSync('echo "piped-output"', { encoding: 'utf-8' });
      console.log('received:', result.trim());
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
				},
			);

			const code = await proc.wait();
			expect(code).toBe(0);

			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			expect(output).toContain("received: piped-output");
		});

		it('async child_process.spawn("sh") can stream output and exit cleanly', async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { spawn } = require('child_process');
      const child = spawn('sh', ['-lc', 'echo async-ok'], {
        stdio: ['ignore', 'pipe', 'inherit'],
      });
      child.stdout.on('data', (chunk) => process.stdout.write(chunk));
      child.on('close', (code) => process.exit(code ?? 0));
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
				},
			);

			const code = await proc.wait();
			expect(code).toBe(0);

			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			expect(output).toContain("async-ok");
		});

		it("child_process.spawn with shell:true preserves shell builtin exit codes", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { spawn } = require('child_process');
      const child = spawn('exit 7', { shell: true });
      child.on('close', (code) => {
        console.log('close:' + code);
        process.exit(code ?? 0);
      });
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
				},
			);

			const code = await proc.wait();
			expect(code).toBe(7);

			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			expect(output).toContain("close:7");
		});

		it("stderr from spawned child processes pipes back to Node caller", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execSync } = require('child_process');
      try {
        execSync('cat /nonexistent/path', { encoding: 'utf-8' });
      } catch (e) {
        console.log('caught-error');
      }
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
				},
			);

			const code = await proc.wait();
			expect(code).toBe(0);

			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			expect(output).toContain("caught-error");
		});

		it("commands not in the registry return ENOENT-like error", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execSync } = require('child_process');
      try {
        execSync('nonexistent-cmd-xyz', { encoding: 'utf-8' });
        console.log('SHOULD_NOT_REACH');
      } catch (e) {
        console.log('error-caught');
      }
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			// execSync wraps the command in bash -c, so the shell handles unknown commands
			// Either the shell returns non-zero (caught by execSync) or ENOENT propagates
			expect(output).not.toContain("SHOULD_NOT_REACH");
			expect(output).toContain("error-caught");
		});

		it("execSync with env passes environment through kernel", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execSync } = require('child_process');
      const result = execSync('echo $TEST_VAR', {
        encoding: 'utf-8',
        env: { TEST_VAR: 'kernel-env-test' },
      });
      console.log(result.trim());
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
				},
			);

			const code = await proc.wait();
			expect(code).toBe(0);

			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			expect(output).toContain("kernel-env-test");
		});

		it("cat reads VFS file through kernel child_process", async () => {
			ctx = await createBridgeIntegrationKernel();
			await ctx.vfs.writeFile("/tmp/bridge-test.txt", "hello from vfs");

			const chunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execSync } = require('child_process');
      const result = execSync('cat /tmp/bridge-test.txt', { encoding: 'utf-8' });
      console.log(result.trim());
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
				},
			);

			const code = await proc.wait();
			expect(code).toBe(0);

			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			expect(output).toContain("hello from vfs");
		});

		it("execSync shell redirection writes command stdout into the kernel VFS", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execSync } = require('child_process');
      execSync("printf 'bash-ok' > bash-output.txt", { encoding: 'utf-8' });
      console.log(execSync('cat /tmp/bash-output.txt', { encoding: 'utf-8' }));
    `,
				],
				{
					cwd: "/tmp",
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(code, `stdout:\n${output}\nstderr:\n${stderr}`).toBe(0);
			expect(output).toContain("bash-ok");
			expect(
				new TextDecoder().decode(
					await ctx.vfs.readFile("/tmp/bash-output.txt"),
				),
			).toBe("bash-ok");
		});

		it("execSync multi-statement shell syntax runs through the guest shell", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const fs = require('fs');
      const { execSync } = require('child_process');
      execSync("echo ignored; echo fallback-ok > fallback-output.txt", { encoding: 'utf-8' });
      console.log(fs.readFileSync('/tmp/fallback-output.txt', 'utf8'));
    `,
				],
				{
					cwd: "/tmp",
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(code, `stdout:\n${output}\nstderr:\n${stderr}`).toBe(0);
			expect(output).toContain("fallback-ok");
			expect(
				new TextDecoder().decode(
					await ctx.vfs.readFile("/tmp/fallback-output.txt"),
				),
			).toBe("fallback-ok\n");
		});

		it("execSync append redirection onto a write-only file succeeds like Linux", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const fs = require('fs');
      const { execSync } = require('child_process');
      fs.writeFileSync('/tmp/write-only.txt', 'original');
      fs.chmodSync('/tmp/write-only.txt', 0o200);
      // A real shell opens the append target write-only, so a 0o200 file is
      // appendable even though guest-side readback remains unavailable.
      try {
        execSync('printf changed >> /tmp/write-only.txt');
      } catch (error) {
        console.error(JSON.stringify({
          message: error instanceof Error ? error.message : String(error),
          status: error && typeof error === 'object' && 'status' in error ? error.status : null,
          stdout: error && typeof error === 'object' && 'stdout' in error ? String(error.stdout ?? '') : '',
          stderr: error && typeof error === 'object' && 'stderr' in error ? String(error.stderr ?? '') : ''
        }));
        process.exit(99);
      }
      console.log(JSON.stringify({
        mode: 'appended'
      }));
    `,
				],
				{
					cwd: "/tmp",
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(code, `stdout:\n${output}\nstderr:\n${stderr}`).toBe(0);
			const result = JSON.parse(output.trim());
			expect(result.mode).toBe("appended");
			expect(
				new TextDecoder().decode(await ctx.vfs.readFile("/tmp/write-only.txt")),
			).toBe("originalchanged");
		}, 15_000);

		it("execSync append redirection appends and creates missing files", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execSync } = require('child_process');
      execSync("printf a > append-base.txt");
      execSync("printf b >> append-base.txt");
      execSync("printf c >> append-fresh.txt");
      console.log('append-done');
    `,
				],
				{
					cwd: "/tmp",
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(code, `stdout:\n${output}\nstderr:\n${stderr}`).toBe(0);
			expect(
				new TextDecoder().decode(
					await ctx.vfs.readFile("/tmp/append-base.txt"),
				),
			).toBe("ab");
			expect(
				new TextDecoder().decode(
					await ctx.vfs.readFile("/tmp/append-fresh.txt"),
				),
			).toBe("c");
		});

		it("execSync stdin redirection feeds the kernel VFS file to the command", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const fs = require('fs');
      const { execSync } = require('child_process');
      fs.writeFileSync('/tmp/stdin-input.txt', 'stdin-redirect-content');
      const result = execSync('cat < stdin-input.txt', { encoding: 'utf-8' });
      console.log('read:' + result);
    `,
				],
				{
					cwd: "/tmp",
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(code, `stdout:\n${output}\nstderr:\n${stderr}`).toBe(0);
			expect(output).toContain("read:stdin-redirect-content");
		});

		it("execSync redirection handles quoted target paths with spaces", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execSync } = require('child_process');
      execSync("printf hi > 'out file.txt'");
      execSync('printf hi > "out file2.txt"');
      console.log('quoted-done');
    `,
				],
				{
					cwd: "/tmp",
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(code, `stdout:\n${output}\nstderr:\n${stderr}`).toBe(0);
			expect(
				new TextDecoder().decode(await ctx.vfs.readFile("/tmp/out file.txt")),
			).toBe("hi");
			expect(
				new TextDecoder().decode(await ctx.vfs.readFile("/tmp/out file2.txt")),
			).toBe("hi");
		});

		it("execSync surfaces shell failure exit codes and truncates redirect targets", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const fs = require('fs');
      const { execSync } = require('child_process');
      let redirectFailure = null;
      try {
        execSync('cat /missing-input-file > fail-out.txt', { encoding: 'utf-8' });
      } catch (error) {
        redirectFailure = {
          status: error.status ?? null,
          stderr: String(error.stderr ?? ''),
        };
      }
      let exitFailure = null;
      try {
        execSync('exit 7', { encoding: 'utf-8' });
      } catch (error) {
        exitFailure = { status: error.status ?? null };
      }
      console.log(JSON.stringify({
        redirectFailure,
        exitFailure,
        redirectTarget: fs.readFileSync('/tmp/fail-out.txt', 'utf8'),
      }));
    `,
				],
				{
					cwd: "/tmp",
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(code, `stdout:\n${output}\nstderr:\n${stderr}`).toBe(0);
			const result = JSON.parse(output.trim());
			expect(result.redirectFailure).not.toBeNull();
			expect(result.redirectFailure.status).not.toBe(0);
			expect(result.redirectFailure.stderr).toContain("missing-input-file");
			// A real shell truncates and creates the redirect target before exec runs.
			expect(result.redirectTarget).toBe("");
			expect(result.exitFailure).toEqual({ status: 7 });
		});

		it("async exec() redirection writes command stdout into the kernel VFS", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { exec } = require('child_process');
      exec('printf hi > async-out.txt', (error, stdout, stderr) => {
        console.log(JSON.stringify({
          error: error ? String(error.message) : null,
          stdout,
        }));
        process.exit(error ? 1 : 0);
      });
    `,
				],
				{
					cwd: "/tmp",
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(code, `stdout:\n${output}\nstderr:\n${stderr}`).toBe(0);
			const result = JSON.parse(output.trim());
			expect(result.error).toBeNull();
			expect(result.stdout).toBe("");
			expect(
				new TextDecoder().decode(await ctx.vfs.readFile("/tmp/async-out.txt")),
			).toBe("hi");
		});

		it("spawn with shell:true performs redirection through the guest shell", async () => {
			ctx = await createBridgeIntegrationKernel();

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { spawn } = require('child_process');
      const child = spawn('printf hi > spawn-out.txt', { shell: true });
      child.on('close', (code) => {
        console.log('close:' + code);
        process.exit(code ?? 1);
      });
    `,
				],
				{
					cwd: "/tmp",
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(code, `stdout:\n${output}\nstderr:\n${stderr}`).toBe(0);
			expect(output).toContain("close:0");
			expect(
				new TextDecoder().decode(await ctx.vfs.readFile("/tmp/spawn-out.txt")),
			).toBe("hi");
		});

		it("execFileSync on node_modules/.bin shell shims unwraps to the node entrypoint", async () => {
			const projectRoot = mkdtempSync(
				join(tmpdir(), "agentos-node-bin-shim-"),
			);
			cleanupPaths.push(projectRoot);

			mkdirSync(join(projectRoot, "node_modules", ".bin"), { recursive: true });
			mkdirSync(join(projectRoot, "node_modules", "demo"), { recursive: true });
			writeFileSync(
				join(projectRoot, "node_modules", "demo", "index.js"),
				"#!/usr/bin/env node\nconsole.log(JSON.stringify(process.argv.slice(2)));\n",
			);
			chmodSync(join(projectRoot, "node_modules", "demo", "index.js"), 0o755);
			writeFileSync(
				join(projectRoot, "node_modules", ".bin", "demo"),
				[
					"#!/bin/sh",
					'basedir=$(dirname "$0")',
					'if [ -x "$basedir/node" ]; then',
					'  exec "$basedir/node" "$basedir/../demo/index.js" "$@"',
					"else",
					'  exec node "$basedir/../demo/index.js" "$@"',
					"fi",
					"",
				].join("\n"),
			);
			chmodSync(join(projectRoot, "node_modules", ".bin", "demo"), 0o755);

			const kernel = createKernel({
				filesystem: new NodeFileSystem({ root: projectRoot }),
			});
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: BRIDGE_COMMAND_DIRS }),
			);
			await kernel.mount(createNodeRuntime());
			ctx = {
				kernel,
				vfs: new NodeFileSystem({ root: projectRoot }),
				dispose: () => kernel.dispose(),
			};

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execFileSync } = require('child_process');
      const result = execFileSync('/node_modules/.bin/demo', ['alpha', 'beta'], {
        encoding: 'utf-8',
      });
      process.stdout.write(result);
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			expect(code).toBe(0);

			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(stderr).toBe("");
			expect(output.trim()).toBe(JSON.stringify(["alpha", "beta"]));
		});

		it("execFileSync dereferences node_modules/.bin main-module symlinks", async () => {
			const projectRoot = mkdtempSync(
				join(tmpdir(), "agentos-node-bin-symlink-"),
			);
			cleanupPaths.push(projectRoot);

			mkdirSync(join(projectRoot, "node_modules", ".bin"), { recursive: true });
			mkdirSync(join(projectRoot, "node_modules", "demo", "bin"), {
				recursive: true,
			});
			writeFileSync(
				join(projectRoot, "node_modules", "demo", "package.json"),
				JSON.stringify({ name: "demo", version: "1.2.3" }),
			);
			writeFileSync(
				join(projectRoot, "node_modules", "demo", "bin", "demo.js"),
				'#!/usr/bin/env node\nconsole.log(require("../package.json").version);\n',
			);
			chmodSync(
				join(projectRoot, "node_modules", "demo", "bin", "demo.js"),
				0o755,
			);
			symlinkSync(
				"../demo/bin/demo.js",
				join(projectRoot, "node_modules", ".bin", "demo"),
			);

			const kernel = createKernel({
				filesystem: new NodeFileSystem({ root: projectRoot }),
			});
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: BRIDGE_COMMAND_DIRS }),
			);
			await kernel.mount(createNodeRuntime());
			ctx = {
				kernel,
				vfs: new NodeFileSystem({ root: projectRoot }),
				dispose: () => kernel.dispose(),
			};

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execFileSync } = require('child_process');
      process.stdout.write(execFileSync('/node_modules/.bin/demo', {
        encoding: 'utf-8',
      }));
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(code, stderr).toBe(0);
			expect(output.trim()).toBe("1.2.3");
		});

		it("execFileSync unwraps shell shims whose node entrypoint has no shebang or extension", async () => {
			const projectRoot = mkdtempSync(
				join(tmpdir(), "agentos-node-bin-shim-no-shebang-"),
			);
			cleanupPaths.push(projectRoot);

			mkdirSync(join(projectRoot, "node_modules", ".bin"), { recursive: true });
			mkdirSync(join(projectRoot, "node_modules", "demo", "dist", "bin"), {
				recursive: true,
			});
			writeFileSync(
				join(projectRoot, "node_modules", "demo", "dist", "bin", "demo"),
				'"use strict";\nconsole.log(JSON.stringify(process.argv.slice(2)));\n',
			);
			chmodSync(
				join(projectRoot, "node_modules", "demo", "dist", "bin", "demo"),
				0o755,
			);
			writeFileSync(
				join(projectRoot, "node_modules", ".bin", "demo-no-shebang"),
				[
					"#!/bin/sh",
					'basedir=$(dirname "$0")',
					'if [ -x "$basedir/node" ]; then',
					'  exec "$basedir/node" "$basedir/../demo/dist/bin/demo" "$@"',
					"else",
					'  exec node "$basedir/../demo/dist/bin/demo" "$@"',
					"fi",
					"",
				].join("\n"),
			);
			chmodSync(
				join(projectRoot, "node_modules", ".bin", "demo-no-shebang"),
				0o755,
			);

			const kernel = createKernel({
				filesystem: new NodeFileSystem({ root: projectRoot }),
			});
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: BRIDGE_COMMAND_DIRS }),
			);
			await kernel.mount(createNodeRuntime());
			ctx = {
				kernel,
				vfs: new NodeFileSystem({ root: projectRoot }),
				dispose: () => kernel.dispose(),
			};

			const chunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = ctx.kernel.spawn(
				"node",
				[
					"-e",
					`
      const { execFileSync } = require('child_process');
      const result = execFileSync('/node_modules/.bin/demo-no-shebang', ['gamma', 'delta'], {
        encoding: 'utf-8',
      });
      process.stdout.write(result);
    `,
				],
				{
					onStdout: (data) => chunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				},
			);

			const code = await proc.wait();
			expect(code).toBe(0);

			const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
			const stderr = stderrChunks
				.map((c) => new TextDecoder().decode(c))
				.join("");
			expect(stderr).toBe("");
			expect(output.trim()).toBe(JSON.stringify(["gamma", "delta"]));
		});
	},
);

describeIf(!skipReason, "bridge child_process exploit/abuse paths", () => {
	let ctx: IntegrationKernelResult;

	afterEach(async () => {
		if (ctx) await ctx.dispose();
	});

	it("child_process cannot escape to host shell", async () => {
		ctx = await createBridgeIntegrationKernel();

		// Use a command that produces different output in sandbox vs host:
		// /etc/hostname exists on the host but not in the kernel VFS
		const chunks: Uint8Array[] = [];
		const proc = ctx.kernel.spawn(
			"node",
			[
				"-e",
				`
      const { execSync } = require('child_process');
      try {
        const result = execSync('cat /etc/hostname', { encoding: 'utf-8' });
        // If we get here, the command read a host-only file
        console.log('ESCAPED:' + result.trim());
      } catch (e) {
        // Expected: /etc/hostname doesn't exist in the sandbox VFS
        console.log('sandbox:contained');
      }
    `,
			],
			{
				onStdout: (data) => chunks.push(data),
			},
		);

		await proc.wait();
		const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
		// Positive: command ran in sandbox and couldn't access host filesystem
		expect(output).toContain("sandbox:contained");
		// Negative: no host data leaked
		expect(output).not.toContain("ESCAPED:");
	});

	it("child_process cannot read host filesystem", async () => {
		ctx = await createBridgeIntegrationKernel();

		const chunks: Uint8Array[] = [];
		const proc = ctx.kernel.spawn(
			"node",
			[
				"-e",
				`
      const { execSync } = require('child_process');
      try {
        // /etc/passwd doesn't exist in the kernel VFS
        execSync('cat /etc/passwd', { encoding: 'utf-8' });
        console.log('SECURITY_BREACH');
      } catch (e) {
        console.log('blocked');
      }
    `,
			],
			{
				onStdout: (data) => chunks.push(data),
			},
		);

		await proc.wait();
		const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
		expect(output).not.toContain("SECURITY_BREACH");
		expect(output).toContain("blocked");
	});

	it("child_process write goes to kernel VFS not host", async () => {
		ctx = await createBridgeIntegrationKernel();

		const chunks: Uint8Array[] = [];
		const proc = ctx.kernel.spawn(
			"node",
			[
				"-e",
				`
      const { execSync } = require('child_process');
      execSync('echo "written-by-child" > /tmp/child-output.txt');
      const result = execSync('cat /tmp/child-output.txt', { encoding: 'utf-8' });
      console.log(result.trim());
    `,
			],
			{
				onStdout: (data) => chunks.push(data),
			},
		);

		const code = await proc.wait();
		expect(code).toBe(0);

		const output = chunks.map((c) => new TextDecoder().decode(c)).join("");
		expect(output).toContain("written-by-child");

		// Verify the file was written to kernel VFS
		const content = await ctx.vfs.readFile("/tmp/child-output.txt");
		expect(new TextDecoder().decode(content)).toContain("written-by-child");
	});
});
