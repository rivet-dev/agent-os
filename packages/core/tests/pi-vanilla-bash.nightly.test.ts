import { resolve } from "node:path";
import common from "@agentos-software/common";
import pi from "@agentos-software/pi";
import type { ToolCall } from "@copilotkit/llmock";
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

function withTimeout<T>(
	promise: Promise<T>,
	timeoutMs: number,
	label: string,
): Promise<T> {
	let timeout: ReturnType<typeof setTimeout> | undefined;
	const timeoutPromise = new Promise<never>((_resolve, reject) => {
		timeout = setTimeout(
			() => reject(new Error(`timed out waiting for ${label}`)),
			timeoutMs,
		);
		timeout.unref?.();
	});
	return Promise.race([promise, timeoutPromise]).finally(() => {
		if (timeout) clearTimeout(timeout);
	});
}

function processRunsCommand(
	process: ReturnType<AgentOs["allProcesses"]>[number],
	command: string,
): boolean {
	return (
		process.status === "running" &&
		(process.command.includes(command) ||
			process.args.some((arg) => arg.includes(command)))
	);
}

async function waitForMockRequest(
	mock: { getRequests(): unknown[] },
	timeoutMs: number,
	label: string,
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		if (mock.getRequests().length > 0) {
			return;
		}
		await new Promise((resolveDelay) => setTimeout(resolveDelay, 25));
	}
	throw new Error(`timed out waiting for ${label}`);
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
			const startedAt = Date.now();
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
	}, 120_000);

	test("aborts an in-flight bash command on session cancel", async () => {
		const heartbeatPath = "/tmp/pi-cancel-heartbeat";
		const fixtures = createBashFixtures(
			bashToolCall({
				command:
					`printf 'tick\\n' >> ${heartbeatPath}; printf 'started\\n'; ` +
					`while :; do printf 'tick\\n' >> ${heartbeatPath}; sleep 1; done`,
				timeout: 120,
			}),
			"the command should have been cancelled.",
		);
		const { mock, url } = await startLlmock(fixtures);
		const vm = await createPiVm(url);

		let sessionId: string | undefined;
		try {
			const homeDir = await createVmPiHome(vm, url);
			const workspaceDir = await createVmWorkspace(vm);
			const requestedSessionId = "main";
			await vm.openSession({
				sessionId: requestedSessionId,
				agent: "pi",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: url,
				},
			});
			sessionId = requestedSessionId;

			const activeSessionId = sessionId;
			const promptOutcome = textPrompt(
				vm,
				activeSessionId,
				"Run sleep 60 in bash.",
			).then(
				(result) => ({ status: "resolved" as const, result }),
				(error: unknown) => ({ status: "rejected" as const, error }),
			);

			// Pi does not publish its tool-call progress event until this
			// long-running command returns. The mock request is independent of
			// the occupied session lane and proves the tool fixture was delivered.
			await waitForMockRequest(mock, 30_000, "Pi model request");
			await new Promise((resolveDelay) => setTimeout(resolveDelay, 2_000));
			const cancelResponse = await withTimeout(
				vm.cancelPrompt({ sessionId: activeSessionId }),
				15_000,
				"Pi cancel response",
			);
			expect(cancelResponse.status).toBe("cancelled");

			const outcome = await withTimeout(
				promptOutcome,
				15_000,
				"Pi prompt cancellation",
			);
			if (outcome.status === "rejected") {
				throw outcome.error;
			}
			const result = outcome.result;
			expect(result.stopReason).toBe("cancelled");

			await new Promise((resolveDelay) => setTimeout(resolveDelay, 1_200));
			const heartbeatAfterCancel = await vm.readFile(heartbeatPath);
			await new Promise((resolveDelay) => setTimeout(resolveDelay, 1_200));
			expect(await vm.readFile(heartbeatPath)).toEqual(heartbeatAfterCancel);

			const lingering = vm
				.allProcesses()
				.filter((process) => processRunsCommand(process, "sleep"));
			expect(lingering).toEqual([]);
		} finally {
			if (sessionId) {
				await vm.unloadSession({ sessionId });
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);
});
