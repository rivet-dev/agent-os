import pi from "@agentos-software/pi";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";

describe("Pi package projection", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({ defaultSoftware: false });
		await vm.linkSoftware(pi);
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("projects the standard Pi ACP adapter and native Pi CLI", async () => {
		expect(await vm.providedCommands()).toEqual(
			expect.arrayContaining([
				expect.objectContaining({ commands: expect.arrayContaining(["pi", "pi-acp"]) }),
			]),
		);
		expect(await vm.listAgents()).toEqual(
			expect.arrayContaining([expect.objectContaining({ id: "pi", installed: true })]),
		);
		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("pi", ["--version"], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode, stderr).toBe(0);
		expect(stdout).toContain("0.80.6");
	});
});
