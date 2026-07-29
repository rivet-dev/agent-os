import { resolve } from "node:path";
import common from "@agentos-software/common";
import pi from "@agentos-software/pi";
import type { Fixture, ToolCall } from "@copilotkit/llmock";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import type { SessionStreamEntry } from "../src/session-api.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";
import { promptResultText } from "./helpers/session-result.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");

function getRequestBody(req: unknown): Record<string, unknown> {
	const direct = req as Record<string, unknown>;
	const body = direct.body;
	return body && typeof body === "object"
		? (body as Record<string, unknown>)
		: direct;
}

function createToolFixtures(
	toolCall: ToolCall,
	expectedToolResult: string,
	finalText: string,
): Fixture[] {
	return [
		createAnthropicFixture(
			{
				predicate: (req) =>
					!JSON.stringify(getRequestBody(req)).includes('"role":"tool"'),
			},
			{ toolCalls: [toolCall] },
		),
		createAnthropicFixture(
			{
				predicate: (req) =>
					JSON.stringify(getRequestBody(req)).includes('"role":"tool"') &&
					JSON.stringify(getRequestBody(req)).includes(expectedToolResult),
			},
			{ content: finalText },
		),
	];
}

async function createPiVm(mockUrl: string): Promise<AgentOs> {
	return AgentOs.create({
		loopbackExemptPorts: [Number(new URL(mockUrl).port)],
		mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
		software: [common, pi],
	});
}

async function createVmPiHome(vm: AgentOs, mockUrl: string): Promise<string> {
	const homeDir = "/home/agentos";
	await vm.mkdir(`${homeDir}/.pi/agent`, { recursive: true });
	await vm.writeFile(
		`${homeDir}/.pi/agent/models.json`,
		JSON.stringify(
			{
				providers: {
					anthropic: {
						baseUrl: mockUrl,
						apiKey: "mock-key",
					},
				},
			},
			null,
			2,
		),
	);
	return homeDir;
}

async function createVmWorkspace(vm: AgentOs): Promise<string> {
	const workspaceDir = "/home/agentos/workspace";
	await vm.mkdir(workspaceDir, { recursive: true });
	return workspaceDir;
}

describe("pi tool execution (llmock)", () => {
	test("pi executes the write tool and creates a file in the VM without a real API key", async () => {
		const workspacePath = "/home/agentos/workspace/tool-verify.txt";
		const fixtures = createToolFixtures(
			{
				name: "write",
				arguments: JSON.stringify({
					path: workspacePath,
					content: "tool-test-ok",
				}),
			},
			"Successfully wrote",
			"tool-verify.txt was created successfully.",
		);
		const { mock, url } = await startLlmock(fixtures);
		const vm = await createPiVm(url);

		let sessionId: string | undefined;
		try {
			const homeDir = await createVmPiHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			sessionId = "main";
			await vm.openSession({
				sessionId,
				agent: "pi",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: url,
				},
			});

			const events: SessionStreamEntry[] = [];
			const unsubscribeEvents = vm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const result = await vm.prompt({
				sessionId,
				content: [
					{
						type: "text",
						text: "Write the text 'tool-test-ok' to tool-verify.txt. Do not explain, just do it.",
					},
				],
			});
			unsubscribeEvents();

			expect(result.stopReason).toBe("end_turn");
			expect(promptResultText(result)).toContain(
				"tool-verify.txt was created successfully.",
			);
			expect(new TextDecoder().decode(await vm.readFile(workspacePath))).toBe(
				"tool-test-ok",
			);
			expect(mock.getRequests().length).toBeGreaterThanOrEqual(2);

			expect(events.some((event) => event.type === "tool_call")).toBe(true);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);
});
