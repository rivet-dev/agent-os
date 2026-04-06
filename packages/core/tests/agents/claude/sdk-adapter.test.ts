import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import type { LLMock, Fixture, ToolCall } from "@copilotkit/llmock";
import type { ManagedProcess } from "../../../src/runtime-compat.js";
import {
	afterAll,
	afterEach,
	beforeAll,
	beforeEach,
	describe,
	expect,
	test,
} from "vitest";
import { AcpClient } from "../../../src/acp-client.js";
import { AgentOs } from "../../../src/agent-os.js";
import { createStdoutLineIterable } from "../../../src/stdout-lines.js";
import { getAgentOsKernel } from "../../../src/test/runtime.js";
import {
	REGISTRY_SOFTWARE,
	registrySkipReason,
} from "../../helpers/registry-commands.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "../../helpers/llmock-helper.js";

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

function resolveClaudeSdkBinPath(): string {
	const hostPkgJson = join(
		MODULE_ACCESS_CWD,
		"node_modules/@rivet-dev/agent-os-claude/package.json",
	);
	const pkg = JSON.parse(readFileSync(hostPkgJson, "utf-8"));

	let binEntry: string;
	if (typeof pkg.bin === "string") {
		binEntry = pkg.bin;
	} else if (typeof pkg.bin === "object" && pkg.bin !== null) {
		binEntry =
			(pkg.bin as Record<string, string>)["claude-sdk-acp"] ??
			Object.values(pkg.bin)[0];
	} else {
		throw new Error("No bin entry in @rivet-dev/agent-os-claude package.json");
	}

	return `/root/node_modules/@rivet-dev/agent-os-claude/${binEntry}`;
}

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

