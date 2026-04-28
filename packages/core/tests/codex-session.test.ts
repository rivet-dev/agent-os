import { resolve } from "node:path";
import codex from "@rivet-dev/agent-os-codex-agent";
import { afterEach, describe, expect, test } from "vitest";
import type { AgentCapabilities, AgentInfo } from "../src/agent-os.js";
import { AgentOs } from "../src/agent-os.js";
import {
	type ResponsesFixture,
	startResponsesMock,
} from "./helpers/openai-responses-mock.js";
import {
	REGISTRY_SOFTWARE,
} from "./helpers/registry-commands.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const XU_COMMAND = "xu hello-agent-os";
const XU_OUTPUT = "xu-ok:hello-agent-os";

type RunningVm = {
	vm: AgentOs;
	stop: () => Promise<void>;
	requests: Record<string, unknown>[];
	url: string;
};

function getInputItems(
	body: Record<string, unknown>,
): Record<string, unknown>[] {
	const input = body.input;
	return Array.isArray(input)
		? input.filter(
				(item): item is Record<string, unknown> =>
					typeof item === "object" && item !== null,
			)
		: [];
}

function hasFunctionCallOutput(
	body: Record<string, unknown>,
	expectedSubstring: string,
): boolean {
	return getInputItems(body).some(
		(item) =>
			item.type === "function_call_output" &&
			typeof item.output === "string" &&
			item.output.includes(expectedSubstring),
	);
}

function hasRoleContent(
	body: Record<string, unknown>,
	role: string,
	expectedSubstring: string,
): boolean {
	return getInputItems(body).some((item) => {
		if (item.role !== role) {
			return false;
		}
		if (typeof item.content === "string") {
			return item.content.includes(expectedSubstring);
		}
		if (Array.isArray(item.content)) {
			return item.content.some(
				(part) =>
					typeof part === "object" &&
					part !== null &&
					(part as { type?: string }).type === "output_text" &&
					typeof (part as { text?: string }).text === "string" &&
					(part as { text: string }).text.includes(expectedSubstring),
			);
		}
		return false;
	});
}

function hasItemType(body: Record<string, unknown>, type: string): boolean {
	return getInputItems(body).some((item) => item.type === type);
}

function hasFunctionCall(
	body: Record<string, unknown>,
	callId: string,
	command: string,
): boolean {
	return getInputItems(body).some((item) => {
		if (item.type !== "function_call" || item.call_id !== callId) {
			return false;
		}
		if (typeof item.arguments !== "string") {
			return false;
		}

		try {
			const parsed = JSON.parse(item.arguments) as { command?: string };
			return parsed.command === command;
		} catch {
			return false;
		}
	});
}

async function createVm(fixtures: ResponsesFixture[]): Promise<RunningVm> {
	const mock = await startResponsesMock(fixtures);
	const vm = await AgentOs.create({
		loopbackExemptPorts: [mock.port],
		moduleAccessCwd: MODULE_ACCESS_CWD,
		software: [codex, ...REGISTRY_SOFTWARE],
	});

	return {
		vm,
		url: mock.url,
		requests: mock.requests,
		stop: async () => {
			await vm.dispose();
			await mock.stop();
		},
	};
}

