import { resolve } from "node:path";
import type { Fixture, ToolCall } from "@copilotkit/llmock";
import common from "@rivet-dev/agent-os-common";
import pi from "@rivet-dev/agent-os-pi";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import { hasRegistryCommands } from "./helpers/registry-commands.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";

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
		moduleAccessCwd: MODULE_ACCESS_CWD,
		software: hasRegistryCommands ? [common, pi] : [pi],
	});
}

async function createVmPiHome(vm: AgentOs, mockUrl: string): Promise<string> {
	const homeDir = "/home/user";
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
	const workspaceDir = "/home/user/workspace";
	await vm.mkdir(workspaceDir, { recursive: true });
	return workspaceDir;
}

describe("pi tool execution (llmock)", () => {
	test("pi executes the write tool and creates a file in the VM without a real API key", async () => {
		const workspacePath = "/home/user/workspace/tool-verify.txt";
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
			sessionId = (
				await vm.createSession("pi", {
					cwd: workspaceDir,
					env: {
						HOME: homeDir,
						ANTHROPIC_API_KEY: "mock-key",
						ANTHROPIC_BASE_URL: url,
					},
				})
			).sessionId;

			const { response, text } = await vm.prompt(
				sessionId,
				"Write the text 'tool-test-ok' to tool-verify.txt. Do not explain, just do it.",
			);

			expect(response.error).toBeUndefined();
			expect(text).toContain("tool-verify.txt was created successfully.");
			expect(
				new TextDecoder().decode(await vm.readFile(workspacePath)),
			).toBe("tool-test-ok");
			expect(mock.getRequests().length).toBeGreaterThanOrEqual(2);

			const events = vm
				.getSessionEvents(sessionId)
				.map((event) => event.notification);
			expect(
				events.some(
					(event) =>
						event.method === "session/update" &&
						JSON.stringify(event.params).includes("tool_call"),
				),
			).toBe(true);
			expect(
				events.some(
					(event) =>
						event.method === "session/update" &&
						JSON.stringify(event.params).includes("\"completed\""),
				),
			).toBe(true);
		} finally {
			if (sessionId) {
				vm.closeSession(sessionId);
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);
});
