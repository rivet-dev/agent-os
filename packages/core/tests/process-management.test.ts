import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";

describe("process management", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create();
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("listProcesses() returns empty when no processes spawned", () => {
		expect(vm.listProcesses()).toEqual([]);
	});

	test("listProcesses() includes processes started via spawn()", async () => {
		// Write a script that stays alive for a few seconds
		await vm.writeFile("/tmp/long-running.mjs", "setTimeout(() => {}, 30000);");
		const { pid } = vm.spawn("node", ["/tmp/long-running.mjs"], {
			env: { HOME: "/home/agentos" },
		});

		const list = vm.listProcesses();
		expect(list.length).toBe(1);
		expect(list[0].pid).toBe(pid);
		expect(list[0].command).toBe("node");
		expect(list[0].args).toEqual(["/tmp/long-running.mjs"]);
		expect(list[0].running).toBe(true);
		expect(list[0].exitCode).toBeNull();

		vm.killProcess(pid);
	}, 30_000);

	test("getProcess(pid) returns correct ProcessInfo for a running process", async () => {
		await vm.writeFile("/tmp/alive.mjs", "setTimeout(() => {}, 30000);");
		const { pid } = vm.spawn("node", ["/tmp/alive.mjs"], {
			env: { HOME: "/home/agentos" },
		});

		const info = vm.getProcess(pid);
		expect(info.pid).toBe(pid);
		expect(info.command).toBe("node");
		expect(info.args).toEqual(["/tmp/alive.mjs"]);
		expect(info.running).toBe(true);
		expect(info.exitCode).toBeNull();

		vm.killProcess(pid);
	}, 30_000);

	test("getProcess with invalid pid throws", () => {
		expect(() => vm.getProcess(99999)).toThrow("Process not found");
	});

	test("stopProcess(pid) terminates the process gracefully", async () => {
		await vm.writeFile("/tmp/stop-me.mjs", "setTimeout(() => {}, 30000);");
		const { pid } = vm.spawn("node", ["/tmp/stop-me.mjs"], {
			env: { HOME: "/home/agentos" },
		});

		expect(vm.getProcess(pid).running).toBe(true);

		vm.stopProcess(pid);

		// Wait for process to exit
		await vm.waitProcess(pid);
		expect(vm.getProcess(pid).running).toBe(false);
		expect(vm.getProcess(pid).exitCode).not.toBeNull();
	}, 30_000);

	test("killProcess(pid) force-kills the process", async () => {
		await vm.writeFile("/tmp/kill-me.mjs", "setTimeout(() => {}, 30000);");
		const { pid } = vm.spawn("node", ["/tmp/kill-me.mjs"], {
			env: { HOME: "/home/agentos" },
		});

		expect(vm.getProcess(pid).running).toBe(true);

		vm.killProcess(pid);

		// Wait for process to exit
		await vm.waitProcess(pid);
		expect(vm.getProcess(pid).running).toBe(false);
	}, 30_000);

	test("listProcesses() reflects process exit (running: false, exitCode set)", async () => {
		// Write a script that exits immediately with code 0
		await vm.writeFile("/tmp/quick-exit.mjs", "process.exit(0);");
		const { pid } = vm.spawn("node", ["/tmp/quick-exit.mjs"], {
			env: { HOME: "/home/agentos" },
		});

		// Wait for it to exit
		await vm.waitProcess(pid);

		const list = vm.listProcesses();
		expect(list.length).toBe(1);
		expect(list[0].running).toBe(false);
		expect(list[0].exitCode).toBe(0);
	}, 30_000);

	test("stopProcess on already-exited process is a no-op", async () => {
		await vm.writeFile("/tmp/already-done.mjs", "process.exit(0);");
		const { pid } = vm.spawn("node", ["/tmp/already-done.mjs"], {
			env: { HOME: "/home/agentos" },
		});

		await vm.waitProcess(pid);

		// Should not throw — just a no-op
		expect(() => vm.stopProcess(pid)).not.toThrow();
	}, 30_000);

	test("nested child_process.spawn executes the requested child entrypoint", async () => {
		await vm.writeFile(
			"/tmp/child.mjs",
			[
				"const chunks = [];",
				"process.stdin.on('data', (chunk) => chunks.push(Buffer.from(chunk)));",
				"process.stdin.on('end', () => {",
				"  const stdin = Buffer.concat(chunks).toString('utf8');",
				"  process.stdout.write(JSON.stringify({ tag: 'child', stdin }));",
				"});",
				"",
			].join("\n"),
		);
		await vm.writeFile(
			"/tmp/parent.mjs",
			[
				"import { spawn } from 'node:child_process';",
				"const child = spawn('node', ['/tmp/child.mjs'], { stdio: ['pipe', 'pipe', 'pipe'] });",
				"let stdout = '';",
				"let stderr = '';",
				"child.stdout.on('data', (chunk) => { stdout += chunk.toString('utf8'); });",
				"child.stderr.on('data', (chunk) => { stderr += chunk.toString('utf8'); });",
				"child.on('error', (error) => { stderr += String(error?.stack ?? error); });",
				"child.stdin.write(JSON.stringify({ request_id: 'abc', type: 'control_request' }) + '\\n');",
				"child.stdin.write(JSON.stringify({ type: 'user', message: { text: 'hello' } }) + '\\n');",
				"child.stdin.end();",
				"child.on('close', (code) => {",
				"  process.stdout.write(JSON.stringify({ stdout, stderr, code }));",
				"  process.exit(code ?? 0);",
				"});",
				"",
			].join("\n"),
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/parent.mjs"], {
			env: { HOME: "/home/agentos" },
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode).toBe(0);
		expect(stderr).toBe("");

		const parentResult = JSON.parse(stdout) as {
			stdout: string;
			stderr: string;
			code: number;
		};
		expect(parentResult.code).toBe(0);
		expect(parentResult.stderr).toBe("");

		const childResult = JSON.parse(parentResult.stdout) as {
			tag: string;
			stdin: string;
		};
		expect(childResult.tag).toBe("child");
		expect(childResult.stdin).toBe(
			[
				JSON.stringify({ request_id: "abc", type: "control_request" }),
				JSON.stringify({ type: "user", message: { text: "hello" } }),
				"",
			].join("\n"),
		);
	}, 30_000);

	test("nested shell spawn drains stdout through readable events before close", async () => {
		await vm.writeFile(
			"/tmp/shell-stream-parent.mjs",
			[
				"import { spawn } from 'node:child_process';",
				"import { once } from 'node:events';",
				"const child = spawn('printf SHELL_STREAM_OK', [], {",
				"  shell: '/bin/sh',",
				"  stdio: ['ignore', 'pipe', 'pipe'],",
				"  detached: true,",
				"});",
				"const collect = (stream) => new Promise((resolve, reject) => {",
				"  const chunks = [];",
				"  stream.on('readable', () => {",
				"    let chunk;",
				"    while ((chunk = stream.read()) !== null) chunks.push(Buffer.from(chunk));",
				"  });",
				"  stream.once('error', reject);",
				"  stream.once('end', () => resolve(Buffer.concat(chunks).toString('utf8')));",
				"});",
				"const stdoutPromise = collect(child.stdout);",
				"const stderrPromise = collect(child.stderr);",
				"const closePromise = once(child, 'close');",
				"const [stdout, stderr, [code]] = await Promise.all([stdoutPromise, stderrPromise, closePromise]);",
				"process.stdout.write(JSON.stringify({ stdout, stderr, code }));",
				"",
			].join("\n"),
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/shell-stream-parent.mjs"], {
			env: { HOME: "/home/agentos" },
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});

		expect(await vm.waitProcess(pid)).toBe(0);
		expect(stderr).toBe("");
		expect(JSON.parse(stdout)).toEqual({
			stdout: "SHELL_STREAM_OK",
			stderr: "",
			code: 0,
		});
	}, 30_000);

	test("nested shell can launch the registered node runtime", async () => {
		await vm.mkdir("/tmp/node-test-project/test", { recursive: true });
		await vm.writeFile(
			"/tmp/node-test-project/test/shell-node.test.mjs",
			[
				"import assert from 'node:assert/strict';",
				"import test from 'node:test';",
				"test('shell node test runner', () => assert.equal(2 + 2, 4));",
				"",
			].join("\n"),
		);
		await vm.writeFile(
			"/tmp/shell-node-parent.mjs",
			[
				"import { spawn } from 'node:child_process';",
				"import { once } from 'node:events';",
				"const child = spawn('/bin/sh', ['-c', 'node --test'], {",
				"  cwd: '/tmp/node-test-project',",
				"  stdio: ['ignore', 'pipe', 'pipe'],",
				"});",
				"let stdout = '';",
				"let stderr = '';",
				"child.stdout.on('data', (chunk) => { stdout += chunk.toString('utf8'); });",
				"child.stderr.on('data', (chunk) => { stderr += chunk.toString('utf8'); });",
				"const [code] = await once(child, 'close');",
				"process.stdout.write(JSON.stringify({ stdout, stderr, code }));",
				"",
			].join("\n"),
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/shell-node-parent.mjs"], {
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});

		expect(await vm.waitProcess(pid)).toBe(0);
		expect(stderr).toBe("");
		expect(JSON.parse(stdout)).toEqual({
			stdout: expect.stringContaining("ok 1 - shell node test runner"),
			stderr: "",
			code: 0,
		});
	}, 30_000);

	test("nested node test failures close with a nonzero status", async () => {
		await vm.writeFile(
			"/tmp/shell-node-failure.test.mjs",
			[
				"import assert from 'node:assert/strict';",
				"import test from 'node:test';",
				"test('expected failure', () => assert.equal(1, 2));",
				"",
			].join("\n"),
		);
		await vm.writeFile(
			"/tmp/shell-node-failure-parent.mjs",
			[
				"import { spawn } from 'node:child_process';",
				"import { once } from 'node:events';",
				"const child = spawn('/bin/sh', ['-c', 'node --test /tmp/shell-node-failure.test.mjs'], {",
				"  stdio: ['ignore', 'pipe', 'pipe'],",
				"});",
				"let stdout = '';",
				"let stderr = '';",
				"child.stdout.on('data', (chunk) => { stdout += chunk.toString('utf8'); });",
				"child.stderr.on('data', (chunk) => { stderr += chunk.toString('utf8'); });",
				"const [code] = await once(child, 'close');",
				"process.stdout.write(JSON.stringify({ stdout, stderr, code }));",
				"",
			].join("\n"),
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/shell-node-failure-parent.mjs"], {
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});

		expect(await vm.waitProcess(pid)).toBe(0);
		expect(stderr).toBe("");
		expect(JSON.parse(stdout)).toEqual({
			stdout: expect.stringContaining("not ok 1 - expected failure"),
			stderr: "",
			code: 1,
		});
	}, 30_000);

	test("npm test completes through the nested node test runner", async () => {
		await vm.mkdir("/workspace/npm-test/test", { recursive: true });
		await vm.writeFile(
			"/workspace/npm-test/package.json",
			JSON.stringify({
				name: "nested-npm-test",
				private: true,
				type: "module",
				scripts: { test: "node --test" },
			}),
		);
		await vm.writeFile(
			"/workspace/npm-test/test/smoke.test.mjs",
			[
				'import assert from "node:assert/strict";',
				'import test from "node:test";',
				'test("smoke", () => assert.equal(2 + 2, 4));',
				'test("strict equality distinguishes signed zero", () => {',
				'  assert.throws(() => assert.equal(-0, 0), assert.AssertionError);',
				'  assert.throws(() => assert.strictEqual(-0, 0), assert.AssertionError);',
				'});',
				"",
			].join("\n"),
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn(
			"/bin/bash",
			["-c", "npm test && pwd && printf shell-finished"],
			{
			cwd: "/workspace/npm-test",
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
				onStderr: (chunk) => {
					stderr += Buffer.from(chunk).toString("utf8");
				},
			},
		);

		expect(await vm.waitProcess(pid), stderr).toBe(0);
		expect(stdout).toContain("ok 1 - smoke");
		expect(stdout).toContain("ok 2 - strict equality distinguishes signed zero");
		expect(stdout).toContain("/workspace/npm-test");
		expect(stdout).toContain("shell-finished");
	}, 30_000);

	test("npm test completes below a JavaScript process parent", async () => {
		await vm.mkdir("/workspace/deep-npm-test/test", { recursive: true });
		await vm.writeFile(
			"/workspace/deep-npm-test/package.json",
			JSON.stringify({
				name: "deep-nested-npm-test",
				private: true,
				type: "module",
				scripts: { test: "node --test" },
			}),
		);
		await vm.writeFile(
			"/workspace/deep-npm-test/test/smoke.test.mjs",
			[
				'import assert from "node:assert/strict";',
				'import test from "node:test";',
				'test("smoke", () => assert.equal(2 + 2, 4));',
				'test("strict equality distinguishes signed zero", () => {',
				'  assert.throws(() => assert.equal(-0, 0), assert.AssertionError);',
				'  assert.throws(() => assert.strictEqual(-0, 0), assert.AssertionError);',
				'});',
				"",
			].join("\n"),
		);
		await vm.writeFile(
			"/workspace/deep-npm-test/parent.mjs",
			[
				'import { spawn } from "node:child_process";',
				'import { once } from "node:events";',
				'const child = spawn("/bin/bash", ["-c", "npm test && printf deep-finished"], {',
				'  cwd: "/workspace/deep-npm-test",',
				'  stdio: ["ignore", "inherit", "inherit"],',
				'});',
				'const [code] = await once(child, "close");',
				'process.exit(code ?? 1);',
				"",
			].join("\n"),
		);
		await vm.writeFile(
			"/workspace/deep-npm-test/grandparent.mjs",
			[
				'import { spawn } from "node:child_process";',
				'import { once } from "node:events";',
				'const child = spawn("node", ["/workspace/deep-npm-test/parent.mjs"], {',
				'  cwd: "/workspace/deep-npm-test",',
				'  stdio: ["ignore", "inherit", "inherit"],',
				'});',
				'const [code] = await once(child, "close");',
				'process.exit(code ?? 1);',
				"",
			].join("\n"),
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn(
			"node",
			["/workspace/deep-npm-test/grandparent.mjs"],
			{
				cwd: "/workspace/deep-npm-test",
				env: {
					AGENTOS_EAGER_STDIN_HANDLE: "1",
					AGENTOS_KEEP_STDIN_OPEN: "1",
				},
				onStdout: (chunk) => {
					stdout += Buffer.from(chunk).toString("utf8");
				},
				onStderr: (chunk) => {
					stderr += Buffer.from(chunk).toString("utf8");
				},
			},
		);

		expect(await vm.waitProcess(pid), stderr).toBe(0);
		expect(stdout).toContain("ok 1 - smoke");
		expect(stdout).toContain("ok 2 - strict equality distinguishes signed zero");
		expect(stdout).toContain("deep-finished");
	}, 30_000);

	test("nested node and shell processes preserve the requested cwd", async () => {
		await vm.writeFile(
			"/tmp/cwd-grandchild.mjs",
			[
				"import { spawn } from 'node:child_process';",
				"import { once } from 'node:events';",
				"import { realpathSync } from 'node:fs';",
				"const child = spawn('/bin/sh', ['-c', 'printf CWD_OK > nested-cwd.txt'], {",
				"  stdio: ['ignore', 'pipe', 'pipe'],",
				"});",
				"const [code] = await once(child, 'close');",
				"let realCwd;",
				"try { realCwd = realpathSync(process.cwd()); } catch (error) { realCwd = { code: error.code, message: error.message }; }",
				"process.stdout.write(JSON.stringify({ code, cwd: process.cwd(), realCwd }));",
				"",
			].join("\n"),
		);
		await vm.writeFile(
			"/tmp/cwd-parent.mjs",
			[
				"import { spawn } from 'node:child_process';",
				"import { once } from 'node:events';",
				"const child = spawn('/bin/node', ['/tmp/cwd-grandchild.mjs'], {",
				"  cwd: '/workspace',",
				"  stdio: ['ignore', 'pipe', 'pipe'],",
				"});",
				"let stdout = '';",
				"child.stdout.on('data', (chunk) => { stdout += chunk.toString('utf8'); });",
				"const [code] = await once(child, 'close');",
				"process.stdout.write(JSON.stringify({ code, child: JSON.parse(stdout) }));",
				"",
			].join("\n"),
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/cwd-parent.mjs"], {
			cwd: "/workspace",
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});

		expect(await vm.waitProcess(pid), stderr).toBe(0);
		expect(JSON.parse(stdout)).toEqual({
			code: 0,
			child: { code: 0, cwd: "/workspace", realCwd: "/workspace" },
		});
		expect(
			new TextDecoder().decode(await vm.readFile("/workspace/nested-cwd.txt")),
		).toBe("CWD_OK");
	}, 30_000);

	test("fast nested child streams retain completion for late consumers", async () => {
		await vm.writeFile(
			"/tmp/late-child-stream-parent.mjs",
			[
				"import { spawn } from 'node:child_process';",
				"const run = async (index) => {",
				"  const child = spawn(`printf LATE_STREAM_OK_${index}`, [], {",
				"    shell: '/bin/sh',",
				"    stdio: ['ignore', 'pipe', 'pipe'],",
				"  });",
				"  await new Promise((resolve, reject) => {",
				"    child.once('error', reject);",
				"    child.once('close', resolve);",
				"  });",
				"  const drain = (stream) => {",
				"    const chunks = [];",
				"    let chunk;",
				"    while ((chunk = stream.read()) !== null) chunks.push(Buffer.from(chunk));",
				"    return Buffer.concat(chunks).toString('utf8');",
				"  };",
				"  const stdout = drain(child.stdout);",
				"  const stderr = drain(child.stderr);",
				"  await Promise.resolve();",
				"  return {",
				"    stdout,",
				"    stderr,",
				"    stdoutEnded: child.stdout.readableEnded,",
				"    stderrEnded: child.stderr.readableEnded,",
				"    exitCode: child.exitCode,",
				"  };",
				"};",
				// Exercise the post-exit stream drain under enough concurrent fast
				// children to expose event-pump scheduling races.
				"const results = await Promise.all(Array.from({ length: 128 }, (_, index) => run(index)));",
				"process.stdout.write(JSON.stringify(results));",
				"",
			].join("\n"),
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/late-child-stream-parent.mjs"], {
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});

		expect(await vm.waitProcess(pid)).toBe(0);
		expect(stderr).toBe("");
		expect(JSON.parse(stdout)).toEqual(
			Array.from({ length: 128 }, (_, index) => ({
				stdout: `LATE_STREAM_OK_${index}`,
				stderr: "",
				stdoutEnded: true,
				stderrEnded: true,
				exitCode: 0,
			})),
		);
	}, 30_000);

	test("JavaScript process supports node:crypto createHash", async () => {
		await vm.writeFile(
			"/tmp/create-hash.mjs",
			[
				'import { createHash } from "node:crypto";',
				'process.stdout.write(createHash("sha1").update("agentos").digest("hex"));',
				"",
			].join("\n"),
		);
		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/create-hash.mjs"], {
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});

		expect(await vm.waitProcess(pid), stderr).toBe(0);
		expect(stderr).toBe("");
		expect(stdout).toBe("72a204ddd2a99dd32152140a87b81f4087d86077");
	}, 30_000);

	test("nested child inherited output files are readable from the exit event", async () => {
		await vm.writeFile(
			"/tmp/inherited-fd-child.mjs",
			"process.stdout.write('inherited-output');",
		);
		await vm.writeFile(
			"/tmp/inherited-fd-parent.mjs",
			[
				"import { closeSync, openSync } from 'node:fs';",
				"import { open, readFile } from 'node:fs/promises';",
				"import { spawn } from 'node:child_process';",
				"const outputPath = '/tmp/inherited-output.txt';",
				"const fd = openSync(outputPath, 'w');",
				"const child = spawn('node', ['/tmp/inherited-fd-child.mjs'], { stdio: ['ignore', fd, fd] });",
				"closeSync(fd);",
				"child.on('exit', async (code) => {",
				"  const handle = await open(outputPath, 'r');",
				"  if (typeof handle[Symbol.asyncDispose] !== 'function') throw new Error('FileHandle is not async disposable');",
				"  await handle[Symbol.asyncDispose]();",
				"  const output = await readFile(outputPath, 'utf8');",
				"  process.stdout.write(JSON.stringify({ code, output }));",
				"});",
				"",
			].join("\n"),
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/inherited-fd-parent.mjs"], {
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});

		expect(await vm.waitProcess(pid), stderr).toBe(0);
		expect(stderr).toBe("");
		expect(JSON.parse(stdout)).toEqual({ code: 0, output: "inherited-output" });
	}, 30_000);
});
