// Nightly: projects the complete registry command bundle.
import { resolve } from "node:path";
import claude from "@agentos-software/claude-code";
import type { Fixture, ToolCall } from "@copilotkit/llmock";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { AgentOs } from "../src/index.js";
import { getAgentOsKernel } from "../src/test/runtime.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";
import { ALLOW_ALL_VM_PERMISSIONS } from "./helpers/permissions.js";
import { REGISTRY_SOFTWARE } from "./helpers/registry-commands.js";

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

	// Regression guard: `mkdir(path, { recursive: true })` must NOT probe each
	// ancestor with a read-side `exists()`. On the native sidecar every read-side op
	// triggers a full shadow-tree walk, so a per-component exists() loop made
	// `mkdir -p` cost O(components * tree) -- a major source of session-creation
	// latency on populated VMs. The recursive kernel mkdir is sufficient on its own.
	test("recursive mkdir issues one mkdir and zero exists() probes", async () => {
		const kernel = getAgentOsKernel(vm);
		const deepPath = "/tmp/mkdirp-no-walk/a/b/c/d/e";
		const existsSpy = vi.spyOn(kernel, "exists");
		const mkdirSpy = vi.spyOn(kernel, "mkdir");

		await vm.mkdir(deepPath, { recursive: true });

		expect(existsSpy).not.toHaveBeenCalled();
		expect(mkdirSpy).toHaveBeenCalledTimes(1);
		expect(mkdirSpy).toHaveBeenCalledWith(deepPath);

		existsSpy.mockRestore();
		mkdirSpy.mockRestore();

		// Behavior is preserved: every intermediate dir exists and is writable, and
		// repeating the call is a no-op (idempotent).
		await vm.writeFile(`${deepPath}/leaf.txt`, "ok");
		expect(
			new TextDecoder().decode(await vm.readFile(`${deepPath}/leaf.txt`)),
		).toBe("ok");
		await vm.mkdir(deepPath, { recursive: true });
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
			mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
			permissions: ALLOW_ALL_VM_PERMISSIONS,
			software: [...REGISTRY_SOFTWARE, claude],
		});

		let sessionId: string | undefined;
		try {
			const requestedSessionId = "main";
			await vm.openSession({
				sessionId: requestedSessionId,
				agent: "claude",
				cwd: "/home/agentos",
				permissionPolicy: "allow_all",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: url,
				},
			});
			sessionId = requestedSessionId;
			const response = await vm.prompt({
				sessionId,
				content: [
					{
						type: "text",
						text: "Use bash to write agent-shadow-ok into /tmp/agent-shadow.txt.",
					},
				],
			});

			expect(response.stopReason).toBeDefined();
			expect(
				new TextDecoder().decode(await vm.readFile("/tmp/agent-shadow.txt")),
			).toBe("agent-shadow-ok");
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await stopLlmock(mock);
		}
	}, 120_000);

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
