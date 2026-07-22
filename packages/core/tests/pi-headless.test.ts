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
import { hasBuiltRegistryCommands } from "./helpers/registry-command-availability.js";
import {
	ONE_PIXEL_PNG_BASE64,
	ONE_PIXEL_PNG_BYTES,
	requestContainsExactPng,
	requestContainsExactPngToolResult,
	startRawRequestCapture,
} from "./helpers/rich-media.js";
import { promptResultText } from "./helpers/session-result.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const registryCommandsAvailable = hasBuiltRegistryCommands(common);
const registryCommandTest = registryCommandsAvailable ? test : test.skip;

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
		// Default software ships no agents; pass the pi agent package explicitly.
		software: [...(registryCommandsAvailable ? common : []), pi],
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

describe("full openSession({ agent: 'pi' }) inside the VM", () => {
	test("projected Pi CLI answers an RPC request while stdin remains open", async () => {
		const { mock, url } = await startLlmock([]);
		const vm = await createPiVm(url);
		let pid: number | undefined;
		try {
			const homeDir = await createVmPiHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			let stdout = "";
			let stderr = "";
			const response = new Promise<Record<string, unknown>>(
				(resolve, reject) => {
					const timeout = setTimeout(() => {
						reject(
							new Error(
								`timed out waiting for Pi RPC response; stdout=${JSON.stringify(stdout)} stderr=${JSON.stringify(stderr)}`,
							),
						);
					}, 60_000);
					const spawned = vm.spawn("pi", ["--mode", "rpc", "--no-themes"], {
						cwd: workspaceDir,
						env: {
							HOME: homeDir,
							ANTHROPIC_API_KEY: "mock-key",
							ANTHROPIC_BASE_URL: url,
							PI_SKIP_VERSION_CHECK: "1",
						},
						streamStdin: true,
						onStdout: (chunk) => {
							stdout += new TextDecoder().decode(chunk);
							const line = stdout
								.split("\n")
								.find((candidate) =>
									candidate.includes('"id":"agentos-probe"'),
								);
							if (!line) return;
							clearTimeout(timeout);
							resolve(JSON.parse(line) as Record<string, unknown>);
						},
						onStderr: (chunk) => {
							stderr += new TextDecoder().decode(chunk);
						},
					});
					pid = spawned.pid;
				},
			);
			if (pid === undefined) throw new Error("Pi RPC process did not start");
			await vm.writeProcessStdin(
				pid,
				`${JSON.stringify({ type: "get_state", id: "agentos-probe" })}\n`,
			);
			await expect(response).resolves.toMatchObject({
				id: "agentos-probe",
				type: "response",
				success: true,
			});
		} finally {
			if (pid !== undefined) {
				vm.killProcess(pid);
				await vm.waitProcess(pid);
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 90_000);

	test("openSession({ agent: 'pi' }) initializes over the default native sidecar transport", async () => {
		const { mock, url } = await startLlmock([]);
		const vm = await createPiVm(url);

		let sessionId: string | undefined;
		let sessionOpened = false;
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
					PI_SKIP_VERSION_CHECK: "1",
				},
			});
			sessionOpened = true;

			expect(sessionId).toBeTruthy();
			expect(
				(await vm.listSessions()).sessions.some(
					(entry) => entry.sessionId === sessionId,
				),
			).toBe(true);
		} finally {
			if (sessionOpened && sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	test("preserves exact PNG media in Pi prompts and read results", async () => {
		const fixtures: Fixture[] = [
			createAnthropicFixture(
				{ userMessage: /Inspect this direct PNG/ },
				{ content: "Direct PNG received." },
			),
			createAnthropicFixture(
				{
					predicate: (request) =>
						!JSON.stringify(getRequestBody(request)).includes('"role":"tool"'),
				},
				{
					toolCalls: [
						{
							name: "read",
							arguments: JSON.stringify({
								path: "/home/agentos/workspace/pixel.png",
							}),
						},
					],
				},
			),
			createAnthropicFixture(
				{
					predicate: (request) =>
						JSON.stringify(getRequestBody(request)).includes('"role":"tool"'),
				},
				{ content: "Pi preserved the PNG." },
			),
		];
		const { mock, url } = await startLlmock(fixtures);
		const capture = await startRawRequestCapture(url);
		const vm = await createPiVm(capture.url);
		const sessionId = "pi-rich-media";
		let sessionOpened = false;
		let unsubscribe: (() => void) | undefined;
		try {
			const homeDir = await createVmPiHome(vm, capture.url);
			const workspaceDir = await createVmWorkspace(vm);
			await vm.writeFile(`${workspaceDir}/pixel.png`, ONE_PIXEL_PNG_BYTES);
			await vm.openSession({
				sessionId,
				agent: "pi",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: capture.url,
				},
			});
			sessionOpened = true;
			const direct = await vm.prompt({
				sessionId,
				content: [
					{ type: "text", text: "Inspect this direct PNG." },
					{
						type: "image",
						data: ONE_PIXEL_PNG_BASE64,
						mimeType: "image/png",
					},
				],
			});
			expect(direct.stopReason).toBe("end_turn");
			expect(capture.requests.some(requestContainsExactPng)).toBe(true);

			const events: SessionStreamEntry[] = [];
			unsubscribe = vm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const result = await vm.prompt({
				sessionId,
				content: [
					{
						type: "text",
						text: "Read pixel.png and inspect the image.",
					},
				],
			});
			unsubscribe();
			unsubscribe = undefined;

			expect(result.stopReason).toBe("end_turn");
			expect(capture.requests.some(requestContainsExactPngToolResult)).toBe(
				true,
			);
			expect(JSON.stringify(capture.requests)).toContain(ONE_PIXEL_PNG_BASE64);
			// pi-acp currently preserves image tool results on the provider path but
			// advertises no embedded-context capability for structured ACP output.
			expect(events.some(requestContainsExactPng)).toBe(false);
		} finally {
			unsubscribe?.();
			if (sessionOpened) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await capture.stop();
			await stopLlmock(mock);
		}
	}, 120_000);

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
		let sessionOpened = false;
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
			sessionOpened = true;

			const agentInfo = await vm.getSessionAgentInfo({ sessionId });
			expect(agentInfo.name).toBe("pi-acp");
			expect(agentInfo.title).toBe("pi ACP adapter");
			expect(agentInfo.version).toBeTruthy();

			const capabilities = await vm.getSessionCapabilities({ sessionId });
			expect(capabilities.prompt?.image).toBe(true);
			expect(capabilities.prompt?.audio).toBeUndefined();
			expect(capabilities.prompt?.embeddedContext).toBeUndefined();

			const config = await vm.getSessionConfig({ sessionId });
			// Pi currently advertises legacy ACP `modes`, not native
			// `configOptions`; agentOS deliberately does not invent a mapping.
			expect(config.options.some((option) => option.id === "mode")).toBe(false);

			const events: SessionStreamEntry[] = [];
			const unsubscribeEvents = vm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const result = await vm.prompt({
				sessionId,
				content: [
					{
						type: "text",
						text: "Create notes.txt with the text hello from pi write.",
					},
				],
			});
			unsubscribeEvents();

			expect(result.stopReason).toBe("end_turn");
			expect(promptResultText(result)).toContain(
				"notes.txt was created successfully.",
			);
			expect(
				new TextDecoder().decode(
					await vm.readFile(`${workspaceDir}/notes.txt`),
				),
			).toBe("hello from pi write");
			expect(mock.getRequests().length).toBeGreaterThanOrEqual(2);

			expect(events.some((event) => event.type === "tool_call")).toBe(true);
		} finally {
			if (sessionOpened && sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	registryCommandTest(
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
			let sessionOpened = false;
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
				sessionOpened = true;

				const result = await vm.prompt({
					sessionId,
					content: [
						{
							type: "text",
							text: "Use bash to write bash-ok into bash-output.txt.",
						},
					],
				});

				expect(result.stopReason).toBe("end_turn");
				expect(promptResultText(result)).toContain(
					"bash-output.txt was written successfully.",
				);
				expect(
					new TextDecoder().decode(
						await vm.readFile(`${workspaceDir}/bash-output.txt`),
					),
				).toBe("bash-ok");
				expect(mock.getRequests().length).toBeGreaterThanOrEqual(2);
			} finally {
				if (sessionOpened && sessionId) {
					await vm.unloadSession({ sessionId });
				}
				await vm.dispose();
				await stopLlmock(mock);
			}
		},
		120_000,
	);
});
