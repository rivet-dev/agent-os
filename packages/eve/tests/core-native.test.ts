import { AgentOs } from "@rivet-dev/agentos-core";
import { describe, expect, it } from "vitest";
import { agentOSCoreBackend } from "../src/index.js";

describe("agentOSCoreBackend native", () => {
	it("drives a real Core VM through Eve's sandbox contract", async () => {
		let vm: AgentOs | undefined;
		const backend = agentOSCoreBackend({
			async create() {
				vm = await AgentOs.create();
				return vm;
			},
		});
		const handle = await backend.create({
			runtimeContext: { appRoot: "/app" },
			sessionKey: "native-core",
			templateKey: null,
		});

		try {
			await handle.session.writeTextFile({
				path: "marker.txt",
				content: "agentos-eve-core",
			});
			await expect(
				handle.session.run({ command: "cat /workspace/marker.txt" }),
			).resolves.toEqual({
				exitCode: 0,
				stdout: "agentos-eve-core",
				stderr: "",
			});
			await expect(handle.captureState()).resolves.toEqual({
				backendName: "agentos-core-v1",
				metadata: { version: 1 },
				sessionKey: "native-core",
			});
		} finally {
			await handle.shutdown();
		}

		await expect(vm?.exists("/workspace/marker.txt")).rejects.toThrow();
	}, 60_000);
});
