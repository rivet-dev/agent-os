import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const ANTHROPIC_API_KEY = process.env.ANTHROPIC_API_KEY;

describe.skipIf(!ANTHROPIC_API_KEY)("pi tool execution (real API)", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({
			moduleAccessCwd: MODULE_ACCESS_CWD,
		});
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("pi executes write tool and creates a file in the VM", async () => {
		const { sessionId } = await vm.createSession("pi", {
			env: { ANTHROPIC_API_KEY: ANTHROPIC_API_KEY! },
		});

		const toolEvents: Array<Record<string, unknown>> = [];
		vm.onSessionEvent(sessionId, (event) => {
			const p = event.params as Record<string, unknown> | undefined;
			const u = p?.update as Record<string, unknown> | undefined;
			if (
				u?.sessionUpdate === "tool_call" ||
				u?.sessionUpdate === "tool_call_update"
			) {
				toolEvents.push(u);
			}
		});

		const { response, text } = await vm.prompt(
			sessionId,
			"Write the text 'tool-test-ok' to /tmp/tool-verify.txt. Do not explain, just do it.",
		);

		expect(response.error).toBeUndefined();

		// Verify Pi executed a tool (write or bash)
		const completedEvents = toolEvents.filter((e) => e.status === "completed");
		expect(completedEvents.length).toBeGreaterThanOrEqual(1);

		// Verify the file was actually written inside the VM
		const fileContent = await vm.readFile("/tmp/tool-verify.txt");
		expect(new TextDecoder().decode(fileContent)).toContain("tool-test-ok");

		vm.closeSession(sessionId);
	}, 120_000);
});
