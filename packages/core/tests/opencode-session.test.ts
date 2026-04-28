import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { resolve } from "node:path";
import type { Fixture, ToolCall } from "@copilotkit/llmock";
import opencode from "@rivet-dev/agent-os-opencode";
import { describe, expect, test } from "vitest";
import type { AgentCapabilities, AgentInfo } from "../src/agent-os.js";
import { AgentOs } from "../src/agent-os.js";
import {
	createAnthropicFixture,
	DEFAULT_TEXT_FIXTURE,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";
import {
	createVmOpenCodeHome,
	createVmWorkspace,
	readVmText,
} from "./helpers/opencode-helper.js";
import {
	REGISTRY_SOFTWARE,
} from "./helpers/registry-commands.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");

type LlmockMessage = {
	role?: string;
	content?: string | null;
};

type ChatCompletionsRequestBody = Record<string, unknown>;

type ChatCompletionsFixture = {
	name: string;
	predicate: (body: ChatCompletionsRequestBody) => boolean;
	response: Record<string, unknown>;
	delayMs?: number;
};

function getLlmockMessages(req: unknown): LlmockMessage[] {
	const directMessages = (req as { messages?: LlmockMessage[] }).messages;
	if (Array.isArray(directMessages)) {
		return directMessages;
	}

	const bodyMessages = (req as { body?: { messages?: LlmockMessage[] } }).body
		?.messages;
	return Array.isArray(bodyMessages) ? bodyMessages : [];
}

function hasToolResultContaining(req: unknown, expected: string): boolean {
	return getLlmockMessages(req).some(
		(message) =>
			message.role === "tool" &&
			typeof message.content === "string" &&
			message.content.includes(expected),
	);
}

function hasAnyToolResult(req: unknown): boolean {
	return getLlmockMessages(req).some((message) => message.role === "tool");
}

function hasUserMessageContaining(req: unknown, expected: string): boolean {
	return getLlmockMessages(req).some(
		(message) =>
			message.role === "user" &&
			typeof message.content === "string" &&
			message.content.includes(expected),
	);
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
					!getLlmockMessages(req).some((message) => message.role === "tool"),
			},
			{ toolCalls: [toolCall] },
		),
		createAnthropicFixture(
			{
				predicate: (req) => hasToolResultContaining(req, expectedToolResult),
			},
			{ content: finalText },
		),
	];
}

async function readJsonBody(
	req: IncomingMessage,
): Promise<ChatCompletionsRequestBody> {
	const chunks: Buffer[] = [];
	for await (const chunk of req) {
		chunks.push(Buffer.from(chunk));
	}

	return JSON.parse(Buffer.concat(chunks).toString("utf8")) as ChatCompletionsRequestBody;
}

function writeJson(
	res: ServerResponse,
	statusCode: number,
	body: Record<string, unknown>,
): void {
	const payload = JSON.stringify(body);
	res.statusCode = statusCode;
	res.setHeader("content-type", "application/json");
	res.setHeader("content-length", Buffer.byteLength(payload));
	res.end(payload);
}

function createChatCompletionResponse(model: string, content: string) {
	return {
		id: `chatcmpl-${model}`,
		object: "chat.completion",
		created: 1,
		model,
		choices: [
			{
				index: 0,
				message: {
					role: "assistant",
					content,
				},
				finish_reason: "stop",
			},
		],
	};
}

