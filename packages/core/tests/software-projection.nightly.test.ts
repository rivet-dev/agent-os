import common, { coreutils } from "@agentos-software/common";
import { afterEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";

async function waitForExit(
	vm: AgentOs,
	pid: number,
	timeoutMs = 30_000,
): Promise<number> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		const proc = vm.getProcess(pid);
		if (!proc.running) {
			return proc.exitCode ?? -1;
		}
		await new Promise((resolve) => setTimeout(resolve, 20));
	}

	throw new Error(`Timed out waiting for process ${pid} to exit`);
}

describe("software projection on the sidecar path", () => {
	let vm: AgentOs | undefined;

	afterEach(async () => {
		await vm?.dispose();
		vm = undefined;
	});

	test("preserves projected package roots without cwd node_modules", async () => {
		vm = await AgentOs.create({
			software: [],
		});

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn(
			"node",
			[
				"-e",
				[
					"const fs = require('node:fs');",
					"console.log('node_modules', fs.existsSync('/root/node_modules'));",
					"console.log('scope', fs.readdirSync('/root/node_modules/@rivet-dev').includes('agentos-pi'));",
					"console.log('adapter', fs.existsSync('/root/node_modules/@agentos-software/pi/package.json'));",
					"console.log('adapterResolved', Boolean(require.resolve('@agentos-software/pi')));",
					"console.log('agent', fs.existsSync('/root/node_modules/@mariozechner/pi-coding-agent/package.json'));",
				].join(" "),
			],
			{
				onStdout: (chunk) => {
					stdout += Buffer.from(chunk).toString("utf8");
				},
				onStderr: (chunk) => {
					stderr += Buffer.from(chunk).toString("utf8");
				},
			},
		);

		const exitCode = await waitForExit(vm, pid);
		expect({ exitCode, stderr }).toEqual({ exitCode: 0, stderr: "" });
		expect(stdout).toContain("node_modules true");
		expect(stdout).toContain("scope true");
		expect(stdout).toContain("adapter true");
		expect(stdout).toContain("adapterResolved true");
		expect(stdout).toContain("agent true");
	});

	test("keeps projected package roots read-only on the sidecar path", async () => {
		vm = await AgentOs.create({
			software: [],
		});

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn(
			"node",
			[
				"-e",
				[
					"const fs = require('node:fs');",
					"try {",
					"  fs.appendFileSync('/root/node_modules/@agentos-software/pi/package.json', '\\nblocked');",
					"  console.log('write:unexpected-success');",
					"} catch (error) {",
					"  console.log('writeError', error && error.code);",
					"}",
				].join(" "),
			],
			{
				onStdout: (chunk) => {
					stdout += Buffer.from(chunk).toString("utf8");
				},
				onStderr: (chunk) => {
					stderr += Buffer.from(chunk).toString("utf8");
				},
			},
		);

		const exitCode = await waitForExit(vm, pid);
		expect({ exitCode, stderr }).toEqual({ exitCode: 0, stderr: "" });
		expect(stdout).not.toContain("write:unexpected-success");
		expect(stdout).toMatch(/writeError (ERR_ACCESS_DENIED|EACCES|EPERM|EROFS)/);
	});

	test("preserves registry meta-package command injection on the sidecar path", async () => {
			vm = await AgentOs.create({
				software: [common],
			});

			expect(await vm.exists("/bin/cat")).toBe(true);
			expect(await vm.exists("/bin/grep")).toBe(true);
	});
});