describe.skipIf(registrySkipReason)("claude-sdk-acp adapter manual spawn", () => {
	let vm: AgentOs;
	let mock: LLMock;
	let mockUrl: string;
	let mockPort: number;
	let client: AcpClient;

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
			software: REGISTRY_SOFTWARE,
		});
	});

	afterEach(async () => {
		if (client) {
			client.close();
		}
		await vm.dispose();
	});

	function spawnClaudeSdkAcp(
		targetVm: AgentOs = vm,
		baseUrl: string = mockUrl,
	): {
		proc: ManagedProcess;
		client: AcpClient;
		stderr: () => string;
	} {
		const binPath = resolveClaudeSdkBinPath();
		const { iterable, onStdout } = createStdoutLineIterable();

		let stderrOutput = "";
		const spawned = getAgentOsKernel(targetVm).spawn("node", [binPath], {
			streamStdin: true,
			onStdout,
			onStderr: (data: Uint8Array) => {
				stderrOutput += new TextDecoder().decode(data);
			},
			env: {
				ANTHROPIC_API_KEY: "mock-key",
				ANTHROPIC_BASE_URL: baseUrl,
				CLAUDE_AGENT_SDK_CLIENT_APP: "agent-os-test",
				CLAUDE_CODE_FORCE_AGENT_OS_RIPGREP: "1",
				CLAUDE_CODE_DEFER_GROWTHBOOK_INIT: "1",
				CLAUDE_CODE_DISABLE_STREAM_JSON_HOOK_EVENTS: "1",
				CLAUDE_CODE_SKIP_SANDBOX_INIT: "1",
				DISABLE_TELEMETRY: "1",
				HOME: "/home/user",
				USE_BUILTIN_RIPGREP: "0",
			},
		});

		const acpClient = new AcpClient(spawned, iterable);
		return { proc: spawned, client: acpClient, stderr: () => stderrOutput };
	}

	test("initialize returns Claude adapter identity", async () => {
		const spawned = spawnClaudeSdkAcp();
		client = spawned.client;

		const response = await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});

		expect(
			response.error,
			`initialize failed: ${spawned.stderr()}`,
		).toBeUndefined();
		const result = response.result as Record<string, unknown>;
		expect(result.protocolVersion).toBe(1);
		expect((result.agentInfo as Record<string, unknown>).name).toBe(
			"claude-sdk-acp",
		);
	}, 60_000);

	test("session/prompt can run PATH-backed xu commands inside Agent OS", async () => {
		const spawned = spawnClaudeSdkAcp();
		client = spawned.client;

		const initResponse = await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		expect(initResponse.error).toBeUndefined();

		const sessionResponse = await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});
		expect(sessionResponse.error).toBeUndefined();
		const sessionId = (
			sessionResponse.result as { sessionId: string }
		).sessionId;

		const notifications: Array<{ method: string; params: unknown }> = [];
		const permissionResponses: Promise<unknown>[] = [];
		client.onNotification((notification) => {
			notifications.push(notification);
			if (notification.method === "request/permission") {
				const params = notification.params as {
					permissionId: string;
				};
				permissionResponses.push(
					client.request("request/permission", {
						sessionId,
						permissionId: params.permissionId,
						reply: "once",
					}),
				);
			}
		});

		const promptResponse = await client.request("session/prompt", {
			sessionId,
			prompt: [
				{
					type: "text",
					text: `Run ${XU_COMMAND} and summarize what it prints.`,
				},
			],
		});

		expect(
			promptResponse.error,
			`prompt failed: ${spawned.stderr()}`,
		).toBeUndefined();
		await Promise.all(permissionResponses);
		expect(
			(promptResponse.result as { stopReason: string }).stopReason,
		).toBe("end_turn");
		expect(
			mock
				.getRequests()
				.some((req) => hasToolResultContaining(req, XU_OUTPUT)),
		).toBe(true);
		expect(
			notifications.some(
				(notification) =>
					notification.method === "session/update" &&
						JSON.stringify(notification.params).includes("tool_call"),
			),
		).toBe(true);
		expect(
			notifications.some(
				(notification) =>
					notification.method === "session/update" &&
					JSON.stringify(notification.params).includes("agent_message_chunk"),
			),
		).toBe(true);
	}, 120_000);

	test("session/prompt can run nested node child_process.execSync() inside Agent OS", async () => {
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
			software: REGISTRY_SOFTWARE,
		});
		const spawned = spawnClaudeSdkAcp(promptVm, promptMockUrl);
		const promptClient = spawned.client;

		try {
			const initResponse = await promptClient.request("initialize", {
				protocolVersion: 1,
				clientCapabilities: {},
			});
			expect(initResponse.error).toBeUndefined();

			const sessionResponse = await promptClient.request("session/new", {
				cwd: "/home/user",
				mcpServers: [],
			});
			expect(sessionResponse.error).toBeUndefined();
			const sessionId = (
				sessionResponse.result as { sessionId: string }
			).sessionId;

			const notifications: Array<{ method: string; params: unknown }> = [];
			const permissionResponses: Promise<unknown>[] = [];
			promptClient.onNotification((notification) => {
				notifications.push(notification);
				if (notification.method === "request/permission") {
					const params = notification.params as {
						permissionId: string;
					};
					permissionResponses.push(
						promptClient.request("request/permission", {
							sessionId,
							permissionId: params.permissionId,
							reply: "once",
						}),
					);
				}
			});

			const promptResponse = await promptClient.request("session/prompt", {
				sessionId,
				prompt: [
					{
						type: "text",
						text: `Run ${NODE_EXECSYNC_COMMAND} and summarize what it prints.`,
					},
				],
			});

			expect(
				promptResponse.error,
				`prompt failed: ${spawned.stderr()}`,
			).toBeUndefined();
			await Promise.all(permissionResponses);
			expect(
				(promptResponse.result as { stopReason: string }).stopReason,
			).toBe("end_turn");
			expect(
				promptMock
					.getRequests()
					.some((req) => hasToolResultContaining(req, NODE_EXECSYNC_OUTPUT)),
			).toBe(true);
			expect(
				notifications.some(
					(notification) =>
						notification.method === "session/update" &&
						JSON.stringify(notification.params).includes("tool_call"),
				),
			).toBe(true);
			expect(
				notifications.some(
					(notification) =>
						notification.method === "session/update" &&
						JSON.stringify(notification.params).includes("agent_message_chunk"),
				),
			).toBe(true);
		} finally {
			promptClient.close();
			await promptVm.dispose();
			await stopLlmock(promptMock);
		}
	}, 120_000);

	test("session/prompt can return text-only responses without tool calls", async () => {
		const { mock: promptMock, url: promptMockUrl } = await startLlmock([
			createAnthropicFixture({}, { content: TEXT_ONLY_OUTPUT }),
		]);
		const promptMockPort = Number(new URL(promptMockUrl).port);
		const promptVm = await AgentOs.create({
			loopbackExemptPorts: [promptMockPort],
			moduleAccessCwd: MODULE_ACCESS_CWD,
			software: REGISTRY_SOFTWARE,
		});
		const spawned = spawnClaudeSdkAcp(promptVm, promptMockUrl);
		const promptClient = spawned.client;

		try {
			const initResponse = await promptClient.request("initialize", {
				protocolVersion: 1,
				clientCapabilities: {},
			});
			expect(initResponse.error).toBeUndefined();

			const sessionResponse = await promptClient.request("session/new", {
				cwd: "/home/user",
				mcpServers: [],
			});
			expect(sessionResponse.error).toBeUndefined();
			const sessionId = (
				sessionResponse.result as { sessionId: string }
			).sessionId;

			const notifications: Array<{ method: string; params: unknown }> = [];
			promptClient.onNotification((notification) => {
				notifications.push(notification);
			});

			const promptResponse = await promptClient.request("session/prompt", {
				sessionId,
				prompt: [
					{
						type: "text",
						text: `Reply with exactly ${TEXT_ONLY_OUTPUT}.`,
					},
				],
			});

			expect(
				promptResponse.error,
				`prompt failed: ${spawned.stderr()}`,
			).toBeUndefined();
			expect(
				(promptResponse.result as { stopReason: string }).stopReason,
			).toBe("end_turn");
			expect(promptMock.getRequests().length).toBeGreaterThanOrEqual(1);
			expect(
				notifications.some(
					(notification) =>
						notification.method === "session/update" &&
						JSON.stringify(notification.params).includes("agent_message_chunk"),
				),
			).toBe(true);
			expect(
				notifications.some(
					(notification) =>
						notification.method === "session/update" &&
						JSON.stringify(notification.params).includes("tool_call"),
				),
			).toBe(false);
		} finally {
			promptClient.close();
			await promptVm.dispose();
			await stopLlmock(promptMock);
		}
	}, 120_000);

	test("session/prompt can run nested node child_process.spawn() inside Agent OS", async () => {
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
			software: REGISTRY_SOFTWARE,
		});
		await writeAsyncSpawnScript(promptVm);
		const spawned = spawnClaudeSdkAcp(promptVm, promptMockUrl);
		const promptClient = spawned.client;

		try {
			const initResponse = await promptClient.request("initialize", {
				protocolVersion: 1,
				clientCapabilities: {},
			});
			expect(initResponse.error).toBeUndefined();

			const sessionResponse = await promptClient.request("session/new", {
				cwd: "/home/user",
				mcpServers: [],
			});
			expect(sessionResponse.error).toBeUndefined();
			const sessionId = (
				sessionResponse.result as { sessionId: string }
			).sessionId;

			const notifications: Array<{ method: string; params: unknown }> = [];
			const permissionResponses: Promise<unknown>[] = [];
			promptClient.onNotification((notification) => {
				notifications.push(notification);
				if (notification.method === "request/permission") {
					const params = notification.params as {
						permissionId: string;
					};
					permissionResponses.push(
						promptClient.request("request/permission", {
							sessionId,
							permissionId: params.permissionId,
							reply: "once",
						}),
					);
				}
			});

			const promptResponse = await promptClient.request("session/prompt", {
				sessionId,
				prompt: [
					{
						type: "text",
						text: `Run ${NODE_ASYNC_SPAWN_COMMAND} and summarize what it prints.`,
					},
				],
			});

			expect(
				promptResponse.error,
				`prompt failed: ${spawned.stderr()}`,
			).toBeUndefined();
			await Promise.all(permissionResponses);
			expect(
				(promptResponse.result as { stopReason: string }).stopReason,
			).toBe("end_turn");
			expect(
				promptMock
					.getRequests()
					.some((req) =>
						hasToolResultContaining(req, NODE_ASYNC_SPAWN_OUTPUT),
					),
			).toBe(true);
			expect(
				notifications.some(
					(notification) =>
						notification.method === "session/update" &&
						JSON.stringify(notification.params).includes("tool_call"),
				),
			).toBe(true);
			expect(
				notifications.some(
					(notification) =>
						notification.method === "session/update" &&
						JSON.stringify(notification.params).includes("agent_message_chunk"),
				),
			).toBe(true);
		} finally {
			promptClient.close();
			await promptVm.dispose();
			await stopLlmock(promptMock);
		}
	}, 120_000);
});
