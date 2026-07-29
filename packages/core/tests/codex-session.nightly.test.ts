// Nightly: projects the complete registry command bundle.
import { resolve } from "node:path";
import codex from "@agentos-software/codex";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";
import { afterEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import { REGISTRY_SOFTWARE } from "./helpers/registry-commands.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");

describe("Codex agent availability", () => {
	const cleanups = new Set<() => Promise<void>>();

	afterEach(async () => {
		for (const stop of cleanups) {
			await stop();
		}
		cleanups.clear();
	});

	test("codex package provides commands without registering a runnable ACP agent", async () => {
		const vm = await AgentOs.create({
			mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
			software: [codex, ...REGISTRY_SOFTWARE],
		});
		cleanups.add(async () => {
			await vm.dispose();
		});

		expect((await vm.listAgents()).some((agent) => agent.id === "codex")).toBe(
			false,
		);
		await expect(vm.openSession({ agent: "codex" })).rejects.toThrow(
			/no projected .*codex.*agent\.acpEntrypoint/,
		);
	});
});
