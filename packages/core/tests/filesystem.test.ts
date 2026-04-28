import { resolve } from "node:path";
import type { Fixture, ToolCall } from "@copilotkit/llmock";
import claude from "@rivet-dev/agent-os-claude";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";
import { REGISTRY_SOFTWARE } from "./helpers/registry-commands.js";
import { ALLOW_ALL_VM_PERMISSIONS } from "./helpers/permissions.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
function hasToolResult(req: unknown): boolean {
	const directMessages = (
		req as {
			messages?: Array<{ role?: string }>;
			body?: { messages?: Array<{ role?: string }> };
		}
	).messages;
	const bodyMessages = (
		req as { body?: { messages?: Array<{ role?: string }> } }
	).body?.messages;
	const messages = Array.isArray(directMessages)
		? directMessages
		: Array.isArray(bodyMessages)
			? bodyMessages
			: [];
	return messages.some((message) => message.role === "tool");
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

describe("filesystem operations", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({ permissions: ALLOW_ALL_VM_PERMISSIONS });
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("writeFile and readFile round-trip", async () => {
		const content = "hello filesystem";
		await vm.writeFile("/tmp/roundtrip.txt", content);
		const data = await vm.readFile("/tmp/roundtrip.txt");
		expect(new TextDecoder().decode(data)).toBe(content);
	});

	test("writeFile is visible to WASM guest commands", async () => {
		await vm.dispose();
		vm = await AgentOs.create({
			permissions: ALLOW_ALL_VM_PERMISSIONS,
			software: REGISTRY_SOFTWARE,
		});

		await vm.writeFile("/tmp/test.txt", "hello");

		const cat = await vm.exec("cat /tmp/test.txt");
		expect(cat.exitCode, cat.stderr || cat.stdout).toBe(0);
		expect(cat.stdout.trim()).toBe("hello");

		const ls = await vm.exec("ls /tmp/");
		expect(ls.exitCode, ls.stderr || ls.stdout).toBe(0);
		expect(ls.stdout).toContain("test.txt");
	});

	test("agent bash tool writes are visible to readFile before the session exits", async () => {
		const { mock, url } = await startLlmock(
			createToolFixtures(
				{
					name: "Bash",
					arguments: JSON.stringify({
						command: "printf 'agent-shadow-ok' > /tmp/agent-shadow.txt",
					}),
				},
				"done",
			),
		);
		const mockPort = Number(new URL(url).port);

		await vm.dispose();
		vm = await AgentOs.create({
			loopbackExemptPorts: [mockPort],
			moduleAccessCwd: MODULE_ACCESS_CWD,
			permissions: ALLOW_ALL_VM_PERMISSIONS,
			software: [claude, ...REGISTRY_SOFTWARE],
		});

		let sessionId: string | undefined;
		try {
			sessionId = (
				await vm.createSession("claude", {
					cwd: "/home/user",
					env: {
						ANTHROPIC_API_KEY: "mock-key",
						ANTHROPIC_BASE_URL: url,
					},
				})
			).sessionId;
			vm.onPermissionRequest(sessionId, (request) => {
				void vm.respondPermission(sessionId!, request.permissionId, "once");
			});

			const { response } = await vm.prompt(
				sessionId,
				"Use bash to write agent-shadow-ok into /tmp/agent-shadow.txt.",
			);

			expect(response.error).toBeUndefined();
			expect(
				new TextDecoder().decode(await vm.readFile("/tmp/agent-shadow.txt")),
			).toBe("agent-shadow-ok");
		} finally {
			if (sessionId) {
				vm.closeSession(sessionId);
			}
			await stopLlmock(mock);
		}
	});

	test("mkdir and readdir", async () => {
		await vm.mkdir("/tmp/testdir");
		await vm.writeFile("/tmp/testdir/a.txt", "a");
		await vm.writeFile("/tmp/testdir/b.txt", "b");
		const entries = await vm.readdir("/tmp/testdir");
		expect(entries).toContain("a.txt");
		expect(entries).toContain("b.txt");
	});

	test("stat returns file info", async () => {
		await vm.writeFile("/tmp/statfile.txt", "stat me");
		const info = await vm.stat("/tmp/statfile.txt");
		expect(info).toBeDefined();
		expect(info.size).toBeGreaterThan(0);
	});

	test("exists returns true for existing file", async () => {
		await vm.writeFile("/tmp/exists.txt", "here");
		const result = await vm.exists("/tmp/exists.txt");
		expect(result).toBe(true);
	});

	test("exists returns false for missing file", async () => {
		const result = await vm.exists("/tmp/nonexistent-file.txt");
		expect(result).toBe(false);
	});
});
