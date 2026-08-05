// Nightly: projects the complete registry command bundle.
import { resolve } from "node:path";
import claude from "@agentos-software/claude-code";
import type { Fixture, LLMock, ToolCall } from "@copilotkit/llmock";
import {
	afterAll,
	afterEach,
	beforeAll,
	beforeEach,
	describe,
	expect,
	test,
} from "vitest";
import { AgentOs } from "../src/agent-os.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";
import { REGISTRY_SOFTWARE } from "./helpers/registry-commands.js";
import {
	ONE_PIXEL_PNG_BASE64,
	ONE_PIXEL_PNG_BYTES,
	requestContainsExactPng,
	requestContainsExactPngToolResult,
	startRawRequestCapture,
} from "./helpers/rich-media.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const XU_COMMAND = "sh -lc 'printf xu-ok:hello-agent-os'";
const XU_OUTPUT = "xu-ok:hello-agent-os";
const NODE_EXECSYNC_CHILD_SCRIPT_PATH = "/tmp/nested-execsync-child.cjs";
const NODE_EXECSYNC_SCRIPT_PATH = "/tmp/nested-execsync.cjs";
const NODE_EXECSYNC_COMMAND = `node ${NODE_EXECSYNC_SCRIPT_PATH}`;
const NODE_EXECSYNC_CHILD_SCRIPT = `
console.log("child-ok");
`.trimStart();
const NODE_EXECSYNC_SCRIPT = `
console.log(
	require("child_process")
		.execSync("node /tmp/nested-execsync-child.cjs")
		.toString()
		.trim(),
);
`.trimStart();
const NODE_ASYNC_SPAWN_SCRIPT_PATH = "/tmp/async-spawn.cjs";
const NODE_ASYNC_SPAWN_COMMAND = `node ${NODE_ASYNC_SPAWN_SCRIPT_PATH}`;
const NODE_ASYNC_SPAWN_OUTPUT = "async-ok";
const NODE_ASYNC_SPAWN_SCRIPT = `
const { spawn } = require("child_process");

const child = spawn("sh", ["-lc", "echo async-ok"], {
	stdio: ["ignore", "pipe", "inherit"],
});

child.stdout.on("data", (chunk) => {
	process.stdout.write(chunk);
});

child.on("close", (code) => {
	process.exit(code ?? 0);
});
`.trimStart();
const TEXT_ONLY_OUTPUT = "plain-text-ok";

function textPrompt(vm: AgentOs, sessionId: string, text: string) {
	return vm.prompt({ sessionId, content: [{ type: "text", text }] });
}

