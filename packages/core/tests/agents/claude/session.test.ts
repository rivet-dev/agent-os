import { resolve } from "node:path";
import type { LLMock, Fixture, ToolCall } from "@copilotkit/llmock";
import {
	afterAll,
	afterEach,
	beforeAll,
	beforeEach,
	describe,
	expect,
	test,
} from "vitest";
import claude from "@rivet-dev/agent-os-claude";
import { AgentOs } from "../../../src/agent-os.js";
import type { AgentCapabilities, AgentInfo } from "../../../src/session.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "../../helpers/llmock-helper.js";
import {
	REGISTRY_SOFTWARE,
	registrySkipReason,
} from "../../helpers/registry-commands.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "../../..");
const XU_COMMAND = "xu hello-agent-os";
const XU_OUTPUT = "xu-ok:hello-agent-os";
const NODE_EXECSYNC_COMMAND =
	'node -e "console.log(require(\'child_process\').execSync(\'echo child-ok\').toString().trim())"';
const NODE_EXECSYNC_OUTPUT = "child-ok";
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

function createToolFixtures(
	toolCall: ToolCall,
	finalText: string,
): Fixture[] {
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

describe.skipIf(registrySkipReason)("full createSession('claude')", () => {
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
			moduleAccessCwd: MODULE_ACCESS_CWD,
			software: [claude, ...REGISTRY_SOFTWARE],
		});
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("createSession('claude') runs PATH-backed xu commands end-to-end", async () => {
		let sessionId: string | undefined;

		try {
			const session = await vm.createSession("claude", {
				cwd: "/home/user",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
				},
			});
			sessionId = session.sessionId;
			vm.onPermissionRequest(sessionId, (request) => {
				void vm.respondPermission(sessionId!, request.permissionId, "once");
			});

			const { response } = await vm.prompt(
				sessionId,
				`Run ${XU_COMMAND} and tell me what it prints.`,
			);

			expect(response.error).toBeUndefined();
			expect(
				(response.result as { stopReason?: string }).stopReason,
			).toBe("end_turn");
			expect(
				mock
					.getRequests()
					.some((req) => hasToolResultContaining(req, XU_OUTPUT)),
			).toBe(true);

			const events = vm
				.getSessionEvents(sessionId)
				.map((event) => event.notification);
			expect(events.length).toBeGreaterThanOrEqual(1);
			expect(events[0].method).toBe("session/update");
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
						JSON.stringify(event.params).includes("agent_message_chunk"),
				),
			).toBe(true);
		} finally {
			if (sessionId) {
				vm.closeSession(sessionId);
			}
		}
	}, 120_000);

	test("createSession('claude') handles text-only responses without tool calls", async () => {
		const { mock: promptMock, url: promptMockUrl } = await startLlmock([
			createAnthropicFixture({}, { content: TEXT_ONLY_OUTPUT }),
		]);
		const promptMockPort = Number(new URL(promptMockUrl).port);
		const promptVm = await AgentOs.create({
			loopbackExemptPorts: [promptMockPort],
			moduleAccessCwd: MODULE_ACCESS_CWD,
			software: [claude, ...REGISTRY_SOFTWARE],
		});
		let sessionId: string | undefined;
		try {
			const session = await promptVm.createSession("claude", {
				cwd: "/home/user",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: promptMockUrl,
				},
			});
			sessionId = session.sessionId;

			const { response } = await promptVm.prompt(
				sessionId,
				`Reply with exactly ${TEXT_ONLY_OUTPUT}.`,
			);

			expect(response.error).toBeUndefined();
			expect(
				(response.result as { stopReason?: string }).stopReason,
			).toBe("end_turn");
			expect(promptMock.getRequests().length).toBeGreaterThanOrEqual(1);

			const events = promptVm
				.getSessionEvents(sessionId)
				.map((event) => event.notification);
			expect(
				events.some(
					(event) =>
						event.method === "session/update" &&
						JSON.stringify(event.params).includes("agent_message_chunk"),
				),
			).toBe(true);
			expect(
				events.some(
					(event) =>
						event.method === "session/update" &&
						JSON.stringify(event.params).includes("tool_call"),
				),
			).toBe(false);
		} finally {
			if (sessionId) {
				promptVm.closeSession(sessionId);
			}
			await promptVm.dispose();
			await stopLlmock(promptMock);
		}
	}, 120_000);

	test("createSession('claude') runs nested node child_process.execSync() end-to-end", async () => {
		const fixtures = createToolFixtures(
			{
				name: "Bash",
				arguments: JSON.stringify({
					command: NODE_EXECSYNC_COMMAND,
				}),
			},
			`nested node execSync executed successfully inside Agent OS: ${NODE_EXECSYNC_OUTPUT}.`,
		);
		const { mock: promptMock, url: promptMockUrl } = await startLlmock(fixtures);
		const promptMockPort = Number(new URL(promptMockUrl).port);
		const promptVm = await AgentOs.create({
			loopbackExemptPorts: [promptMockPort],
			moduleAccessCwd: MODULE_ACCESS_CWD,
			software: [claude, ...REGISTRY_SOFTWARE],
		});
		let sessionId: string | undefined;
		try {
			const session = await promptVm.createSession("claude", {
				cwd: "/home/user",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: promptMockUrl,
				},
			});
			sessionId = session.sessionId;
			promptVm.onPermissionRequest(sessionId, (request) => {
				void promptVm.respondPermission(
					sessionId!,
					request.permissionId,
					"once",
				);
			});

			const { response } = await promptVm.prompt(
				sessionId,
				`Run ${NODE_EXECSYNC_COMMAND} and tell me what it prints.`,
			);

			expect(response.error).toBeUndefined();
			expect(
				(response.result as { stopReason?: string }).stopReason,
			).toBe("end_turn");
			expect(
				promptMock
					.getRequests()
					.some((req) => hasToolResultContaining(req, NODE_EXECSYNC_OUTPUT)),
			).toBe(true);

			const events = promptVm
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
						JSON.stringify(event.params).includes("agent_message_chunk"),
				),
			).toBe(true);
		} finally {
			if (sessionId) {
				promptVm.closeSession(sessionId);
			}
			await promptVm.dispose();
			await stopLlmock(promptMock);
		}
	}, 120_000);

	test("createSession('claude') runs nested node child_process.spawn() end-to-end", async () => {
		const fixtures = createToolFixtures(
			{
				name: "Bash",
				arguments: JSON.stringify({
					command: NODE_ASYNC_SPAWN_COMMAND,
				}),
			},
			`nested node async spawn executed successfully inside Agent OS: ${NODE_ASYNC_SPAWN_OUTPUT}.`,
		);
		const { mock: promptMock, url: promptMockUrl } = await startLlmock(fixtures);
		const promptMockPort = Number(new URL(promptMockUrl).port);
		const promptVm = await AgentOs.create({
			loopbackExemptPorts: [promptMockPort],
			moduleAccessCwd: MODULE_ACCESS_CWD,
			software: [claude, ...REGISTRY_SOFTWARE],
		});
		let sessionId: string | undefined;
		try {
			await writeAsyncSpawnScript(promptVm);
			const session = await promptVm.createSession("claude", {
				cwd: "/home/user",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: promptMockUrl,
				},
			});
			sessionId = session.sessionId;
			promptVm.onPermissionRequest(sessionId, (request) => {
				void promptVm.respondPermission(
					sessionId!,
					request.permissionId,
					"once",
				);
			});

			const { response } = await promptVm.prompt(
				sessionId,
				`Run ${NODE_ASYNC_SPAWN_COMMAND} and tell me what it prints.`,
			);

			expect(response.error).toBeUndefined();
			expect(
				(response.result as { stopReason?: string }).stopReason,
			).toBe("end_turn");
			expect(
				promptMock
					.getRequests()
					.some((req) =>
						hasToolResultContaining(req, NODE_ASYNC_SPAWN_OUTPUT),
					),
			).toBe(true);

			const events = promptVm
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
						JSON.stringify(event.params).includes("agent_message_chunk"),
				),
			).toBe(true);
		} finally {
			if (sessionId) {
				promptVm.closeSession(sessionId);
			}
			await promptVm.dispose();
			await stopLlmock(promptMock);
		}
	}, 120_000);

	test("createSession('claude') is integrated into the session metadata and lifecycle API", async () => {
		let sessionId: string | undefined;

		try {
			const session = await vm.createSession("claude", {
				cwd: "/home/user",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
				},
			});
			sessionId = session.sessionId;

			expect(vm.listSessions()).toContainEqual({
				sessionId,
				agentType: "claude",
			});
			expect(vm.resumeSession(sessionId)).toEqual({ sessionId });

			const agentInfo = vm.getSessionAgentInfo(sessionId) as AgentInfo;
			expect(agentInfo).toMatchObject({
				name: "claude-sdk-acp",
				title: "Claude Agent SDK ACP adapter",
				version: "0.1.0",
			});

			const capabilities = vm.getSessionCapabilities(
				sessionId,
			) as AgentCapabilities;
			expect(capabilities.promptCapabilities).toMatchObject({
				audio: false,
				embeddedContext: false,
				image: true,
			});

			const modes = vm.getSessionModes(sessionId);
			expect(modes?.currentModeId).toBe("default");
			expect(modes?.availableModes.map((mode) => mode.id)).toEqual(
				expect.arrayContaining(["default", "plan", "dontAsk"]),
			);
			expect(vm.getSessionConfigOptions(sessionId)).toEqual([]);

			const closedSessionId = sessionId;
			vm.closeSession(closedSessionId);
			sessionId = undefined;

			expect(vm.listSessions()).not.toContainEqual({
				sessionId: closedSessionId,
				agentType: "claude",
			});
			expect(() => vm.resumeSession(closedSessionId)).toThrow(
				"Session not found",
			);
		} finally {
			if (sessionId) {
				vm.closeSession(sessionId);
			}
		}
	}, 120_000);

	test("createSession('claude') supports cancelSession() and destroySession()", async () => {
		const session = await vm.createSession("claude", {
			cwd: "/home/user",
			env: {
				ANTHROPIC_API_KEY: "mock-key",
				ANTHROPIC_BASE_URL: mockUrl,
			},
		});
		const sessionId = session.sessionId;

		const cancelResponse = await vm.cancelSession(sessionId);
		expect(cancelResponse.error).toBeUndefined();
		expect(vm.listSessions()).toContainEqual({
			sessionId,
			agentType: "claude",
		});

		await vm.destroySession(sessionId);

		expect(vm.listSessions()).not.toContainEqual({
			sessionId,
			agentType: "claude",
		});
		expect(() => vm.resumeSession(sessionId)).toThrow("Session not found");
	}, 120_000);

	test("createSession('claude') reflects setSessionMode() through getSessionModes()", async () => {
		let sessionId: string | undefined;

		try {
			const session = await vm.createSession("claude", {
				cwd: "/home/user",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
				},
			});
			sessionId = session.sessionId;

			const response = await vm.setSessionMode(sessionId, "plan");
			expect(response.error).toBeUndefined();

			const modes = vm.getSessionModes(sessionId);
			expect(modes?.currentModeId).toBe("plan");

			const modeEvents = vm
				.getSessionEvents(sessionId)
				.map((event) => event.notification)
				.filter(
					(event) =>
						event.method === "session/update" &&
						JSON.stringify(event.params).includes("current_mode_update"),
				);
			expect(modeEvents.length).toBeGreaterThanOrEqual(1);
		} finally {
			if (sessionId) {
				vm.closeSession(sessionId);
			}
		}
	}, 120_000);

	test("createSession('claude') supports rawSessionSend() for supported ACP methods", async () => {
		let sessionId: string | undefined;

		try {
			const session = await vm.createSession("claude", {
				cwd: "/home/user",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
				},
			});
			sessionId = session.sessionId;

			const response = await vm.rawSessionSend(sessionId, "session/set_mode", {
				modeId: "plan",
			});
			expect(response.error).toBeUndefined();

			const modes = vm.getSessionModes(sessionId);
			expect(modes?.currentModeId).toBe("plan");
		} finally {
			if (sessionId) {
				vm.closeSession(sessionId);
			}
		}
	}, 120_000);
});