async function startChatCompletionsMock(
	fixtures: ChatCompletionsFixture[],
): Promise<{
	url: string;
	requests: ChatCompletionsRequestBody[];
	stop: () => Promise<void>;
}> {
	const requests: ChatCompletionsRequestBody[] = [];
	const server = createServer(async (req, res) => {
		if (req.method !== "POST" || req.url !== "/chat/completions") {
			writeJson(res, 404, { error: "not_found" });
			return;
		}

		try {
			const body = await readJsonBody(req);
			requests.push(body);

			const fixture = fixtures.find((candidate) => candidate.predicate(body));
			if (!fixture) {
				writeJson(res, 500, {
					error: "no_matching_fixture",
					request: body,
				});
				return;
			}

			if (fixture.delayMs) {
				await new Promise((resolve) => setTimeout(resolve, fixture.delayMs));
			}

			writeJson(res, 200, fixture.response);
		} catch (error) {
			writeJson(res, 500, {
				error: "invalid_request",
				message: error instanceof Error ? error.message : String(error),
			});
		}
	});

	await new Promise<void>((resolve) => {
		server.listen(0, "127.0.0.1", () => resolve());
	});
	server.unref();

	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("chat completions mock did not expose a TCP port");
	}

	return {
		url: `http://127.0.0.1:${address.port}`,
		requests,
		stop: async () => {
			await new Promise<void>((resolve, reject) => {
				server.close((error) => {
					if (error) reject(error);
					else resolve();
				});
			});
		},
	};
}

async function createOpenCodeVm(mockUrl: string): Promise<AgentOs> {
	return AgentOs.create({
		loopbackExemptPorts: [Number(new URL(mockUrl).port)],
		moduleAccessCwd: MODULE_ACCESS_CWD,
		software: [opencode, ...REGISTRY_SOFTWARE],
	});
}

