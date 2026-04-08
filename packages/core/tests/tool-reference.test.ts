import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { z } from "zod";
import { AGENT_CONFIGS } from "../src/agents.js";
import { AgentOs, hostTool, toolKit } from "../src/index.js";
import { getAgentOsKernel } from "../src/test/runtime.js";

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
			examples: [
				{
					description: "Add 1 and 2",
					input: { a: 1, b: 2 },
				},
			],
		}),
	},
});

describe("tool reference registration", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({
			toolKits: [mathToolKit],
		});
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("stores sidecar-generated tool reference markdown on the VM", () => {
		const toolReference = (vm as unknown as { _toolReference: string })
			._toolReference;

		expect(toolReference).toContain("## Available Host Tools");
		expect(toolReference).toContain(
			"Run `agentos list-tools` to see all available tools.",
		);
		expect(toolReference).toContain("### math");
		expect(toolReference).toContain("Math utilities");
		expect(toolReference).toContain(
			"`agentos-math add --a <number> --b <number>`",
		);
		expect(toolReference).toContain("Add 1 and 2");
	});

	test("PI prepareInstructions appends the registered tool reference", async () => {
		const toolReference = (vm as unknown as { _toolReference: string })
			._toolReference;
		const prepare = AGENT_CONFIGS.pi.prepareInstructions;
		expect(prepare).toBeDefined();

		const result = await prepare!(
			getAgentOsKernel(vm),
			"/home/user",
			undefined,
			{ toolReference },
		);
		const argIndex = (result.args ?? []).indexOf("--append-system-prompt");
		expect(argIndex).toBeGreaterThan(-1);
		expect(result.args?.[argIndex + 1]).toContain("## Available Host Tools");
		expect(result.args?.[argIndex + 1]).toContain(
			"`agentos-math add --a <number> --b <number>`",
		);
	});
});