type LlmockMessage = {
	role?: string;
	content?: string | null;
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

function hasToolResult(req: unknown): boolean {
	return getLlmockMessages(req).some((message) => message.role === "tool");
}

function hasToolResultContaining(req: unknown, expected: string): boolean {
	return getLlmockMessages(req).some(
		(message) =>
			message.role === "tool" &&
			typeof message.content === "string" &&
			message.content.includes(expected),
	);
}

function createToolFixtures(toolCall: ToolCall, finalText: string): Fixture[] {
	return [
		createAnthropicFixture(
			{
				predicate: (req) => !hasToolResult(req),
			},
			{ toolCalls: [toolCall] },
		),
		createAnthropicFixture(
			{
				predicate: (req) => hasToolResult(req),
			},
			{ content: finalText },
		),
	];
}

async function writeAsyncSpawnScript(vm: AgentOs): Promise<void> {
	await vm.writeFile(NODE_ASYNC_SPAWN_SCRIPT_PATH, NODE_ASYNC_SPAWN_SCRIPT);
}

async function writeExecSyncScript(vm: AgentOs): Promise<void> {
	await vm.writeFile(
		NODE_EXECSYNC_CHILD_SCRIPT_PATH,
		NODE_EXECSYNC_CHILD_SCRIPT,
	);
	await vm.writeFile(NODE_EXECSYNC_SCRIPT_PATH, NODE_EXECSYNC_SCRIPT);
}

describe("full openSession({ agent: 'claude' })", () => {
	let vm: AgentOs;
	let mock: LLMock;
	let mockUrl: string;
	let mockPort: number;

	beforeAll(async () => {
		const fixtures = createToolFixtures(
			{
				name: "Bash",
				arguments: JSON.stringify({
					command: XU_COMMAND,
				}),
			},
			`xu command executed successfully inside Agent OS: ${XU_OUTPUT}.`,
		);

		const result = await startLlmock(fixtures);
		mock = result.mock;
		mockUrl = result.url;
		mockPort = Number(new URL(result.url).port);
	});

	afterAll(async () => {
		await stopLlmock(mock);
	});

	beforeEach(async () => {
		vm = await AgentOs.create({
			loopbackExemptPorts: [mockPort],
			mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
			software: [claude, ...REGISTRY_SOFTWARE],
		});
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("openSession({ agent: 'claude' }) runs PATH-backed shell commands end-to-end", async () => {
		let sessionId: string | undefined;

		try {
			sessionId = "claude-path-shell";
			await vm.openSession({
				sessionId,
				agent: "claude",
				cwd: "/home/agentos",
				permissionPolicy: "allow_all",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
				},
			});
			const events: unknown[] = [];
			const unsubscribeEvents = vm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const response = await textPrompt(
				vm,
				sessionId,
				`Run ${XU_COMMAND} and tell me what it prints.`,
			);
			unsubscribeEvents();

			expect(response.stopReason).toBe("end_turn");
			expect(
				mock
					.getRequests()
					.some((req) => hasToolResultContaining(req, XU_OUTPUT)),
			).toBe(true);

			expect(events.length).toBeGreaterThanOrEqual(1);
			expect(
				events.some((event) => JSON.stringify(event).includes("tool_call")),
			).toBe(true);
			expect(
				events.some((event) =>
					JSON.stringify(event).includes("agent_message_chunk"),
				),
			).toBe(true);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
		}
	}, 120_000);

	test("preserves exact PNG media in Claude prompts and Read results", async () => {
		const fixtures: Fixture[] = [
			createAnthropicFixture(
				{ userMessage: /Inspect this direct PNG/ },
				{ content: "Direct PNG received." },
			),
			createAnthropicFixture(
				{ predicate: (request) => !hasToolResult(request) },
				{
					toolCalls: [
						{
							name: "Read",
							arguments: JSON.stringify({
								file_path: "/home/agentos/pixel.png",
							}),
						},
					],
				},
			),
			createAnthropicFixture(
				{ predicate: hasToolResult },
				{ content: "Claude preserved the PNG." },
			),
		];
		const { mock: mediaMock, url: mediaMockUrl } = await startLlmock(fixtures);
		const capture = await startRawRequestCapture(mediaMockUrl);
		const mediaVm = await AgentOs.create({
			loopbackExemptPorts: [Number(new URL(capture.url).port)],
			mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
			software: [claude, ...REGISTRY_SOFTWARE],
		});
		const sessionId = "claude-rich-media";
		let sessionOpened = false;
		let unsubscribe: (() => void) | undefined;
		try {
			await mediaVm.writeFile("/home/agentos/pixel.png", ONE_PIXEL_PNG_BYTES);
			await mediaVm.openSession({
				sessionId,
				agent: "claude",
				cwd: "/home/agentos",
				permissionPolicy: "allow_all",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: capture.url,
				},
			});
			sessionOpened = true;
			const direct = await mediaVm.prompt({
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

			const capabilities = await mediaVm.getSessionCapabilities({ sessionId });
			expect(capabilities?.prompt).toMatchObject({
				embeddedContext: true,
				image: true,
			});
			const events: unknown[] = [];
			unsubscribe = mediaVm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const result = await textPrompt(
				mediaVm,
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
				await mediaVm.unloadSession({ sessionId });
			}
			await mediaVm.dispose();
			await capture.stop();
			await stopLlmock(mediaMock);
		}
	}, 120_000);

	test("openSession({ agent: 'claude' }) handles text-only responses without tool calls", async () => {
		const { mock: promptMock, url: promptMockUrl } = await startLlmock([
			createAnthropicFixture({}, { content: TEXT_ONLY_OUTPUT }),
		]);
		const promptMockPort = Number(new URL(promptMockUrl).port);
		const promptVm = await AgentOs.create({
			loopbackExemptPorts: [promptMockPort],
			mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
			software: [claude, ...REGISTRY_SOFTWARE],
		});
		let sessionId: string | undefined;
		try {
			await writeExecSyncScript(promptVm);
			sessionId = "claude-text-only";
			await promptVm.openSession({
				sessionId,
				agent: "claude",
				cwd: "/home/agentos",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: promptMockUrl,
				},
			});

			const events: unknown[] = [];
			const unsubscribeEvents = promptVm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const response = await textPrompt(
				promptVm,
				sessionId,
				`Reply with exactly ${TEXT_ONLY_OUTPUT}.`,
			);
			unsubscribeEvents();

			expect(response.stopReason).toBe("end_turn");
			expect(promptMock.getRequests().length).toBeGreaterThanOrEqual(1);

			expect(
				events.some((event) =>
					JSON.stringify(event).includes("agent_message_chunk"),
				),
			).toBe(true);
			expect(
				events.some((event) => JSON.stringify(event).includes("tool_call")),
			).toBe(false);
		} finally {
			if (sessionId) {
				await promptVm.unloadSession({ sessionId });
			}
			await promptVm.dispose();
			await stopLlmock(promptMock);
		}
	}, 120_000);

	test("openSession({ agent: 'claude' }) runs nested node child_process.execSync() end-to-end", async () => {
		const fixtures = createToolFixtures(
			{
				name: "Bash",
				arguments: JSON.stringify({
					command: NODE_EXECSYNC_COMMAND,
				}),
			},
			"nested node execSync completed successfully inside Agent OS.",
		);
		const { mock: promptMock, url: promptMockUrl } =
			await startLlmock(fixtures);
		const promptMockPort = Number(new URL(promptMockUrl).port);
		const promptVm = await AgentOs.create({
			loopbackExemptPorts: [promptMockPort],
			mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
			software: [claude, ...REGISTRY_SOFTWARE],
		});
		let sessionId: string | undefined;
		try {
			sessionId = "claude-exec-sync";
			await promptVm.openSession({
				sessionId,
				agent: "claude",
				cwd: "/home/agentos",
				permissionPolicy: "allow_all",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: promptMockUrl,
				},
			});
			const events: unknown[] = [];
			const unsubscribeEvents = promptVm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const response = await textPrompt(
				promptVm,
				sessionId,
				`Run ${NODE_EXECSYNC_COMMAND} and tell me what it prints.`,
			);
			unsubscribeEvents();

			expect(response.stopReason).toBe("end_turn");
			expect(promptMock.getRequests().some((req) => hasToolResult(req))).toBe(
				true,
			);

			expect(
				events.some((event) => JSON.stringify(event).includes("tool_call")),
			).toBe(true);
			expect(
				events.some((event) =>
					JSON.stringify(event).includes("agent_message_chunk"),
				),
			).toBe(true);
		} finally {
			if (sessionId) {
				await promptVm.unloadSession({ sessionId });
			}
			await promptVm.dispose();
			await stopLlmock(promptMock);
		}
	}, 120_000);

	test("openSession({ agent: 'claude' }) runs nested node child_process.spawn() end-to-end", async () => {
		const fixtures = createToolFixtures(
			{
				name: "Bash",
				arguments: JSON.stringify({
					command: NODE_ASYNC_SPAWN_COMMAND,
				}),
			},
			`nested node async spawn executed successfully inside Agent OS: ${NODE_ASYNC_SPAWN_OUTPUT}.`,
		);
		const { mock: promptMock, url: promptMockUrl } =
			await startLlmock(fixtures);
		const promptMockPort = Number(new URL(promptMockUrl).port);
		const promptVm = await AgentOs.create({
			loopbackExemptPorts: [promptMockPort],
			mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
			software: [claude, ...REGISTRY_SOFTWARE],
		});
		let sessionId: string | undefined;
		try {
			await writeAsyncSpawnScript(promptVm);
			sessionId = "claude-async-spawn";
			await promptVm.openSession({
				sessionId,
				agent: "claude",
				cwd: "/home/agentos",
				permissionPolicy: "allow_all",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: promptMockUrl,
				},
			});
			const events: unknown[] = [];
			const unsubscribeEvents = promptVm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const response = await textPrompt(
				promptVm,
				sessionId,
				`Run ${NODE_ASYNC_SPAWN_COMMAND} and tell me what it prints.`,
			);
			unsubscribeEvents();

			expect(response.stopReason).toBe("end_turn");
			expect(
				promptMock
					.getRequests()
					.some((req) => hasToolResultContaining(req, NODE_ASYNC_SPAWN_OUTPUT)),
			).toBe(true);

			expect(
				events.some((event) => JSON.stringify(event).includes("tool_call")),
			).toBe(true);
			expect(
				events.some((event) =>
					JSON.stringify(event).includes("agent_message_chunk"),
				),
			).toBe(true);
		} finally {
			if (sessionId) {
				await promptVm.unloadSession({ sessionId });
			}
			await promptVm.dispose();
			await stopLlmock(promptMock);
		}
	}, 120_000);

	test("openSession({ agent: 'claude' }) is integrated into the durable session lifecycle API", async () => {
		let sessionId: string | undefined;

		try {
			sessionId = "claude-lifecycle";
			await vm.openSession({
				sessionId,
				agent: "claude",
				cwd: "/home/agentos",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
				},
			});

			expect((await vm.listSessions()).sessions).toContainEqual(
				expect.objectContaining({ sessionId, agent: "claude" }),
			);

			const agentInfo = await vm.getSessionAgentInfo({ sessionId });
			expect(agentInfo).toMatchObject({
				name: "@agentclientprotocol/claude-agent-acp",
				title: "Claude Agent",
				version: "0.29.2",
			});

			const capabilities = await vm.getSessionCapabilities({ sessionId });
			expect(capabilities?.prompt?.image).toBe(true);
			expect(capabilities?.prompt?.audio).toBeUndefined();
			expect(capabilities?.prompt?.embeddedContext).toBe(true);

			const config = await vm.getSessionConfig({ sessionId });
			expect(config.revision).toBe(0);
			expect(config.options).toEqual(expect.any(Array));
			expect(config.options.some((option) => option.id === "mode")).toBe(true);

			const closedSessionId = sessionId;
			await vm.unloadSession({ sessionId: closedSessionId });
			sessionId = undefined;

			expect((await vm.listSessions()).sessions).toContainEqual(
				expect.objectContaining({
					sessionId: closedSessionId,
					agent: "claude",
				}),
			);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
		}
	}, 120_000);

	test("Claude sessions support cancellation and durable deletion", async () => {
		const sessionId = "claude-cancellation";
		await vm.openSession({
			sessionId,
			agent: "claude",
			cwd: "/home/agentos",
			env: {
				ANTHROPIC_API_KEY: "mock-key",
				ANTHROPIC_BASE_URL: mockUrl,
			},
		});

		const cancelResponse = await vm.cancelPrompt({ sessionId });
		expect(cancelResponse.status).toBe("no_active_prompt");
		expect((await vm.listSessions()).sessions).toContainEqual(
			expect.objectContaining({ sessionId, agent: "claude" }),
		);

		await vm.deleteSession({ sessionId });

		expect((await vm.listSessions()).sessions).not.toContainEqual(
			expect.objectContaining({ sessionId }),
		);
	}, 120_000);

	test("Claude sessions apply native ACP configuration changes", async () => {
		let sessionId: string | undefined;

		try {
			sessionId = "claude-config";
			await vm.openSession({
				sessionId,
				agent: "claude",
				cwd: "/home/agentos",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
				},
			});
			const initialConfig = await vm.getSessionConfig({ sessionId });

			const updatedConfig = await vm.setSessionConfigOption({
				sessionId,
				configId: "mode",
				value: "plan",
			});
			expect(updatedConfig.revision).toBe(initialConfig.revision + 1);
			expect(
				updatedConfig.options.find((option) => option.id === "mode"),
			).toMatchObject({
				type: "select",
				currentValue: "plan",
			});
			expect(await vm.getSessionConfig({ sessionId })).toEqual(updatedConfig);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
		}
	}, 120_000);
});