describe("OpenCode session API integration", () => {
	test("full createSession'opencode' inside the VM", async () => {
		const { mock, url } = await startLlmock([DEFAULT_TEXT_FIXTURE]);
		const vm = await createOpenCodeVm(url);

		let sessionId: string | undefined;
		try {
			const homeDir = await createVmOpenCodeHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			sessionId = (
				await vm.createSession("opencode", {
					cwd: workspaceDir,
					env: {
						HOME: homeDir,
						ANTHROPIC_API_KEY: "mock-key",
					},
				})
			).sessionId;

			const agentInfo = vm.getSessionAgentInfo(sessionId) as AgentInfo;
			expect(agentInfo.name).toBe("OpenCode");
			expect(agentInfo.version).toBeTruthy();

			const capabilities = vm.getSessionCapabilities(
				sessionId,
			) as AgentCapabilities;
			expect(capabilities.promptCapabilities).toMatchObject({
				embeddedContext: true,
				image: true,
			});

			const modes = vm.getSessionModes(sessionId);
			expect(modes?.currentModeId).toBe("build");
			expect(modes?.availableModes.map((mode) => mode.id)).toEqual(
				expect.arrayContaining(["build", "plan"]),
			);

			const configOptions = vm.getSessionConfigOptions(sessionId);
			expect(
				configOptions.some((option) => option.category === "model"),
			).toBe(true);

			expect(vm.listSessions()).toContainEqual({
				sessionId,
				agentType: "opencode",
			});
		} finally {
			if (sessionId) {
				vm.closeSession(sessionId);
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	test("runs the real OpenCode ACP flow end-to-end for write tool calls", async () => {
			const fixtures = createToolFixtures(
				{
					name: "write",
					arguments: JSON.stringify({
						filePath: "notes.txt",
						content: "hello from tool",
					}),
				},
				"hello from tool",
				"notes.txt was created successfully.",
			);
			const { mock, url } = await startLlmock(fixtures);
			const vm = await createOpenCodeVm(url);

			let sessionId: string | undefined;
			try {
				const homeDir = await createVmOpenCodeHome(vm, url);
				const workspaceDir = await createVmWorkspace(vm);
				sessionId = (
					await vm.createSession("opencode", {
						cwd: workspaceDir,
						env: {
							HOME: homeDir,
							ANTHROPIC_API_KEY: "mock-key",
						},
					})
				).sessionId;

				const agentInfo = vm.getSessionAgentInfo(sessionId) as AgentInfo;
				expect(agentInfo.name).toBe("OpenCode");
				expect(agentInfo.version).toBeTruthy();

				const capabilities = vm.getSessionCapabilities(
					sessionId,
				) as AgentCapabilities;
				expect(capabilities.promptCapabilities).toMatchObject({
					embeddedContext: true,
					image: true,
				});

				const modes = vm.getSessionModes(sessionId);
				expect(modes?.currentModeId).toBe("build");
				expect(modes?.availableModes.map((mode) => mode.id)).toEqual(
					expect.arrayContaining(["build", "plan"]),
				);

				const configOptions = vm.getSessionConfigOptions(sessionId);
				expect(
					configOptions.some((option) => option.category === "model"),
				).toBe(true);

				const { response } = await vm.prompt(
					sessionId,
					"Create notes.txt with the text hello from tool.",
				);

				expect(response.error).toBeUndefined();
				expect(await readVmText(vm, `${workspaceDir}/notes.txt`)).toBe(
					"hello from tool",
				);
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
				expect(events.length).toBeGreaterThan(0);
			} finally {
				if (sessionId) {
					vm.closeSession(sessionId);
				}
				await vm.dispose();
				await stopLlmock(mock);
			}
	}, 120_000);

	test("supports real OpenCode ACP prompts through Groq and Mistral providers", async () => {
		const providerCases = [
			{
				providerId: "groq",
				modelId: "llama-3.3-70b-versatile",
				envName: "GROQ_API_KEY",
				reply: "groq provider ok",
			},
			{
				providerId: "mistral",
				modelId: "mistral-small-latest",
				envName: "MISTRAL_API_KEY",
				reply: "mistral provider ok",
			},
		] as const;
		const mock = await startChatCompletionsMock(
			providerCases.map((providerCase) => ({
				name: providerCase.providerId,
				predicate: (body) => body.model === providerCase.modelId,
				response: createChatCompletionResponse(
					providerCase.modelId,
					providerCase.reply,
				),
			})),
		);

		try {
			for (const providerCase of providerCases) {
				const vm = await createOpenCodeVm(mock.url);
				let sessionId: string | undefined;
				try {
					const homeDir = await createVmOpenCodeHome(vm, mock.url, {
						model: `${providerCase.providerId}/${providerCase.modelId}`,
						providers: {
							[providerCase.providerId]: {
								options: {
									baseURL: mock.url,
								},
							},
						},
					});
					const workspaceDir = await createVmWorkspace(vm);
					sessionId = (
						await vm.createSession("opencode", {
							cwd: workspaceDir,
							env: {
								HOME: homeDir,
								[providerCase.envName]: "mock-key",
							},
						})
					).sessionId;

					const { response } = await vm.prompt(
						sessionId,
						`Reply with exactly ${providerCase.reply}.`,
					);

					expect(response.error).toBeUndefined();
				} finally {
					if (sessionId) {
						vm.closeSession(sessionId);
					}
					await vm.dispose();
				}
			}

			expect(
				mock.requests.map((request) => request.model),
			).toEqual(
				expect.arrayContaining(providerCases.map((providerCase) => providerCase.modelId)),
			);
		} finally {
			await mock.stop();
		}
	}, 120_000);

	test("integrates OpenCode session metadata, plan mode, and lifecycle into the Agent OS session API", async () => {
			const { mock, url } = await startLlmock([DEFAULT_TEXT_FIXTURE]);
			const vm = await createOpenCodeVm(url);

			let sessionId: string | undefined;
			try {
				const homeDir = await createVmOpenCodeHome(vm, url);
				const workspaceDir = await createVmWorkspace(vm);
				sessionId = (
					await vm.createSession("opencode", {
						cwd: workspaceDir,
						env: {
							HOME: homeDir,
							ANTHROPIC_API_KEY: "mock-key",
						},
					})
				).sessionId;

				expect(vm.listSessions()).toContainEqual({
					sessionId,
					agentType: "opencode",
				});
				expect(vm.resumeSession(sessionId)).toEqual({ sessionId });

				const modelOption = vm
					.getSessionConfigOptions(sessionId)
					.find((option) => option.category === "model");
				expect(modelOption).toMatchObject({
					id: "model",
					category: "model",
					currentValue: "anthropic/claude-sonnet-4-20250514",
					readOnly: true,
				});
				expect(modelOption?.description).toContain("before createSession()");

				const setModelResponse = await vm.setSessionModel(
					sessionId,
					"anthropic/claude-opus-4-1-20250805",
				);
				expect(setModelResponse.error?.message).toContain(
					"configured before createSession()",
				);

				const setModeResponse = await vm.setSessionMode(sessionId, "plan");
				expect(setModeResponse.error).toBeUndefined();
				expect(vm.getSessionModes(sessionId)?.currentModeId).toBe("plan");

				const { response: promptResponse } = await vm.prompt(
					sessionId,
					"Plan the next step without running tools.",
				);
				expect(promptResponse.error).toBeUndefined();
				expect(
					mock
						.getRequests()
						.some((request) =>
							hasUserMessageContaining(request, "Plan Mode - System Reminder"),
						),
				).toBe(true);

				const modelsUsed = mock
					.getRequests()
					.map((request) =>
						request.body && typeof request.body === "object"
							? (request.body as { model?: unknown }).model
							: undefined,
					)
					.filter((model): model is string => typeof model === "string");
				expect(modelsUsed).toContain("claude-sonnet-4-20250514");
				expect(modelsUsed).not.toContain("claude-opus-4-1-20250805");

				const destroyedSessionId = sessionId;
				await vm.destroySession(destroyedSessionId);
				sessionId = undefined;
				expect(vm.listSessions()).not.toContainEqual({
					sessionId: destroyedSessionId,
					agentType: "opencode",
				});
				expect(() => vm.resumeSession(destroyedSessionId)).toThrow(
					"Session not found",
				);
			} finally {
				if (sessionId) {
					vm.closeSession(sessionId);
				}
				await vm.dispose();
				await stopLlmock(mock);
			}
	}, 120_000);

	test("surfaces OpenCode cancelSession() honestly through the Agent OS session API", async () => {
			const { mock, url } = await startLlmock([
				{
					match: { predicate: () => true },
					response: {
						content: "This response should outlive the cancel request.",
					},
					latency: 1_500,
				},
			]);
			const vm = await createOpenCodeVm(url);

			let sessionId: string | undefined;
			try {
				const homeDir = await createVmOpenCodeHome(vm, url);
				const workspaceDir = await createVmWorkspace(vm);
				sessionId = (
					await vm.createSession("opencode", {
						cwd: workspaceDir,
						env: {
							HOME: homeDir,
							ANTHROPIC_API_KEY: "mock-key",
						},
					})
				).sessionId;

				const promptPromise = vm.prompt(
					sessionId,
					"Take a while and then answer.",
				);
				await new Promise((resolveDelay) => setTimeout(resolveDelay, 100));

				const cancelResponse = await vm.cancelSession(sessionId);
				expect(cancelResponse.error).toBeUndefined();
				expect(
					cancelResponse.result as {
						cancelled: boolean;
						requested: boolean;
						via: string;
					},
				).toMatchObject({
					cancelled: false,
					requested: true,
					via: "notification-fallback",
				});

				const promptResponse = await promptPromise;
				expect(promptResponse.error).toBeUndefined();
				expect(promptResponse.result).toBeUndefined();
				expect(
					mock
						.getRequests()
						.some((request) =>
							hasUserMessageContaining(
								request,
								"Take a while and then answer.",
							),
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

	test("supports real OpenCode permission approval through the Agent OS session API", async () => {
			const fixtures = [
				createAnthropicFixture(
					{
						predicate: (req) => !hasAnyToolResult(req),
					},
					{
						toolCalls: [
							{
								name: "bash",
								arguments: JSON.stringify({
									command: "printf 'perm-ok' > perm-output.txt",
									description: "write perm-ok",
								}),
							},
						],
					},
				),
				createAnthropicFixture(
					{
						predicate: (req) => hasAnyToolResult(req),
					},
					{ content: "perm-output.txt was written after approval." },
				),
				createAnthropicFixture(
					{
						predicate: (req) =>
							hasUserMessageContaining(
								req,
								"Generate a title for this conversation:",
							),
					},
					{ content: "Permission approval check" },
				),
			];
			const { mock, url } = await startLlmock(fixtures);
			const vm = await createOpenCodeVm(url);

			let sessionId: string | undefined;
			const permissionIds: string[] = [];
			try {
				const homeDir = await createVmOpenCodeHome(vm, url, {
					permission: { bash: "ask" },
				});
				const workspaceDir = await createVmWorkspace(vm);
				sessionId = (
					await vm.createSession("opencode", {
						cwd: workspaceDir,
						env: {
							HOME: homeDir,
							ANTHROPIC_API_KEY: "mock-key",
						},
					})
				).sessionId;

				vm.onPermissionRequest(sessionId, (request) => {
					permissionIds.push(request.permissionId);
					expect((request.params._acpMethod as string | undefined) ?? "").toBe(
						"session/request_permission",
					);
					expect(
						(
							request.params.options as Array<{ optionId?: string }> | undefined
						)?.map((option) => option.optionId),
					).toEqual(["once", "always", "reject"]);
					void vm.respondPermission(sessionId!, request.permissionId, "once");
				});

				const { response } = await vm.prompt(
					sessionId,
					"Use bash to write perm-ok into perm-output.txt.",
				);
				expect(response.error).toBeUndefined();
				expect(permissionIds).toHaveLength(1);
				expect(await readVmText(vm, `${workspaceDir}/perm-output.txt`)).toBe(
					"perm-ok",
				);
			} finally {
				if (sessionId) {
					vm.closeSession(sessionId);
				}
				await vm.dispose();
				await stopLlmock(mock);
			}
	}, 120_000);

	test("supports real OpenCode permission rejection through the Agent OS session API", async () => {
			const toolCall = {
				name: "bash",
				arguments: JSON.stringify({
					command: "printf 'perm-no' > perm-output.txt",
					description: "write perm-no",
				}),
			};
			const { mock, url } = await startLlmock([
				createAnthropicFixture(
					{
						predicate: (req) =>
							hasUserMessageContaining(
								req,
								"Use bash to write perm-no into perm-output.txt.",
							),
					},
					{ toolCalls: [toolCall] },
				),
				createAnthropicFixture(
					{
						predicate: (req) =>
							hasAnyToolResult(req) &&
							!hasUserMessageContaining(
								req,
								"Generate a title for this conversation:",
							),
					},
					{ content: "Permission rejected. I did not run the bash command." },
				),
				createAnthropicFixture(
					{
						predicate: (req) =>
							hasUserMessageContaining(
								req,
								"Generate a title for this conversation:",
							),
					},
					{ content: "Permission rejection check" },
				),
			]);
			const vm = await createOpenCodeVm(url);

			let sessionId: string | undefined;
			const permissionIds: string[] = [];
			try {
				const homeDir = await createVmOpenCodeHome(vm, url, {
					permission: { bash: "ask" },
				});
				const workspaceDir = await createVmWorkspace(vm);
				sessionId = (
					await vm.createSession("opencode", {
						cwd: workspaceDir,
						env: {
							HOME: homeDir,
							ANTHROPIC_API_KEY: "mock-key",
						},
					})
				).sessionId;

				vm.onPermissionRequest(sessionId, (request) => {
					permissionIds.push(request.permissionId);
					void vm.respondPermission(sessionId!, request.permissionId, "reject");
				});

				const { response } = await vm.prompt(
					sessionId,
					"Use bash to write perm-no into perm-output.txt.",
				);
				expect(response.error).toBeUndefined();
				expect(permissionIds).toHaveLength(1);
				await expect(
					vm.readFile(`${workspaceDir}/perm-output.txt`),
				).rejects.toThrow();
				expect(
					mock
						.getRequests()
						.some((request) =>
							hasUserMessageContaining(
								request,
								"Use bash to write perm-no into perm-output.txt.",
							),
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

	test("supports rawSend() mode changes through the Agent OS session API", async () => {
			const { mock, url } = await startLlmock([DEFAULT_TEXT_FIXTURE]);
			const vm = await createOpenCodeVm(url);

			let sessionId: string | undefined;
			try {
				const homeDir = await createVmOpenCodeHome(vm, url);
				const workspaceDir = await createVmWorkspace(vm);
				sessionId = (
					await vm.createSession("opencode", {
						cwd: workspaceDir,
						env: {
							HOME: homeDir,
							ANTHROPIC_API_KEY: "mock-key",
						},
					})
				).sessionId;

				const receivedEvents: string[] = [];
				const unsubscribe = vm.onSessionEvent(sessionId, (event) => {
					if (event.method !== "session/update") {
						return;
					}
					const serialized = JSON.stringify(event.params);
					if (serialized.includes("current_mode_update")) {
						receivedEvents.push(serialized);
					}
				});

				const setPlanResponse = await vm.setSessionMode(sessionId, "plan");
				expect(setPlanResponse.error).toBeUndefined();
				expect(vm.getSessionModes(sessionId)?.currentModeId).toBe("plan");

				const planPrompt = "Plan once and do not run tools.";
				const { response: planPromptResponse } = await vm.prompt(
					sessionId,
					planPrompt,
				);
				expect(planPromptResponse.error).toBeUndefined();

				const rawBuildResponse = await vm.rawSend(
					sessionId,
					"session/set_mode",
					{
						modeId: "build",
					},
				);
				expect(rawBuildResponse.error).toBeUndefined();
				expect(vm.getSessionModes(sessionId)?.currentModeId).toBe("build");

				const buildPrompt = "Answer normally after returning to build mode.";
				const { response: buildPromptResponse } = await vm.prompt(
					sessionId,
					buildPrompt,
				);
				expect(buildPromptResponse.error).toBeUndefined();

				const modeEvents = vm
					.getSessionEvents(sessionId)
					.map((entry) => entry.notification)
					.filter(
						(event) =>
							event.method === "session/update" &&
							JSON.stringify(event.params).includes("current_mode_update"),
					);
				expect(
					modeEvents.some((event) =>
						JSON.stringify(event.params).includes('"currentModeId":"plan"'),
					),
				).toBe(true);
				expect(
					modeEvents.some((event) =>
						JSON.stringify(event.params).includes('"currentModeId":"build"'),
					),
				).toBe(true);
				expect(
					receivedEvents.some((event) =>
						event.includes('"currentModeId":"plan"'),
					),
				).toBe(true);
				expect(
					receivedEvents.some((event) =>
						event.includes('"currentModeId":"build"'),
					),
				).toBe(true);
				unsubscribe();

				const planRequest = mock
					.getRequests()
					.find((request) => hasUserMessageContaining(request, planPrompt));
				expect(planRequest).toBeDefined();
				expect(
					hasUserMessageContaining(planRequest, "Plan Mode - System Reminder"),
				).toBe(true);

				const buildRequest = mock
					.getRequests()
					.find((request) => hasUserMessageContaining(request, buildPrompt));
				expect(buildRequest).toBeDefined();
				expect(
					hasUserMessageContaining(buildRequest, "Plan Mode - System Reminder"),
				).toBe(false);
			} finally {
				if (sessionId) {
					vm.closeSession(sessionId);
				}
				await vm.dispose();
				await stopLlmock(mock);
			}
	}, 120_000);
});
