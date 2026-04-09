import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import { AGENT_CONFIGS } from "../src/agents.js";

describe("listAgents()", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create();
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("returns the shipped built-in agents", () => {
		const agents = vm.listAgents();
		const ids = agents.map((a) => a.id);
		expect(ids).toContain("pi");
		expect(ids).toContain("pi-cli");
		expect(ids).toContain("opencode");
		expect(ids).toContain("claude");
		expect(ids).toContain("codex");
	});

	test("each entry exposes the current built-in adapter metadata", () => {
		const agents = vm.listAgents();
		for (const [id, config] of Object.entries(AGENT_CONFIGS)) {
			const agent = agents.find((entry) => entry.id === id);
			expect(agent).toBeDefined();
			expect(agent?.acpAdapter).toBe(config.acpAdapter);
			expect(agent?.agentPackage).toBe(config.agentPackage);
			expect(typeof agent?.installed).toBe("boolean");
		}
	});

	test("installed is true when adapter package exists", () => {
		const agents = vm.listAgents();
		for (const id of Object.keys(AGENT_CONFIGS)) {
			expect(agents.find((agent) => agent.id === id)?.installed).toBe(true);
		}
	});

	test("installed is false when adapter package is missing", async () => {
		// Create a VM with moduleAccessCwd pointing to a directory without node_modules
		const vm2 = await AgentOs.create({ moduleAccessCwd: "/tmp" });
		try {
			const agents = vm2.listAgents();
			// No packages installed in /tmp
			for (const agent of agents) {
				expect(agent.installed).toBe(false);
			}
		} finally {
			await vm2.dispose();
		}
	});
});
