import common from "@rivet-dev/agent-os-common";
import { afterEach, describe, expect, test } from "vitest";
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

const duplicateMathToolKit = toolKit({
	name: "math",
	description: "Duplicate math utilities",
	tools: {
		multiply: hostTool({
			description: "Multiply two numbers",
			inputSchema: z.object({
				a: z.number(),
				b: z.number(),
			}),
			execute: ({ a, b }) => ({ product: a * b }),
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

describe("toolkit permissions", () => {
	let vm: AgentOs | null = null;

	afterEach(async () => {
		await vm?.dispose();
		vm = null;
	});

	test("rejects duplicate toolkit registration with a conflict", async () => {
		await expect(
			AgentOs.create({
				toolKits: [mathToolKit, duplicateMathToolKit],
			}),
		).rejects.toThrow(/conflict: toolkit already registered: math/);
	});

	test("denies toolkit invocation by default until tool permissions are granted", async () => {
		vm = await AgentOs.create({
			software: [common],
			toolKits: [mathToolKit],
			permissions: {
				fs: "allow",
				childProcess: "allow",
			},
		});

		const result = await runCommand(vm, "agentos-math", [
			"add",
			"--a",
			"5",
			"--b",
			"7",
		]);
		expect(result.exitCode).toBe(1);
		expect(result.stdout).toBe("");
		expect(result.stderr).toContain("tool.invoke");
		expect(result.stderr).toContain("math:add");
	});

	test("allows toolkit invocation when a matching tool permission is granted", async () => {
		vm = await AgentOs.create({
			software: [common],
			toolKits: [mathToolKit],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				tool: {
					default: "deny",
					rules: [
						{
							mode: "allow",
							operations: ["invoke"],
							patterns: ["math:add"],
						},
					],
				},
			},
		});

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
});
