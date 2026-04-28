import { resolve } from "node:path";
import type { Fixture, ToolCall } from "@copilotkit/llmock";
import common from "@rivet-dev/agent-os-common";
import piCli from "@rivet-dev/agent-os-pi-cli";
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

async function createPiCliVm(mockUrl: string): Promise<AgentOs> {
	return AgentOs.create({
		loopbackExemptPorts: [Number(new URL(mockUrl).port)],
		moduleAccessCwd: MODULE_ACCESS_CWD,
		software: hasRegistryCommands ? [common, piCli] : [piCli],
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

describe("full createSession('pi-cli') inside the VM", () => {
	test("runs the unmodified Pi CLI ACP flow end-to-end for write tool calls", async () => {
		const workspacePath = "/home/user/workspace/notes.txt";
		const fixtures = createToolFixtures(
			{
				name: "write",
				arguments: JSON.stringify({
					path: workspacePath,
					content: "hello from pi cli write",
				}),
			},
			"Successfully wrote",
			"notes.txt was created successfully.",
		);
		const { mock, url } = await startLlmock(fixtures);
		const vm = await createPiCliVm(url);

		let sessionId: string | undefined;
		try {
			const homeDir = await createVmPiHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			sessionId = (
				await vm.createSession("pi-cli", {
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
				`Create ${workspacePath} with the text hello from pi cli write.`,
			);

			expect(response.error).toBeUndefined();
			expect(text).toContain("notes.txt was created successfully.");
			expect(
				new TextDecoder().decode(await vm.readFile(workspacePath)),
			).toBe("hello from pi cli write");
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

	test("runs the unmodified Pi CLI ACP flow end-to-end for bash tool calls", async () => {
			const workspacePath = "/home/user/workspace/bash-output.txt";
			const fixtures = createToolFixtures(
				{
					name: "bash",
					arguments: JSON.stringify({
						command: `printf 'bash-ok' > ${workspacePath}`,
						timeout: 10,
					}),
				},
				"bash-ok",
				"bash-output.txt was written successfully.",
			);
			const { mock, url } = await startLlmock(fixtures);
			const vm = await createPiCliVm(url);

			let sessionId: string | undefined;
			try {
				const homeDir = await createVmPiHome(vm, url);
				const workspaceDir = await createVmWorkspace(vm);
				sessionId = (
					await vm.createSession("pi-cli", {
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
					`Use bash to write bash-ok into ${workspacePath}.`,
				);

				expect(response.error).toBeUndefined();
				expect(text).toContain("bash-output.txt was written successfully.");
				expect(
					new TextDecoder().decode(await vm.readFile(workspacePath)),
				).toBe("bash-ok");
				expect(mock.getRequests().length).toBeGreaterThanOrEqual(2);
			} finally {
				if (sessionId) {
					vm.closeSession(sessionId);
				}
				await vm.dispose();
				await stopLlmock(mock);
			}
	}, 120_000);
});
