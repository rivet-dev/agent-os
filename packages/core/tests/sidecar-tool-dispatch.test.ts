import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { z } from "zod";
import { AgentOs, hostTool, toolKit } from "../src/index.js";

const mathToolKit = toolKit({
	name: "math",
	description: "Math utilities",
	tools: {
		add: hostTool({
			description: "Add two numbers",
			inputSchema: z.object({
				a: z.number(),
				b: z.number(),
			}),
			execute: ({ a, b }) => ({ sum: a + b }),
		}),
	},
});

async function runCommand(vm: AgentOs, command: string, args: string[]) {
	const stdoutChunks: string[] = [];
	const stderrChunks: string[] = [];
	const { pid } = vm.spawn(command, args, {
		onStdout: (chunk) => {
			stdoutChunks.push(new TextDecoder().decode(chunk));
		},
		onStderr: (chunk) => {
			stderrChunks.push(new TextDecoder().decode(chunk));
		},
	});

	return {
		exitCode: await vm.waitProcess(pid),
		stdout: stdoutChunks.join(""),
		stderr: stderrChunks.join(""),
	};
}

describe("native sidecar tool dispatch", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({
			toolKits: [mathToolKit],
		});
	});

	afterEach(async () => {
		await vm?.dispose();
	});

	test("agentos list-tools returns registered toolkits", async () => {
		const result = await runCommand(vm, "agentos", ["list-tools"]);
		expect(result.exitCode).toBe(0);
		expect(JSON.parse(result.stdout)).toEqual({
			ok: true,
			result: {
				toolkits: [
					{
						name: "math",
						description: "Math utilities",
						tools: ["add"],
					},
				],
			},
		});
	});

	test("agentos-<toolkit> executes the tool through the sidecar", async () => {
		const result = await runCommand(vm, "agentos-math", [
			"add",
			"--a",
			"5",
			"--b",
			"7",
		]);
		expect(result.exitCode).toBe(0);
		expect(JSON.parse(result.stdout)).toEqual({
			ok: true,
			result: { sum: 12 },
		});
	});

	test("invalid tool input exits non-zero and writes the error to stderr", async () => {
		const result = await runCommand(vm, "agentos-math", ["add", "--a", "5"]);
		expect(result.exitCode).toBe(1);
		expect(result.stderr).toContain("Missing required flag");
		expect(result.stderr).toContain("--b");
	});
});
