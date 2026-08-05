import common from "@agentos-software/common";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";

describe("child_process detached", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create();
	}, 30_000);

	afterEach(async () => {
		if (vm) {
			await vm.dispose();
		}
	}, 30_000);

	test("detached unref child processes survive parent exit", async () => {
		await vm.writeFile(
			"/tmp/detached-child.mjs",
			[
				"import net from 'node:net';",
				"import fs from 'node:fs';",
				"const socketPath = '/tmp/detached-test.sock';",
				"fs.writeFileSync('/tmp/detached-child-started.txt', 'started');",
				"try { fs.unlinkSync(socketPath); } catch {}",
				"const server = net.createServer((socket) => socket.end('ok'));",
				"server.listen(socketPath, () => {",
				"  fs.writeFileSync('/tmp/detached-child-listening.txt', String(process.pid));",
				"});",
				"setInterval(() => {}, 1000);",
			].join("\n"),
		);
		await vm.writeFile(
			"/tmp/detached-parent.mjs",
			[
				"import { spawn } from 'node:child_process';",
				"const child = spawn('node', ['/tmp/detached-child.mjs'], {",
				"  detached: true,",
				"  stdio: ['ignore', 'ignore', 'ignore'],",
				"});",
				"child.unref();",
				"console.log('PARENT_DONE:' + child.pid);",
			].join("\n"),
		);
		await vm.writeFile(
			"/tmp/detached-probe.mjs",
			[
				"import fs from 'node:fs';",
				"import net from 'node:net';",
				"const socketPath = '/tmp/detached-test.sock';",
				"const deadline = Date.now() + 5000;",
				"while (Date.now() < deadline) {",
				"  const connected = await new Promise((resolve) => {",
				"    const socket = net.createConnection(socketPath);",
				"    const timer = setTimeout(() => { socket.destroy(); resolve(false); }, 250);",
				"    socket.on('connect', () => { clearTimeout(timer); socket.destroy(); resolve(true); });",
				"    socket.on('error', () => { clearTimeout(timer); resolve(false); });",
				"  });",
				"  if (connected) {",
				"    console.log('PROBE_CONNECTED');",
				"    process.exit(0);",
				"  }",
				"  await new Promise((resolve) => setTimeout(resolve, 50));",
				"}",
				"console.log(JSON.stringify({",
				"  started: fs.existsSync('/tmp/detached-child-started.txt'),",
				"  listening: fs.existsSync('/tmp/detached-child-listening.txt'),",
				"}));",
				"process.exit(1);",
			].join("\n"),
		);

		let parentStdout = "";
		let parentStderr = "";
		const { pid } = vm.spawn("node", ["/tmp/detached-parent.mjs"], {
			onStdout: (data) => {
				parentStdout += new TextDecoder().decode(data);
			},
			onStderr: (data) => {
				parentStderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode, `stdout:\n${parentStdout}\nstderr:\n${parentStderr}`).toBe(
			0,
		);

		const detachedChildPid = Number(
			parentStdout.match(/PARENT_DONE:(\d+)/)?.[1] ?? NaN,
		);
		expect(detachedChildPid).toBeGreaterThan(0);

		let probeStdout = "";
		let probeStderr = "";
		const probe = vm.spawn("node", ["/tmp/detached-probe.mjs"], {
			onStdout: (data) => {
				probeStdout += new TextDecoder().decode(data);
			},
			onStderr: (data) => {
				probeStderr += new TextDecoder().decode(data);
			},
		});
		const probeExitCode = await vm.waitProcess(probe.pid);
		expect(
			probeExitCode,
			`stdout:\n${probeStdout}\nstderr:\n${probeStderr}`,
		).toBe(0);
		expect(probeStdout).toContain("PROBE_CONNECTED");

		const detachedProcess = vm
			.allProcesses()
			.find((process) => process.pid === detachedChildPid);
		expect(detachedProcess?.command).toBe("node");
	}, 30_000);

	test("detached unix socket daemons can read line-delimited requests and reply", async () => {
		await vm.writeFile(
			"/tmp/detached-echo-child.mjs",
			[
				"import fs from 'node:fs';",
				"import net from 'node:net';",
				"import readline from 'node:readline';",
				"const socketPath = '/tmp/detached-echo.sock';",
				"try { fs.unlinkSync(socketPath); } catch {}",
				"const server = net.createServer((conn) => {",
				"  const rl = readline.createInterface({ input: conn });",
				"  rl.on('line', (line) => {",
				"    fs.writeFileSync('/tmp/detached-echo-last-line.txt', line);",
				"    conn.write('reply:' + line + '\\n');",
				"  });",
				"});",
				"server.listen(socketPath, () => {",
				"  fs.writeFileSync('/tmp/detached-echo-listening.txt', String(process.pid));",
				"});",
				"setInterval(() => {}, 1000);",
			].join("\n"),
		);
		await vm.writeFile(
			"/tmp/detached-echo-parent.mjs",
			[
				"import { spawn } from 'node:child_process';",
				"const child = spawn('node', ['/tmp/detached-echo-child.mjs'], {",
				"  detached: true,",
				"  stdio: ['ignore', 'ignore', 'ignore'],",
				"});",
				"child.unref();",
				"console.log('PARENT_DONE:' + child.pid);",
			].join("\n"),
		);
		await vm.writeFile(
			"/tmp/detached-echo-probe.mjs",
			[
				"import fs from 'node:fs';",
				"import net from 'node:net';",
				"const socketPath = '/tmp/detached-echo.sock';",
				"const deadline = Date.now() + 5000;",
				"while (Date.now() < deadline) {",
				"  const result = await new Promise((resolve) => {",
				"    const socket = net.createConnection(socketPath);",
				"    const timer = setTimeout(() => { socket.destroy(); resolve(null); }, 500);",
				"    let data = '';",
				"    socket.on('connect', () => { socket.write('ping\\n'); });",
				"    socket.on('data', (chunk) => {",
				"      data += chunk.toString();",
				"      if (data.includes('\\n')) {",
				"        clearTimeout(timer);",
				"        socket.destroy();",
				"        resolve(data);",
				"      }",
				"    });",
				"    socket.on('end', () => { clearTimeout(timer); resolve(data); });",
				"    socket.on('close', () => {",
				"      if (data) { clearTimeout(timer); resolve(data); }",
				"    });",
				"    socket.on('error', () => { clearTimeout(timer); resolve(null); });",
				"  });",
				"  if (result) {",
				"    console.log('PROBE_REPLY:' + result.trim());",
				"    process.exit(0);",
				"  }",
				"  await new Promise((resolve) => setTimeout(resolve, 50));",
				"}",
				"console.log(JSON.stringify({",
				"  listening: fs.existsSync('/tmp/detached-echo-listening.txt'),",
				"  lastLine: fs.existsSync('/tmp/detached-echo-last-line.txt')",
				"    ? fs.readFileSync('/tmp/detached-echo-last-line.txt', 'utf8')",
				"    : null,",
				"}));",
				"process.exit(1);",
			].join("\n"),
		);

		let parentStdout = "";
		let parentStderr = "";
		const { pid } = vm.spawn("node", ["/tmp/detached-echo-parent.mjs"], {
			onStdout: (data) => {
				parentStdout += new TextDecoder().decode(data);
			},
			onStderr: (data) => {
				parentStderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode, `stdout:\n${parentStdout}\nstderr:\n${parentStderr}`).toBe(
			0,
		);

		let probeStdout = "";
		let probeStderr = "";
		const probe = vm.spawn("node", ["/tmp/detached-echo-probe.mjs"], {
			onStdout: (data) => {
				probeStdout += new TextDecoder().decode(data);
			},
			onStderr: (data) => {
				probeStderr += new TextDecoder().decode(data);
			},
		});
		const probeExitCode = await vm.waitProcess(probe.pid);
		expect(
			probeExitCode,
			`stdout:\n${probeStdout}\nstderr:\n${probeStderr}`,
		).toBe(0);
		expect(probeStdout).toContain("PROBE_REPLY:reply:ping");
	}, 30_000);

	test("detached unix socket daemons can use fs.promises inside request handlers", async () => {
		await vm.writeFile("/tmp/detached-fs-data.txt", "ready");
		await vm.writeFile(
			"/tmp/detached-fs-child.mjs",
			[
				"import fs from 'node:fs';",
				"import net from 'node:net';",
				"import readline from 'node:readline';",
				"const socketPath = '/tmp/detached-fs.sock';",
				"try { fs.unlinkSync(socketPath); } catch {}",
				"const server = net.createServer((conn) => {",
				"  const rl = readline.createInterface({ input: conn });",
				"  rl.on('line', async () => {",
				"    const value = await fs.promises.readFile('/tmp/detached-fs-data.txt', 'utf8');",
				"    conn.write('reply:' + value + '\\n');",
				"  });",
				"});",
				"server.listen(socketPath, () => {",
				"  fs.writeFileSync('/tmp/detached-fs-listening.txt', String(process.pid));",
				"});",
				"setInterval(() => {}, 1000);",
			].join("\n"),
		);
		await vm.writeFile(
			"/tmp/detached-fs-parent.mjs",
			[
				"import { spawn } from 'node:child_process';",
				"const child = spawn('node', ['/tmp/detached-fs-child.mjs'], {",
				"  detached: true,",
				"  stdio: ['ignore', 'ignore', 'ignore'],",
				"});",
				"child.unref();",
				"console.log('PARENT_DONE:' + child.pid);",
			].join("\n"),
		);
		await vm.writeFile(
			"/tmp/detached-fs-probe.mjs",
			[
				"import net from 'node:net';",
				"const socketPath = '/tmp/detached-fs.sock';",
				"const deadline = Date.now() + 5000;",
				"while (Date.now() < deadline) {",
				"  const result = await new Promise((resolve) => {",
				"    const socket = net.createConnection(socketPath);",
				"    const timer = setTimeout(() => { socket.destroy(); resolve(null); }, 1000);",
				"    let data = '';",
				"    socket.on('connect', () => { socket.write('ping\\n'); });",
				"    socket.on('data', (chunk) => {",
				"      data += chunk.toString();",
				"      if (data.includes('\\n')) {",
				"        clearTimeout(timer);",
				"        socket.destroy();",
				"        resolve(data);",
				"      }",
				"    });",
				"    socket.on('close', () => {",
				"      if (data) { clearTimeout(timer); resolve(data); }",
				"    });",
				"    socket.on('error', () => { clearTimeout(timer); resolve(null); });",
				"  });",
				"  if (result) {",
				"    console.log('PROBE_REPLY:' + result.trim());",
				"    process.exit(0);",
				"  }",
				"  await new Promise((resolve) => setTimeout(resolve, 50));",
				"}",
				"process.exit(1);",
			].join("\n"),
		);

		let parentStdout = "";
		let parentStderr = "";
		const { pid } = vm.spawn("node", ["/tmp/detached-fs-parent.mjs"], {
			onStdout: (data) => {
				parentStdout += new TextDecoder().decode(data);
			},
			onStderr: (data) => {
				parentStderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode, `stdout:\n${parentStdout}\nstderr:\n${parentStderr}`).toBe(
			0,
		);

		let probeStdout = "";
		let probeStderr = "";
		const probe = vm.spawn("node", ["/tmp/detached-fs-probe.mjs"], {
			onStdout: (data) => {
				probeStdout += new TextDecoder().decode(data);
			},
			onStderr: (data) => {
				probeStderr += new TextDecoder().decode(data);
			},
		});
		const probeExitCode = await vm.waitProcess(probe.pid);
		expect(
			probeExitCode,
			`stdout:\n${probeStdout}\nstderr:\n${probeStderr}`,
		).toBe(0);
		expect(probeStdout).toContain("PROBE_REPLY:reply:ready");
	}, 30_000);
});

// Conformance for the unmodified Pi SDK bash backend shape: resolve the shell
// like Pi's getShellConfig (existsSync /bin/bash, then `which bash`), prepend a
// nonexistent PATH entry like Pi's getShellEnv, spawn the shell binary
// directly with detached: true and piped output, and kill the whole process
// group on timeout via process.kill(-pid, "SIGKILL").
function registerPiShapedShellBackendTests(): void {
	describe("pi-shaped detached shell backend", () => {
		let vm: AgentOs;

		beforeEach(async () => {
			vm = await AgentOs.create({ software: [common] });
		}, 60_000);

		afterEach(async () => {
			if (vm) {
				await vm.dispose();
			}
		}, 30_000);

		test("detached shell spawn, cwd, dead PATH entry, and group kill match Pi's backend", async () => {
			await vm.writeFile(
				"/tmp/pi-backend-probe.mjs",
				[
					"import { spawn, spawnSync } from 'node:child_process';",
					"import { existsSync } from 'node:fs';",
					"let shell = 'sh';",
					"if (existsSync('/bin/bash')) {",
					"  shell = '/bin/bash';",
					"} else {",
					"  const which = spawnSync('which', ['bash'], { timeout: 5000 });",
					"  const resolved = which.status === 0 ? String(which.stdout).trim() : '';",
					"  if (resolved) {",
					"    shell = resolved;",
					"  }",
					"}",
					"console.log('shell-resolved:' + shell);",
					"const env = {",
					"  ...process.env,",
					"  PATH: '/home/agentos/.pi/agent/bin:' + (process.env.PATH || ''),",
					"};",
					"const pwdResult = spawnSync(shell, ['-c', 'pwd'], { cwd: '/tmp', env, encoding: 'utf8' });",
					"console.log('pwd-status:' + pwdResult.status);",
					"console.log('pwd-output:' + String(pwdResult.stdout || '').trim());",
					"const child = spawn(shell, ['-c', 'echo started; sleep 60'], {",
					"  cwd: '/tmp',",
					"  env,",
					"  detached: true,",
					"  stdio: ['ignore', 'pipe', 'pipe'],",
					"});",
					"let captured = '';",
					"const started = new Promise((resolve, reject) => {",
					"  child.on('error', reject);",
					"  child.stdout.on('data', (chunk) => {",
					"    captured += chunk.toString();",
					"    if (captured.includes('started')) {",
					"      resolve();",
					"    }",
					"  });",
					"});",
					"child.stderr.on('data', (chunk) => {",
					"  captured += chunk.toString();",
					"});",
					"await started;",
					"const closed = new Promise((resolve) => {",
					"  child.on('close', (code, signal) => resolve({ code, signal }));",
					"});",
					"const killProcessTree = (pid) => {",
					"  try {",
					"    process.kill(-pid, 'SIGKILL');",
					"  } catch {",
					"    try {",
					"      process.kill(pid, 'SIGKILL');",
					"    } catch {}",
					"  }",
					"};",
					"killProcessTree(child.pid);",
					"const closeResult = await closed;",
					"console.log('close-fired:' + JSON.stringify(closeResult));",
					"let liveness = 'alive';",
					"try {",
					"  process.kill(child.pid, 0);",
					"} catch (error) {",
					"  liveness = (error && error.code) || 'error';",
					"}",
					"console.log('shell-liveness:' + liveness);",
					"console.log('captured:' + captured.trim());",
				].join("\n"),
			);

			let stdout = "";
			let stderr = "";
			const { pid } = vm.spawn("node", ["/tmp/pi-backend-probe.mjs"], {
				onStdout: (data) => {
					stdout += new TextDecoder().decode(data);
				},
				onStderr: (data) => {
					stderr += new TextDecoder().decode(data);
				},
			});

			const exitCode = await vm.waitProcess(pid);
			await new Promise((resolveTask) => setTimeout(resolveTask, 0));
			const context = `stdout:\n${stdout}\nstderr:\n${stderr}`;
			expect(exitCode, context).toBe(0);
			expect(stdout, context).toMatch(/shell-resolved:.*bash/);
			expect(stdout, context).toContain("pwd-status:0");
			expect(stdout, context).toContain("pwd-output:/tmp");
			expect(stdout, context).toContain("close-fired:");
			expect(stdout, context).toContain("shell-liveness:ESRCH");
			expect(stdout, context).toContain("captured:started");
		}, 90_000);
	});
}

registerPiShapedShellBackendTests();
