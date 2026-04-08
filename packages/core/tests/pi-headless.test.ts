import { resolve } from "node:path";
import type { Fixture, ToolCall } from "@copilotkit/llmock";
import common from "@rivet-dev/agent-os-common";
import pi from "@rivet-dev/agent-os-pi";
import { describe, expect, test } from "vitest";
import type { AgentCapabilities, AgentInfo } from "../src/agent-os.js";
import { AgentOs } from "../src/agent-os.js";
import {
	hasRegistryCommands,
} from "./helpers/registry-commands.js";
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

describe("full createSession('pi') inside the VM", () => {
	test("runs the real Pi SDK ACP flow end-to-end for write tool calls", async () => {
		const fixtures = createToolFixtures(
			{
				name: "write",
				arguments: JSON.stringify({
					path: "notes.txt",
					content: "hello from pi write",
				}),
			},
			"Successfully wrote",
			"notes.txt was created successfully.",
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

			const agentInfo = vm.getSessionAgentInfo(sessionId) as AgentInfo;
			expect(agentInfo.name).toBe("pi-sdk-acp");
			expect(agentInfo.title).toBe("Pi SDK ACP adapter");
			expect(agentInfo.version).toBeTruthy();

			const capabilities = vm.getSessionCapabilities(
				sessionId,
			) as AgentCapabilities;
			expect(capabilities.promptCapabilities).toMatchObject({
				image: true,
				audio: false,
				embeddedContext: false,
			});

			const modes = vm.getSessionModes(sessionId);
			expect(modes?.currentModeId).toBeTruthy();
			expect(modes?.availableModes.length).toBeGreaterThan(0);

			const { response, text } = await vm.prompt(
				sessionId,
				"Create notes.txt with the text hello from pi write.",
			);

			expect(response.error).toBeUndefined();
			expect(text).toContain("notes.txt was created successfully.");
			expect(
				new TextDecoder().decode(await vm.readFile(`${workspaceDir}/notes.txt`)),
			).toBe("hello from pi write");
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

	(hasRegistryCommands ? test : test.skip)(
		"runs the real Pi SDK ACP flow end-to-end for bash tool calls",
		async () => {
		const fixtures = createToolFixtures(
			{
				name: "bash",
				arguments: JSON.stringify({
					command: "printf 'bash-ok' > bash-output.txt",
					timeout: 10,
				}),
			},
			"bash-ok",
			"bash-output.txt was written successfully.",
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
				"Use bash to write bash-ok into bash-output.txt.",
			);

			expect(response.error).toBeUndefined();
			expect(text).toContain("bash-output.txt was written successfully.");
			expect(
				new TextDecoder().decode(
					await vm.readFile(`${workspaceDir}/bash-output.txt`),
				),
			).toBe("bash-ok");
			expect(mock.getRequests().length).toBeGreaterThanOrEqual(2);
		} finally {
			if (sessionId) {
				vm.closeSession(sessionId);
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
		},
		120_000,
	);
});
