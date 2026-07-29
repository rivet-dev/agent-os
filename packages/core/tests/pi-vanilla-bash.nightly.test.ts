import { resolve } from "node:path";
import common from "@agentos-software/common";
import pi from "@agentos-software/pi";
import type { Fixture, ToolCall } from "@copilotkit/llmock";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");

function getRequestBody(req: unknown): Record<string, unknown> {
	const direct = req as Record<string, unknown>;
	const body = direct.body;
	return body && typeof body === "object"
		? (body as Record<string, unknown>)
		: direct;
}

/**
 * Two-turn fixture: the first model turn (no tool result in the request) emits
 * the bash tool call; the second turn (the request now carries the tool result)
 * returns the final assistant text.
 */
function createBashFixtures(toolCall: ToolCall, finalText: string): Fixture[] {
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
					JSON.stringify(getRequestBody(req)).includes('"role":"tool"'),
			},
			{ content: finalText },
		),
	];
}

function bashToolCall(args: Record<string, unknown>): ToolCall {
	return {
		name: "bash",
		arguments: JSON.stringify(args),
	};
}

async function createPiVm(mockUrl: string): Promise<AgentOs> {
	return AgentOs.create({
		loopbackExemptPorts: [Number(new URL(mockUrl).port)],
		mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
		// Default software ships no agents; project Pi explicitly together with
		// the shell commands used by its unmodified bash backend.
		software: [...common, pi],
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

function captureSessionEventText(
	vm: AgentOs,
	sessionId: string,
): {
	text: () => string;
	unsubscribe: () => void;
} {
	const events: string[] = [];
	const unsubscribe = vm.onSessionEvent(sessionId, (event) => {
		events.push(JSON.stringify(event));
	});
	return {
		text: () => events.join("\n"),
		unsubscribe,
	};
}

function textPrompt(vm: AgentOs, sessionId: string, text: string) {
	return vm.prompt({ sessionId, content: [{ type: "text", text }] });
}

/**
 * Vanilla Pi bash coverage: these tests use the unmodified Pi SDK bash backend
 * (`createLocalBashOperations()` spawning the shell directly with
 * `detached: true` and streaming stdout/stderr), with no custom `operations`
 * override in the adapter. Everything stays inside the VM.
 *
 * The coverage includes shell output, filesystem side effects, timeout-driven
 * process-tree termination, and cancellation of an in-flight command.
 */
describe("vanilla Pi bash tool inside the VM", () => {
	test("runs the vanilla bash backend in the session working directory", async () => {
		const fixtures = createBashFixtures(
			bashToolCall({ command: "pwd", timeout: 10 }),
			"reported the directory.",
		);
		const { mock, url } = await startLlmock(fixtures);
		const vm = await createPiVm(url);

		let sessionId: string | undefined;
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

			const eventText = captureSessionEventText(vm, sessionId);
			const result = await textPrompt(vm, sessionId, "Run pwd.");
			eventText.unsubscribe();
			expect(result.stopReason).toBe("end_turn");
			expect(eventText.text()).toContain(workspaceDir);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	test("inherits session env in the spawned shell", async () => {
		const fixtures = createBashFixtures(
			bashToolCall({ command: "echo $APP_TEST_FLAG", timeout: 10 }),
			"reported the flag.",
		);
		const { mock, url } = await startLlmock(fixtures);
		const vm = await createPiVm(url);

		let sessionId: string | undefined;
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
					APP_TEST_FLAG: "vanilla",
				},
			});

			const eventText = captureSessionEventText(vm, sessionId);
			const result = await textPrompt(
				vm,
				sessionId,
				"Echo the APP_TEST_FLAG variable.",
			);
			eventText.unsubscribe();
			expect(result.stopReason).toBe("end_turn");
			expect(eventText.text()).toContain("vanilla");
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	test("captures stdout, stderr, and the nonzero exit code", async () => {
		const fixtures = createBashFixtures(
			bashToolCall({
				command: "printf 'out-line\\n'; printf 'err-line\\n' 1>&2; exit 3",
				timeout: 10,
			}),
			"the command failed.",
		);
		const { mock, url } = await startLlmock(fixtures);
		const vm = await createPiVm(url);

		let sessionId: string | undefined;
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

			const eventText = captureSessionEventText(vm, sessionId);
			const result = await textPrompt(
				vm,
				sessionId,
				"Run a command that writes to stdout and stderr and exits nonzero.",
			);
			eventText.unsubscribe();
			expect(result.stopReason).toBe("end_turn");
			const events = eventText.text();
			expect(events).toContain("out-line");
			expect(events).toContain("err-line");
			expect(events).toContain("3");
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	test("writes a file through the default bash backend", async () => {
		const fixtures = createBashFixtures(
			bashToolCall({ command: "printf 'ok' > out.txt", timeout: 10 }),
			"out.txt was written.",
		);
		const { mock, url } = await startLlmock(fixtures);
		const vm = await createPiVm(url);

		let sessionId: string | undefined;
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

			const result = await textPrompt(
				vm,
				sessionId,
				"Use bash to write ok into out.txt.",
			);
			expect(result.stopReason).toBe("end_turn");
			expect(
				new TextDecoder().decode(await vm.readFile(`${workspaceDir}/out.txt`)),
			).toBe("ok");
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);

	test("enforces the bash timeout by killing the process tree", async () => {
		const fixtures = createBashFixtures(
			bashToolCall({ command: "sleep 30", timeout: 1 }),
			"the command timed out.",
		);
		const { mock, url } = await startLlmock(fixtures);
		const vm = await createPiVm(url);

		let sessionId: string | undefined;
		const startedAt = Date.now();
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

			const eventText = captureSessionEventText(vm, sessionId);
			const result = await textPrompt(
				vm,
				sessionId,
				"Run sleep 30 with a 1 second timeout.",
			);
			eventText.unsubscribe();
			expect(result.stopReason).toBe("end_turn");
			// The kill must actually fire: completing in seconds (not ~30s) proves
			// the timeout killed the sleep instead of waiting for it to finish.
			expect(Date.now() - startedAt).toBeLessThan(20_000);
			expect(eventText.text().toLowerCase()).toContain("timed out");
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 60_000);

	test("aborts an in-flight bash command on session cancel", async () => {
		const fixtures: Fixture[] = [
			createAnthropicFixture(
				{
					predicate: (req) =>
						!JSON.stringify(getRequestBody(req)).includes('"role":"tool"'),
				},
				{ toolCalls: [bashToolCall({ command: "sleep 60", timeout: 120 })] },
			),
		];
		const { mock, url } = await startLlmock(fixtures);
		const vm = await createPiVm(url);

		let sessionId: string | undefined;
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

			const activeSessionId = sessionId;
			const sawInProgress = new Promise<void>((resolveInProgress) => {
				const unsubscribe = vm.onSessionEvent(activeSessionId, (event) => {
					const serialized = JSON.stringify(event);
					if (
						serialized.includes('"in_progress"') &&
						serialized.includes("bash")
					) {
						unsubscribe();
						resolveInProgress();
					}
				});
			});

			const promptPromise = textPrompt(
				vm,
				activeSessionId,
				"Run sleep 60 in bash.",
			);

			await sawInProgress;
			await vm.cancelPrompt({ sessionId: activeSessionId });

			const result = await promptPromise;
			expect(result.stopReason).toBe("cancelled");

			const lingering = vm
				.allProcesses()
				.filter(
					(proc) =>
						proc.status === "running" &&
						(proc.command.includes("sleep") ||
							proc.args.some((arg) => arg.includes("sleep"))),
				);
			expect(lingering).toEqual([]);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 60_000);
});
