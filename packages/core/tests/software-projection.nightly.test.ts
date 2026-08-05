import common, { coreutils } from "@agentos-software/common";
import pi from "@agentos-software/pi";
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

	test("projects package roots under /opt/agentos without cwd node_modules", async () => {
		vm = await AgentOs.create({
			software: [pi],
		});

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn(
			"node",
			[
				"-e",
				[
					"const fs = require('node:fs');",
					"console.log('root', fs.existsSync('/opt/agentos/pkgs/pi/current'));",
					"console.log('adapter', fs.existsSync('/opt/agentos/pkgs/pi/current/node_modules/@agentos-software/pi/package.json'));",
					"console.log('agent', fs.existsSync('/opt/agentos/pkgs/pi/current/node_modules/@earendil-works/pi-coding-agent/package.json'));",
					"console.log('pi', fs.existsSync('/opt/agentos/bin/pi'));",
					"console.log('pi-acp', fs.existsSync('/opt/agentos/bin/pi-acp'));",
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
		expect(stdout).toContain("root true");
		expect(stdout).toContain("adapter true");
		expect(stdout).toContain("agent true");
		expect(stdout).toContain("pi true");
		expect(stdout).toContain("pi-acp true");
	});

	test("keeps projected package roots read-only on the sidecar path", async () => {
		vm = await AgentOs.create({
			software: [pi],
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
					"  fs.appendFileSync('/opt/agentos/pkgs/pi/current/agentos-package.json', '\\nblocked');",
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
