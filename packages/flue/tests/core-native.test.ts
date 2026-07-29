import { AgentOs } from "@rivet-dev/agentos-core";
import { describe, expect, it } from "vitest";
import { agentOSCoreSandbox } from "../src/index.js";

describe("agentOSCoreSandbox native", () => {
	it("drives a real Core VM through Flue's sandbox contract", async () => {
		let vm: AgentOs | undefined;
		const sandbox = agentOSCoreSandbox({
			async create() {
				vm = await AgentOs.create({ defaultSoftware: false });
				return vm;
			},
		});
		const env = await sandbox.createSessionEnv({ id: "native-core" });

		try {
			await env.writeFile("marker.txt", "agentos-flue-core");
			await expect(env.readFile("marker.txt")).resolves.toBe(
				"agentos-flue-core",
			);
			await expect(env.exists("marker.txt")).resolves.toBe(true);
			await expect(env.stat("marker.txt")).resolves.toMatchObject({
				isFile: true,
				size: 17,
			});
		} finally {
			await vm?.dispose();
		}
	}, 120_000);
});