describe("full createSession('codex')", () => {
	const cleanups = new Set<() => Promise<void>>();

	afterEach(async () => {
		for (const stop of cleanups) {
			await stop();
		}
		cleanups.clear();
	});

	test("codex agent package is discoverable through listAgents()", async () => {
		const vm = await AgentOs.create({
			moduleAccessCwd: MODULE_ACCESS_CWD,
			software: [codex, ...REGISTRY_SOFTWARE],
		});
		cleanups.add(async () => {
			await vm.dispose();
		});

		const agents = vm.listAgents();
		const codexAgent = agents.find((agent) => agent.id === "codex");
		expect(codexAgent).toBeDefined();
		expect(codexAgent?.acpAdapter).toBe("@rivet-dev/agent-os-codex-agent");
		expect(codexAgent?.agentPackage).toBe("@rivet-dev/agent-os-codex");
		expect(codexAgent?.installed).toBe(true);
	});

	test("createSession('codex') runs codex-exec turns end-to-end with permissioned shell tools", async () => {
		const fixtures: ResponsesFixture[] = [
			{
				name: "tool-call",
				predicate: (body) => !hasFunctionCallOutput(body, XU_OUTPUT),
				response: {
					id: "resp_tool",
					output: [
						{
							type: "reasoning",
							id: "rs_1",
							summary: [],
						},
						{
							type: "function_call",
							call_id: "call_shell_1",
							name: "shell",
							arguments: JSON.stringify({ command: XU_COMMAND }),
						},
					],
				},
			},
			{
				name: "final-text",
				predicate: (body) => hasFunctionCallOutput(body, XU_OUTPUT),
				response: {
					id: "resp_text",
					output: [
						{
							type: "message",
							role: "assistant",
							content: [
								{
									type: "output_text",
									text: `xu command executed successfully inside Agent OS: ${XU_OUTPUT}.`,
								},
							],
						},
					],
				},
			},
		];

		const runtime = await createVm(fixtures);
		cleanups.add(runtime.stop);

		const session = await runtime.vm.createSession("codex", {
			cwd: "/home/user",
			env: {
				OPENAI_API_KEY: "mock-key",
				OPENAI_BASE_URL: runtime.url,
			},
		});
		const sessionId = session.sessionId;

		const permissionIds: string[] = [];
		runtime.vm.onPermissionRequest(sessionId, (request) => {
			permissionIds.push(request.permissionId);
			void runtime.vm.respondPermission(
				sessionId,
				request.permissionId,
				"once",
			);
		});

		const { response } = await runtime.vm.prompt(
			sessionId,
			`Run ${XU_COMMAND} and tell me what it prints.`,
		);

		expect(response.error).toBeUndefined();
		expect((response.result as { stopReason?: string }).stopReason).toBe(
			"end_turn",
		);
		expect(permissionIds).toHaveLength(1);
		expect(runtime.requests.length).toBeGreaterThanOrEqual(2);
		expect(
			runtime.requests.some((body) => hasFunctionCallOutput(body, XU_OUTPUT)),
		).toBe(true);
		expect(hasItemType(runtime.requests[1], "reasoning")).toBe(true);
		expect(
			hasFunctionCall(runtime.requests[1], "call_shell_1", XU_COMMAND),
		).toBe(true);

		const events = runtime.vm
			.getSessionEvents(sessionId)
			.map((entry) => entry.notification);
		expect(
			events.some(
				(event) =>
					event.method === "session/update" &&
					JSON.stringify(event.params).includes("tool_call_update"),
			),
		).toBe(true);
		expect(
			events.some(
				(event) =>
					event.method === "session/update" &&
					JSON.stringify(event.params).includes("agent_message_chunk"),
			),
		).toBe(true);

		runtime.vm.closeSession(sessionId);
	}, 120_000);

	test("createSession('codex') executes multiple shell calls from a single model turn", async () => {
		const firstCommand = "xu alpha";
		const secondCommand = "xu beta";
		const firstOutput = "xu-ok:alpha";
		const secondOutput = "xu-ok:beta";

		const runtime = await createVm([
			{
				name: "multi-tool-call",
				predicate: (body) =>
					!hasFunctionCallOutput(body, firstOutput) &&
					!hasFunctionCallOutput(body, secondOutput),
				response: {
					id: "resp_multi_tool",
					output: [
						{
							type: "reasoning",
							id: "rs_multi",
							summary: [],
						},
						{
							type: "function_call",
							call_id: "call_shell_alpha",
							name: "shell",
							arguments: JSON.stringify({ command: firstCommand }),
						},
						{
							type: "function_call",
							call_id: "call_shell_beta",
							name: "shell",
							arguments: JSON.stringify({ command: secondCommand }),
						},
					],
				},
			},
			{
				name: "multi-tool-final",
				predicate: (body) =>
					hasFunctionCallOutput(body, firstOutput) &&
					hasFunctionCallOutput(body, secondOutput),
				response: {
					id: "resp_multi_final",
					output: [
						{
							type: "message",
							role: "assistant",
							content: [
								{
									type: "output_text",
									text: `Both commands completed: ${firstOutput} and ${secondOutput}.`,
								},
							],
						},
					],
				},
			},
		]);
		cleanups.add(runtime.stop);

		const session = await runtime.vm.createSession("codex", {
			cwd: "/home/user",
			env: {
				OPENAI_API_KEY: "mock-key",
				OPENAI_BASE_URL: runtime.url,
			},
		});
		const sessionId = session.sessionId;

		const permissionIds: string[] = [];
		runtime.vm.onPermissionRequest(sessionId, (request) => {
			permissionIds.push(request.permissionId);
			void runtime.vm.respondPermission(
				sessionId,
				request.permissionId,
				"once",
			);
		});

		const { response } = await runtime.vm.prompt(
			sessionId,
			"Run both xu alpha and xu beta, then summarize the outputs.",
		);

		expect(response.error).toBeUndefined();
		expect((response.result as { stopReason?: string }).stopReason).toBe(
			"end_turn",
		);
		expect(permissionIds).toHaveLength(2);
		expect(runtime.requests).toHaveLength(2);
		expect(
			hasFunctionCall(runtime.requests[1], "call_shell_alpha", firstCommand),
		).toBe(true);
		expect(
			hasFunctionCall(runtime.requests[1], "call_shell_beta", secondCommand),
		).toBe(true);
		expect(hasFunctionCallOutput(runtime.requests[1], firstOutput)).toBe(true);
		expect(hasFunctionCallOutput(runtime.requests[1], secondOutput)).toBe(true);

		runtime.vm.closeSession(sessionId);
	}, 120_000);

	test("createSession('codex') exposes session metadata and configurable mode/model state", async () => {
		const runtime = await createVm([
			{
				name: "plan-response",
				predicate: () => true,
				response: {
					id: "resp_plan",
					output: [
						{
							type: "message",
							role: "assistant",
							content: [
								{
									type: "output_text",
									text: "Plan recorded without running tools.",
								},
							],
						},
					],
				},
			},
		]);
		cleanups.add(runtime.stop);

		const session = await runtime.vm.createSession("codex", {
			cwd: "/home/user",
			env: {
				OPENAI_API_KEY: "mock-key",
				OPENAI_BASE_URL: runtime.url,
			},
		});
		const sessionId = session.sessionId;

		expect(runtime.vm.listSessions()).toContainEqual({
			sessionId,
			agentType: "codex",
		});
		expect(runtime.vm.resumeSession(sessionId)).toEqual({ sessionId });

		const agentInfo = runtime.vm.getSessionAgentInfo(sessionId) as AgentInfo;
		expect(agentInfo).toMatchObject({
			name: "codex-wasm-acp",
			title: "Codex WASM ACP adapter",
			version: "0.1.0",
		});

		const capabilities = runtime.vm.getSessionCapabilities(
			sessionId,
		) as AgentCapabilities;
		expect(capabilities).toMatchObject({
			permissions: true,
			plan_mode: true,
			tool_calls: true,
			streaming_deltas: true,
		});
		expect(capabilities.promptCapabilities).toMatchObject({
			audio: false,
			embeddedContext: false,
			image: false,
		});

		expect(runtime.vm.getSessionModes(sessionId)?.currentModeId).toBe(
			"default",
		);
		expect(
			runtime.vm
				.getSessionConfigOptions(sessionId)
				.map((option) => option.category),
		).toEqual(expect.arrayContaining(["model", "thought_level"]));

		await runtime.vm.setSessionModel(sessionId, "gpt-5.4");
		await runtime.vm.setSessionThoughtLevel(sessionId, "high");
		await runtime.vm.setSessionMode(sessionId, "plan");

		expect(runtime.vm.getSessionModes(sessionId)?.currentModeId).toBe("plan");
		const configOptions = runtime.vm.getSessionConfigOptions(sessionId);
		const modelOption = configOptions.find(
			(option) => option.category === "model",
		);
		const thoughtOption = configOptions.find(
			(option) => option.category === "thought_level",
		);
		expect(modelOption?.currentValue).toBe("gpt-5.4");
		expect(thoughtOption?.currentValue).toBe("high");

		const rawResponse = await runtime.vm.rawSend(
			sessionId,
			"session/set_mode",
			{
				modeId: "default",
			},
		);
		expect(rawResponse.error).toBeUndefined();
		expect(runtime.vm.getSessionModes(sessionId)?.currentModeId).toBe(
			"default",
		);
		await runtime.vm.setSessionMode(sessionId, "plan");

		const { response: promptResponse } = await runtime.vm.prompt(
			sessionId,
			"Plan the next step without running shell commands.",
		);
		expect(promptResponse.error).toBeUndefined();

		expect(runtime.requests).toHaveLength(1);
		expect(runtime.requests[0].model).toBe("gpt-5.4");
		expect(
			(runtime.requests[0].reasoning as { effort?: string } | undefined)
				?.effort,
		).toBe("high");
		expect(runtime.requests[0].tools).toEqual([]);

		const modeEvents = runtime.vm
			.getSessionEvents(sessionId)
			.map((entry) => entry.notification)
			.filter(
				(event) =>
					event.method === "session/update" &&
					JSON.stringify(event.params).includes("current_mode_update"),
			);
		expect(modeEvents.length).toBeGreaterThanOrEqual(1);

		const configEvents = runtime.vm
			.getSessionEvents(sessionId)
			.map((entry) => entry.notification)
			.filter(
				(event) =>
					event.method === "session/update" &&
					JSON.stringify(event.params).includes("config_option_update"),
			);
		expect(configEvents.length).toBeGreaterThanOrEqual(2);

		runtime.vm.closeSession(sessionId);
	}, 120_000);

	test("createSession('codex') preserves multi-turn session history across prompts", async () => {
		const firstReply = "First Codex answer.";
		const secondReply = "Second Codex answer that used prior context.";
		const firstPrompt = "Say a short sentence so I can reference it.";
		const secondPrompt = "Repeat what you said previously in one sentence.";

		const runtime = await createVm([
			{
				name: "first-turn",
				predicate: (body) => !hasRoleContent(body, "assistant", firstReply),
				response: {
					id: "resp_first",
					output: [
						{
							type: "message",
							role: "assistant",
							content: [
								{
									type: "output_text",
									text: firstReply,
								},
							],
						},
					],
				},
			},
			{
				name: "second-turn",
				predicate: (body) =>
					hasRoleContent(body, "assistant", firstReply) &&
					hasRoleContent(body, "user", firstPrompt) &&
					hasRoleContent(body, "user", secondPrompt),
				response: {
					id: "resp_second",
					output: [
						{
							type: "message",
							role: "assistant",
							content: [
								{
									type: "output_text",
									text: secondReply,
								},
							],
						},
					],
				},
			},
		]);
		cleanups.add(runtime.stop);

		const session = await runtime.vm.createSession("codex", {
			cwd: "/home/user",
			env: {
				OPENAI_API_KEY: "mock-key",
				OPENAI_BASE_URL: runtime.url,
			},
		});
		const sessionId = session.sessionId;

		const { response: firstResponse } = await runtime.vm.prompt(
			sessionId,
			firstPrompt,
		);
		expect(firstResponse.error).toBeUndefined();
		expect((firstResponse.result as { stopReason?: string }).stopReason).toBe(
			"end_turn",
		);

		const { response: secondResponse } = await runtime.vm.prompt(
			sessionId,
			secondPrompt,
		);
		expect(secondResponse.error).toBeUndefined();
		expect((secondResponse.result as { stopReason?: string }).stopReason).toBe(
			"end_turn",
		);

		expect(runtime.requests).toHaveLength(2);
		expect(hasRoleContent(runtime.requests[1], "user", firstPrompt)).toBe(true);
		expect(hasRoleContent(runtime.requests[1], "assistant", firstReply)).toBe(
			true,
		);
		expect(hasRoleContent(runtime.requests[1], "user", secondPrompt)).toBe(
			true,
		);

		const messageChunks = runtime.vm
			.getSessionEvents(sessionId)
			.map((entry) => entry.notification)
			.filter(
				(event) =>
					event.method === "session/update" &&
					JSON.stringify(event.params).includes("agent_message_chunk"),
			);
		expect(messageChunks.length).toBeGreaterThanOrEqual(2);

		runtime.vm.closeSession(sessionId);
	}, 120_000);

	test("createSession('codex') cleanly cancels a turn when permission is rejected", async () => {
		const runtime = await createVm([
			{
				name: "tool-call",
				predicate: () => true,
				response: {
					id: "resp_tool",
					output: [
						{
							type: "function_call",
							call_id: "call_shell_reject",
							name: "shell",
							arguments: JSON.stringify({ command: XU_COMMAND }),
						},
					],
				},
			},
		]);
		cleanups.add(runtime.stop);

		const session = await runtime.vm.createSession("codex", {
			cwd: "/home/user",
			env: {
				OPENAI_API_KEY: "mock-key",
				OPENAI_BASE_URL: runtime.url,
			},
		});
		const sessionId = session.sessionId;

		runtime.vm.onPermissionRequest(sessionId, (request) => {
			void runtime.vm.respondPermission(
				sessionId,
				request.permissionId,
				"reject",
			);
		});

		const { response } = await runtime.vm.prompt(
			sessionId,
			`Run ${XU_COMMAND} even if permission is denied.`,
		);

		expect(response.error).toBeUndefined();
		expect((response.result as { stopReason?: string }).stopReason).toBe(
			"cancelled",
		);
		expect(runtime.requests).toHaveLength(1);
		expect(
			runtime.requests.some((body) => hasFunctionCallOutput(body, XU_OUTPUT)),
		).toBe(false);

		runtime.vm.closeSession(sessionId);
	}, 120_000);

	test("createSession('codex') supports cancelSession() and destroySession()", async () => {
		const runtime = await createVm([
			{
				name: "slow-response",
				predicate: () => true,
				delayMs: 1_500,
				response: {
					id: "resp_slow",
					output: [
						{
							type: "message",
							role: "assistant",
							content: [
								{
									type: "output_text",
									text: "This response should be cancelled before it completes.",
								},
							],
						},
					],
				},
			},
		]);
		cleanups.add(runtime.stop);

		const session = await runtime.vm.createSession("codex", {
			cwd: "/home/user",
			env: {
				OPENAI_API_KEY: "mock-key",
				OPENAI_BASE_URL: runtime.url,
			},
		});
		const sessionId = session.sessionId;

		const promptPromise = runtime.vm.prompt(
			sessionId,
			"Take a while and then answer.",
		);
		await new Promise((resolve) => setTimeout(resolve, 100));

		const cancelResponse = await runtime.vm.cancelSession(sessionId);
		expect(cancelResponse.error).toBeUndefined();

		const { response: promptResponse } = await promptPromise;
		expect(promptResponse.error).toBeUndefined();
		expect((promptResponse.result as { stopReason?: string }).stopReason).toBe(
			"cancelled",
		);

		await runtime.vm.destroySession(sessionId);
		expect(runtime.vm.listSessions()).not.toContainEqual({
			sessionId,
			agentType: "codex",
		});
		expect(() => runtime.vm.resumeSession(sessionId)).toThrow(
			"Session not found",
		);
	}, 120_000);
});
