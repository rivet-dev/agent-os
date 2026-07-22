import { existsSync, mkdtempSync, rmSync } from "node:fs";
import {
	createServer,
	type IncomingMessage,
	type ServerResponse,
} from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import opencode from "@agentos-software/opencode";
import type { Fixture, ToolCall } from "@copilotkit/llmock";
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import {
	createAnthropicFixture,
	DEFAULT_TEXT_FIXTURE,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";
import {
	createVmOpenCodeHome,
	createVmWorkspace,
	OPENCODE_TEST_V8_HEAP_LIMIT_MB,
	readVmText,
} from "./helpers/opencode-helper.js";
import {
	ONE_PIXEL_PNG_BASE64,
	ONE_PIXEL_PNG_BYTES,
	requestContainsExactPng,
	requestContainsExactPngToolResult,
	startRawRequestCapture,
} from "./helpers/rich-media.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const ACP_TRACE_DIR = mkdtempSync(join(tmpdir(), "agentos-opencode-trace-"));
const ACP_TRACE_PATH = join(ACP_TRACE_DIR, "acp.jsonl");
const PREVIOUS_ACP_TRACE_PATH = process.env.AGENT_OS_ACP_TRACE_PATH;

beforeAll(() => {
	// The native sidecar is shared across VMs, so its process environment must
	// be configured before this file creates the first VM.
	process.env.AGENT_OS_ACP_TRACE_PATH = ACP_TRACE_PATH;
});

afterAll(() => {
	if (PREVIOUS_ACP_TRACE_PATH === undefined) {
		delete process.env.AGENT_OS_ACP_TRACE_PATH;
	} else {
		process.env.AGENT_OS_ACP_TRACE_PATH = PREVIOUS_ACP_TRACE_PATH;
	}
	rmSync(ACP_TRACE_DIR, { recursive: true, force: true });
});
const REGISTRY_COMMAND_DIR_CANDIDATES = [
	resolve(
		import.meta.dirname,
		"../../../toolchain/target/wasm32-wasip1/release/commands",
	),
	resolve(
		import.meta.dirname,
		"../../../../agentos/toolchain/target/wasm32-wasip1/release/commands",
	),
];

function findShellCommandDir(): string | null {
	for (const candidate of REGISTRY_COMMAND_DIR_CANDIDATES) {
		if (
			existsSync(candidate) &&
			existsSync(resolve(candidate, "sh")) &&
			existsSync(resolve(candidate, "bash"))
		) {
			return candidate;
		}
	}
	return null;
}

const shellCommandDir = findShellCommandDir();
const shellSoftware = shellCommandDir
	? [
			{
				commandDir: shellCommandDir,
				commands: [
					{ name: "sh", permissionTier: "full" as const },
					{ name: "bash", permissionTier: "full" as const, aliasOf: "sh" },
				],
			},
		]
	: [];
const testWithShell = shellCommandDir ? test : test.skip;

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

function createToolFixtures(toolCall: ToolCall, finalText: string): Fixture[] {
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
				predicate: (req) => hasAnyToolResult(req),
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

	return JSON.parse(
		Buffer.concat(chunks).toString("utf8"),
	) as ChatCompletionsRequestBody;
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
		limits: {
			jsRuntime: { v8HeapLimitMb: OPENCODE_TEST_V8_HEAP_LIMIT_MB },
		},
		mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
		software: [opencode, ...shellSoftware],
	});
}

async function createOpenCodeOnlyVm(mockUrl: string): Promise<AgentOs> {
	return AgentOs.create({
		loopbackExemptPorts: [Number(new URL(mockUrl).port)],
		limits: {
			jsRuntime: { v8HeapLimitMb: OPENCODE_TEST_V8_HEAP_LIMIT_MB },
		},
		software: [opencode],
	});
}

function textPrompt(vm: AgentOs, sessionId: string, text: string) {
	return vm.prompt({ sessionId, content: [{ type: "text", text }] });
}

