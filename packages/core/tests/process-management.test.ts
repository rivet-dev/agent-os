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
			env: { HOME: "/home/user" },
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
			env: { HOME: "/home/user" },
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
			env: { HOME: "/home/user" },
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
			env: { HOME: "/home/user" },
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
			env: { HOME: "/home/user" },
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
			env: { HOME: "/home/user" },
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
			env: { HOME: "/home/user" },
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

});