async function waitForMockRequest(
	mock: { getRequests(): unknown[] },
	predicate: (request: unknown) => boolean,
	label: string,
	timeoutMs = 30_000,
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		if (mock.getRequests().some(predicate)) {
			return;
		}
		await new Promise((resolveDelay) => setTimeout(resolveDelay, 25));
	}
	throw new Error(`timed out waiting for ${label}`);
}

describe("OpenCode session API integration", () => {
	test("full openSession({ agent: 'opencode' }) inside the VM", async () => {
		const { mock, url } = await startLlmock([DEFAULT_TEXT_FIXTURE]);
		const vm = await createOpenCodeVm(url);

		let sessionId: string | undefined;
		try {
			const homeDir = await createVmOpenCodeHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			sessionId = "main";
			await vm.openSession({
				sessionId,
				agent: "opencode",
				permissionPolicy: "ask",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
				},
			});

			const agentInfo = await vm.getSessionAgentInfo({ sessionId });
			expect(agentInfo.name).toBe("OpenCode");
			expect(agentInfo.version).toBeTruthy();

			const capabilities = await vm.getSessionCapabilities({ sessionId });
			expect(capabilities?.prompt).toMatchObject({
				embeddedContext: true,
				image: true,
			});

			const config = await vm.getSessionConfig({ sessionId });
			const modes = config.options.find((option) => option.id === "mode");
			expect(modes?.type).toBe("select");
			if (modes?.type !== "select") throw new Error("missing mode selector");
			expect(modes.currentValue).toBe("build");
			expect(modes.options.map((mode) => mode.value)).toEqual(
				expect.arrayContaining(["build", "plan"]),
			);

			expect(config.options.some((option) => option.category === "model")).toBe(
				true,
			);

			expect((await vm.listSessions()).sessions).toContainEqual(
				expect.objectContaining({ sessionId, agent: "opencode" }),
			);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	test("preserves exact PNG media in OpenCode prompts and read results", async () => {
		const fixtures: Fixture[] = [
			createAnthropicFixture(
				{ userMessage: /Inspect this direct PNG/ },
				{ content: "Direct PNG received." },
			),
			createAnthropicFixture(
				{ predicate: (request) => !hasAnyToolResult(request) },
				{
					toolCalls: [
						{
							name: "read",
							arguments: JSON.stringify({
								filePath: "pixel.png",
							}),
						},
					],
				},
			),
			createAnthropicFixture(
				{ predicate: hasAnyToolResult },
				{ content: "PNG read successfully." },
			),
		];
		const { mock, url } = await startLlmock(fixtures);
		const capture = await startRawRequestCapture(url);
		const vm = await createOpenCodeVm(capture.url);
		const sessionId = "opencode-rich-media";
		let sessionOpened = false;
		let unsubscribe: (() => void) | undefined;
		try {
			const homeDir = await createVmOpenCodeHome(vm, capture.url);
			const workspaceDir = await createVmWorkspace(vm);
			await vm.writeFile(`${workspaceDir}/pixel.png`, ONE_PIXEL_PNG_BYTES);
			await vm.openSession({
				sessionId,
				agent: "opencode",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
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
					{
						type: "resource",
						resource: {
							uri: "file:///workspace/context.txt",
							mimeType: "text/plain",
							text: "embedded context ✓",
						},
					},
				],
			});
			expect(direct.stopReason).toBe("end_turn");
			expect(capture.requests.some(requestContainsExactPng)).toBe(true);
			expect(JSON.stringify(capture.requests)).toContain("embedded context ✓");

			const events: unknown[] = [];
			unsubscribe = vm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const result = await textPrompt(
				vm,
				sessionId,
				"Read pixel.png and inspect the image.",
			);
			unsubscribe();
			unsubscribe = undefined;

			expect(result.stopReason).toBe("end_turn");
			expect(capture.requests.some(requestContainsExactPngToolResult)).toBe(
				true,
			);
			expect(JSON.stringify(capture.requests)).toContain(ONE_PIXEL_PNG_BASE64);
			expect(events.some(requestContainsExactPng)).toBe(true);
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

	test("runs the real OpenCode ACP flow end-to-end for write tool calls", async () => {
		const fixtures = createToolFixtures(
			{
				name: "write",
				arguments: JSON.stringify({
					filePath: "notes.txt",
					content: "hello from tool",
				}),
			},
			"notes.txt was created successfully.",
		);
		const { mock, url } = await startLlmock(fixtures);
		const vm = await createOpenCodeVm(url);

		let sessionId: string | undefined;
		try {
			const homeDir = await createVmOpenCodeHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			sessionId = "main";
			await vm.openSession({
				sessionId,
				agent: "opencode",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
				},
			});

			const agentInfo = await vm.getSessionAgentInfo({ sessionId });
			expect(agentInfo.name).toBe("OpenCode");
			expect(agentInfo.version).toBeTruthy();

			const capabilities = await vm.getSessionCapabilities({ sessionId });
			expect(capabilities?.prompt).toMatchObject({
				embeddedContext: true,
				image: true,
			});

			const config = await vm.getSessionConfig({ sessionId });
			const modes = config.options.find((option) => option.id === "mode");
			expect(modes?.type).toBe("select");
			if (modes?.type !== "select") throw new Error("missing mode selector");
			expect(modes.currentValue).toBe("build");
			expect(modes.options.map((mode) => mode.value)).toEqual(
				expect.arrayContaining(["build", "plan"]),
			);

			expect(config.options.some((option) => option.category === "model")).toBe(
				true,
			);

			const events: unknown[] = [];
			const unsubscribeEvents = vm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const response = await textPrompt(
				vm,
				sessionId,
				"Create notes.txt with the text hello from tool.",
			);
			unsubscribeEvents();

			expect(response.stopReason).toBeDefined();
			expect(await readVmText(vm, `${workspaceDir}/notes.txt`)).toBe(
				"hello from tool",
			);
			expect(mock.getRequests().length).toBeGreaterThanOrEqual(2);

			expect(
				events.some((event) => JSON.stringify(event).includes("tool_call")),
			).toBe(true);
			expect(events.length).toBeGreaterThan(0);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
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
					sessionId = "main";
					await vm.openSession({
						sessionId,
						agent: "opencode",
						cwd: workspaceDir,
						env: {
							HOME: homeDir,
							[providerCase.envName]: "mock-key",
						},
					});

					const response = await textPrompt(
						vm,
						sessionId,
						`Reply with exactly ${providerCase.reply}.`,
					);

					expect(response.stopReason).toBeDefined();
				} finally {
					if (sessionId) {
						await vm.unloadSession({ sessionId });
					}
					await vm.dispose();
				}
			}

			expect(mock.requests.map((request) => request.model)).toEqual(
				expect.arrayContaining(
					providerCases.map((providerCase) => providerCase.modelId),
				),
			);
		} finally {
			await mock.stop();
		}
	}, 120_000);

	test("integrates OpenCode session metadata, plan mode, and lifecycle into the agentOS session API", async () => {
		const { mock, url } = await startLlmock([DEFAULT_TEXT_FIXTURE]);
		const vm = await createOpenCodeVm(url);

		let sessionId: string | undefined;
		try {
			const homeDir = await createVmOpenCodeHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			sessionId = "main";
			await vm.openSession({
				sessionId,
				agent: "opencode",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
				},
			});

			expect((await vm.listSessions()).sessions).toContainEqual(
				expect.objectContaining({ sessionId, agent: "opencode" }),
			);

			const modelOption = (
				await vm.getSessionConfig({ sessionId })
			).options.find((option) => option.category === "model");
			expect(modelOption).toMatchObject({
				id: "model",
				category: "model",
			});
			expect(modelOption?.currentValue).toMatch(
				/^anthropic\/claude-sonnet-4-6(?:\/.+)?$/,
			);

			const setModelResponse = await vm.setSessionConfigOption({
				sessionId,
				configId: "model",
				value: "anthropic/claude-opus-4-6",
			});
			const updatedModel = setModelResponse.options.find(
				(option) => option.id === "model",
			);
			expect(updatedModel?.currentValue).toBe("anthropic/claude-opus-4-6");

			const setModeResponse = await vm.setSessionConfigOption({
				sessionId,
				configId: "mode",
				value: "plan",
			});
			const modeOption = setModeResponse.options.find(
				(option) => option.id === "mode",
			);
			expect(modeOption?.type === "select" && modeOption.currentValue).toBe(
				"plan",
			);

			const promptResponse = await textPrompt(
				vm,
				sessionId,
				"Plan the next step without running tools.",
			);
			expect(promptResponse.stopReason).toBeDefined();
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
			expect(modelsUsed).toContain("claude-opus-4-6");
			expect(modelsUsed).not.toContain("claude-sonnet-4-6");

			const destroyedSessionId = sessionId;
			await vm.deleteSession({ sessionId: destroyedSessionId });
			sessionId = undefined;
			expect((await vm.listSessions()).sessions).not.toContainEqual(
				expect.objectContaining({ sessionId: destroyedSessionId }),
			);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	test("real OpenCode session/load resumes an existing native session", async () => {
		const firstPrompt = "Remember the native resume token: orchid-2718.";
		const secondPrompt = "What native resume token did I give you earlier?";
		const { mock, url } = await startLlmock([
			createAnthropicFixture(
				{
					predicate: (req) => hasUserMessageContaining(req, firstPrompt),
				},
				{ content: "I will remember orchid-2718." },
			),
			createAnthropicFixture(
				{
					predicate: (req) => hasUserMessageContaining(req, secondPrompt),
				},
				{ content: "The token was orchid-2718." },
			),
		]);
		const vm = await createOpenCodeOnlyVm(url);

		let sessionId: string | undefined;
		try {
			const homeDir = await createVmOpenCodeHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			sessionId = "main";
			await vm.openSession({
				sessionId,
				agent: "opencode",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
				},
			});

			const firstResponse = await textPrompt(vm, sessionId, firstPrompt);
			expect(firstResponse.stopReason).toBeDefined();
			await vm.unloadSession({ sessionId });

			const secondResponse = await textPrompt(vm, sessionId, secondPrompt);
			expect(secondResponse.stopReason).toBeDefined();

			const secondRequest = mock
				.getRequests()
				.find((request) => hasUserMessageContaining(request, secondPrompt));
			expect(secondRequest).toBeDefined();
			expect(hasUserMessageContaining(secondRequest, firstPrompt)).toBe(true);
			expect(
				hasUserMessageContaining(
					secondRequest,
					"You are continuing an earlier session",
				),
			).toBe(false);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	test("surfaces OpenCode cancelPrompt() honestly through the agentOS session API", async () => {
		const { mock, url } = await startLlmock([
			{
				match: { predicate: () => true },
				response: {
					content: "This response should outlive the cancel request.",
				},
				latency: 30_000,
			},
		]);
		const vm = await createOpenCodeVm(url);

		let sessionId: string | undefined;
		try {
			const homeDir = await createVmOpenCodeHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			sessionId = "main";
			await vm.openSession({
				sessionId,
				agent: "opencode",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
				},
			});

			const promptPromise = textPrompt(
				vm,
				sessionId,
				"Take a while and then answer.",
			);
			await waitForMockRequest(
				mock,
				(request) =>
					hasUserMessageContaining(request, "Take a while and then answer."),
				"OpenCode cancellation prompt request",
			);

			const cancelResponse = await vm.cancelPrompt({ sessionId });
			expect(cancelResponse.status).toBe("cancelled");

			const promptResponse = await promptPromise;
			expect(promptResponse.stopReason).toBe("cancelled");
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	testWithShell(
		"supports real OpenCode permission approval through the agentOS session API",
		async () => {
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
									command: "echo perm-ok > perm-output.txt",
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
			const permissionParams: Record<string, unknown>[] = [];
			const permissionResponses: Promise<unknown>[] = [];
			try {
				const homeDir = await createVmOpenCodeHome(vm, url, {
					permission: { bash: "ask" },
				});
				const workspaceDir = await createVmWorkspace(vm);
				sessionId = "main";
				await vm.openSession({
					sessionId,
					agent: "opencode",
					permissionPolicy: "ask",
					cwd: workspaceDir,
					env: {
						HOME: homeDir,
						ANTHROPIC_API_KEY: "mock-key",
					},
				});
				const activeSessionId = sessionId;

				vm.onSessionEvent(activeSessionId, (event) => {
					if (event.type !== "permission_request") return;
					permissionIds.push(event.requestId);
					permissionParams.push(event);
					const option = event.options.find(
						(candidate) => candidate.kind === "allow_once",
					);
					if (!option) throw new Error("missing ACP allow_once option");
					permissionResponses.push(
						vm.respondPermission({
							sessionId: activeSessionId,
							requestId: event.requestId,
							optionId: option.optionId,
						}),
					);
				});

				const response = await textPrompt(
					vm,
					sessionId,
					"Use bash to write perm-ok into perm-output.txt.",
				);
				expect(response.stopReason).toBeDefined();
				expect(permissionIds).toHaveLength(1);
				expect(
					(
						permissionParams[0]?.options as
							| Array<{ optionId?: string }>
							| undefined
					)?.map((option) => option.optionId),
				).toEqual(["once", "always", "reject"]);
				await expect(Promise.all(permissionResponses)).resolves.toEqual([
					{ status: "accepted" },
				]);
				expect(await readVmText(vm, `${workspaceDir}/perm-output.txt`)).toBe(
					"perm-ok\n",
				);
			} finally {
				if (sessionId) {
					await vm.unloadSession({ sessionId });
				}
				await vm.dispose();
				await stopLlmock(mock);
			}
		},
		120_000,
	);

	test("supports real OpenCode permission rejection through the agentOS session API", async () => {
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
		const permissionResponses: Promise<unknown>[] = [];
		try {
			const homeDir = await createVmOpenCodeHome(vm, url, {
				permission: { bash: "ask" },
			});
			const workspaceDir = await createVmWorkspace(vm);
			sessionId = "main";
			await vm.openSession({
				sessionId,
				agent: "opencode",
				permissionPolicy: "ask",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
				},
			});
			const activeSessionId = sessionId;

			vm.onSessionEvent(activeSessionId, (event) => {
				if (event.type !== "permission_request") return;
				permissionIds.push(event.requestId);
				const option = event.options.find(
					(candidate) => candidate.kind === "reject_once",
				);
				if (!option) throw new Error("missing ACP reject_once option");
				permissionResponses.push(
					vm.respondPermission({
						sessionId: activeSessionId,
						requestId: event.requestId,
						optionId: option.optionId,
					}),
				);
			});

			const response = await textPrompt(
				vm,
				sessionId,
				"Use bash to write perm-no into perm-output.txt.",
			);
			expect(response.stopReason).toBeDefined();
			expect(permissionIds).toHaveLength(1);
			await expect(Promise.all(permissionResponses)).resolves.toEqual([
				{ status: "accepted" },
			]);
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
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	test("supports native ACP mode changes through the agentOS session API", async () => {
		const { mock, url } = await startLlmock([DEFAULT_TEXT_FIXTURE]);
		const vm = await createOpenCodeVm(url);

		let sessionId: string | undefined;
		try {
			const homeDir = await createVmOpenCodeHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			sessionId = "main";
			await vm.openSession({
				sessionId,
				agent: "opencode",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
				},
			});

			const setPlanResponse = await vm.setSessionConfigOption({
				sessionId,
				configId: "mode",
				value: "plan",
			});
			const planMode = setPlanResponse.options.find(
				(option) => option.id === "mode",
			);
			expect(planMode?.type === "select" && planMode.currentValue).toBe("plan");

			const planPrompt = "Plan once and do not run tools.";
			const planPromptResponse = await textPrompt(vm, sessionId, planPrompt);
			expect(planPromptResponse.stopReason).toBeDefined();

			const buildResponse = await vm.setSessionConfigOption({
				sessionId,
				configId: "mode",
				value: "build",
			});
			const buildMode = buildResponse.options.find(
				(option) => option.id === "mode",
			);
			expect(buildMode?.type === "select" && buildMode.currentValue).toBe(
				"build",
			);

			const buildPrompt = "Answer normally after returning to build mode.";
			const buildPromptResponse = await textPrompt(vm, sessionId, buildPrompt);
			expect(buildPromptResponse.stopReason).toBeDefined();

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
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);
});
