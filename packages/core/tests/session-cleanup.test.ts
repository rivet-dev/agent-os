import { execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { readlink, readdir } from "node:fs/promises";
import type { AddressInfo, Socket } from "node:net";
import { resolve } from "node:path";
import claude from "@rivet-dev/agent-os-claude";
import codex from "@rivet-dev/agent-os-codex-agent";
import opencode from "@rivet-dev/agent-os-opencode";
import pi from "@rivet-dev/agent-os-pi";
import piCli from "@rivet-dev/agent-os-pi-cli";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import { getAgentOsKernel } from "../src/test/runtime.js";
import type { SidecarSessionState } from "../src/sidecar/rpc-client.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";
import {
	createVmOpenCodeHome,
	createVmWorkspace as createOpenCodeWorkspace,
} from "./helpers/opencode-helper.js";
import {
	type ResponsesFixture,
	startResponsesMock,
} from "./helpers/openai-responses-mock.js";
import {
	REGISTRY_SOFTWARE,
	registrySkipReason,
} from "./helpers/registry-commands.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const PROMPT_TEXT = "Reply with exactly cleanup-ok.";
const PROMPT_RESPONSE = "cleanup-ok";

type MockKind = "anthropic" | "openai";

type SessionCleanupAgent = {
	agentType: string;
	label: string;
	mockKind: MockKind;
	activePromptTermination: "close" | "cancel_then_close";
	activePromptMock: "hang" | "slow_response";
	createVm: (mockUrl: string) => Promise<AgentOs>;
	createSession: (vm: AgentOs, mockUrl: string) => Promise<{ sessionId: string }>;
};

const PI_AGENTS: SessionCleanupAgent[] = [
	{
		agentType: "pi",
		label: "Pi SDK",
		mockKind: "anthropic",
		activePromptTermination: "close",
		activePromptMock: "hang",
		createVm: async (mockUrl) =>
			AgentOs.create({
				loopbackExemptPorts: [Number(new URL(mockUrl).port)],
				moduleAccessCwd: MODULE_ACCESS_CWD,
				software: [pi],
			}),
		createSession: async (vm, mockUrl) => {
			const homeDir = await createVmPiHome(vm, mockUrl);
			const workspaceDir = await createVmPiWorkspace(vm);
			return vm.createSession("pi", {
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
					PI_SKIP_VERSION_CHECK: "1",
				},
			});
		},
	},
	{
		agentType: "pi-cli",
		label: "Pi CLI",
		mockKind: "anthropic",
		activePromptTermination: "close",
		activePromptMock: "hang",
		createVm: async (mockUrl) =>
			AgentOs.create({
				loopbackExemptPorts: [Number(new URL(mockUrl).port)],
				moduleAccessCwd: MODULE_ACCESS_CWD,
				software: [piCli],
			}),
		createSession: async (vm, mockUrl) => {
			const homeDir = await createVmPiHome(vm, mockUrl);
			const workspaceDir = await createVmPiWorkspace(vm);
			return vm.createSession("pi-cli", {
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
					PI_SKIP_VERSION_CHECK: "1",
				},
			});
		},
	},
];

const REGISTRY_AGENTS: SessionCleanupAgent[] = [
	{
		agentType: "claude",
		label: "Claude",
		mockKind: "anthropic",
		activePromptTermination: "close",
		activePromptMock: "hang",
		createVm: async (mockUrl) =>
			AgentOs.create({
				loopbackExemptPorts: [Number(new URL(mockUrl).port)],
				moduleAccessCwd: MODULE_ACCESS_CWD,
				software: [claude, ...REGISTRY_SOFTWARE],
			}),
		createSession: async (vm, mockUrl) =>
			vm.createSession("claude", {
				cwd: "/home/user",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
				},
			}),
	},
	{
		agentType: "opencode",
		label: "OpenCode",
		mockKind: "anthropic",
		activePromptTermination: "close",
		activePromptMock: "hang",
		createVm: async (mockUrl) =>
			AgentOs.create({
				loopbackExemptPorts: [Number(new URL(mockUrl).port)],
				moduleAccessCwd: MODULE_ACCESS_CWD,
				software: [opencode, ...REGISTRY_SOFTWARE],
			}),
		createSession: async (vm, mockUrl) => {
			const homeDir = await createVmOpenCodeHome(vm, mockUrl);
			const workspaceDir = await createOpenCodeWorkspace(vm);
			return vm.createSession("opencode", {
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
				},
			});
		},
	},
	{
		agentType: "codex",
		label: "Codex",
		mockKind: "openai",
		activePromptTermination: "cancel_then_close",
		activePromptMock: "slow_response",
		createVm: async (mockUrl) =>
			AgentOs.create({
				loopbackExemptPorts: [Number(new URL(mockUrl).port)],
				moduleAccessCwd: MODULE_ACCESS_CWD,
				software: [codex, ...REGISTRY_SOFTWARE],
			}),
		createSession: async (vm, mockUrl) =>
			vm.createSession("codex", {
				cwd: "/home/user",
				env: {
					OPENAI_API_KEY: "mock-key",
					OPENAI_BASE_URL: mockUrl,
				},
			}),
	},
];

async function waitFor<T>(
	read: () => Promise<T> | T,
	options?: {
		timeoutMs?: number;
		intervalMs?: number;
		isReady?: (value: T) => boolean;
	},
): Promise<T> {
	const timeoutMs = options?.timeoutMs ?? 20_000;
	const intervalMs = options?.intervalMs ?? 50;
	const isReady = options?.isReady ?? ((value: T) => Boolean(value));
	const deadline = Date.now() + timeoutMs;
	let lastValue = await read();
	while (!isReady(lastValue)) {
		if (Date.now() >= deadline) {
			throw new Error("timed out waiting for expected state");
		}
		await new Promise((resolve) => setTimeout(resolve, intervalMs));
		lastValue = await read();
	}
	return lastValue;
}

async function createVmPiHome(vm: AgentOs, mockUrl: string): Promise<string> {
	const homeDir = "/home/user";
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

async function createVmPiWorkspace(vm: AgentOs): Promise<string> {
	const workspaceDir = "/home/user/workspace";
	await vm.mkdir(workspaceDir, { recursive: true });
	return workspaceDir;
}

type SidecarBackdoor = AgentOs & {
	_sidecarClient: {
		getSessionState(
			session: unknown,
			vm: unknown,
			sessionId: string,
		): Promise<SidecarSessionState>;
	};
	_sessionClosePromises: Map<string, Promise<void>>;
	_sidecarSession: unknown;
	_sidecarVm: unknown;
};

type HostProcessRow = {
	pid: number;
	ppid: number;
};

async function getSessionState(
	vm: AgentOs,
	sessionId: string,
): Promise<SidecarSessionState> {
	const backdoor = vm as SidecarBackdoor;
	return backdoor._sidecarClient.getSessionState(
		backdoor._sidecarSession,
		backdoor._sidecarVm,
		sessionId,
	);
}

async function closeSessionAndWait(
	vm: AgentOs,
	sessionId: string,
): Promise<void> {
	vm.closeSession(sessionId);
	const backdoor = vm as SidecarBackdoor;
	const closePromise = backdoor._sessionClosePromises.get(sessionId);
	if (closePromise) {
		await closePromise;
	}
}

function readHostProcesses(): HostProcessRow[] {
	return execFileSync("ps", ["-eo", "pid=,ppid="], {
		encoding: "utf8",
	})
		.split("\n")
		.map((line) => line.trim())
		.filter(Boolean)
		.map((line) => {
			const [pid, ppid] = line.split(/\s+/);
			return {
				pid: Number(pid),
				ppid: Number(ppid),
			};
		})
		.filter(
			(row) => Number.isFinite(row.pid) && Number.isFinite(row.ppid),
		);
}

function collectHostProcessTree(rootPid: number): number[] {
	const rows = readHostProcesses();
	const byParent = new Map<number, number[]>();
	for (const row of rows) {
		const children = byParent.get(row.ppid);
		if (children) {
			children.push(row.pid);
		} else {
			byParent.set(row.ppid, [row.pid]);
		}
	}
	if (!rows.some((row) => row.pid === rootPid)) {
		return [];
	}
	const discovered: number[] = [];
	const queue = [rootPid];
	while (queue.length > 0) {
		const pid = queue.shift();
		if (pid === undefined || discovered.includes(pid)) {
			continue;
		}
		discovered.push(pid);
		for (const childPid of byParent.get(pid) ?? []) {
			queue.push(childPid);
		}
	}
	return discovered.sort((left, right) => left - right);
}

async function listFdLinks(pid: number): Promise<string[]> {
	try {
		const fds = await readdir(`/proc/${pid}/fd`);
		const links = await Promise.all(
			fds.map(async (fd) => {
				try {
					return await readlink(`/proc/${pid}/fd/${fd}`);
				} catch {
					return null;
				}
			}),
		);
		return links.filter((link): link is string => link !== null);
	} catch {
		return [];
	}
}

async function snapshotSessionResources(rootPid: number): Promise<{
	pids: number[];
	fdLinks: string[];
	socketLinks: string[];
}> {
	const pids = collectHostProcessTree(rootPid);
	const links = (await Promise.all(pids.map((pid) => listFdLinks(pid)))).flat();
	return {
		pids,
		fdLinks: links,
		socketLinks: links.filter((link) => link.startsWith("socket:[")),
	};
}

function zombieTimerCount(vm: AgentOs): number {
	return getAgentOsKernel(vm).zombieTimerCount;
}

async function waitForSessionResources(
	rootPids: number[],
	baselineZombieTimers: number,
	vm: AgentOs,
): Promise<void> {
	return waitFor(
		() => ({
			pids: rootPids.filter((pid) => collectHostProcessTree(pid).length > 0),
			zombieTimers: zombieTimerCount(vm),
		}),
		{
			timeoutMs: 30_000,
			isReady: ({ pids, zombieTimers }) =>
				pids.length === 0 && zombieTimers === baselineZombieTimers,
		},
	).then(() => undefined);
}

function uniqueSessionRootPids(sessionStates: Array<{ pid?: number }>): number[] {
	const pidCounts = new Map<number, number>();
	for (const state of sessionStates) {
		if (typeof state.pid !== "number") {
			continue;
		}
		pidCounts.set(state.pid, (pidCounts.get(state.pid) ?? 0) + 1);
	}
	return sessionStates
		.map((state) => state.pid)
		.filter(
			(pid): pid is number =>
				typeof pid === "number" && (pidCounts.get(pid) ?? 0) === 1,
		);
}

function isSharedRuntimeCloseRaceError(error: unknown): boolean {
	if (!(error instanceof Error)) {
		return false;
	}

	return [
		"sidecar stdout closed while reading frame",
		"Broken pipe (os error 32)",
		"timed out waiting for sidecar protocol frame for close_agent_session",
	].some((fragment) => error.message.includes(fragment));
}

async function createTextMock(mockKind: MockKind): Promise<{
	url: string;
	stop: () => Promise<void>;
}> {
	if (mockKind === "openai") {
		const fixtures: ResponsesFixture[] = [
			{
				name: "cleanup-text-response",
				predicate: () => true,
				response: {
					id: "resp_cleanup_text",
					output: [
						{
							type: "message",
							role: "assistant",
							content: [
								{
									type: "output_text",
									text: PROMPT_RESPONSE,
								},
							],
						},
					],
				},
			},
		];
		const mock = await startResponsesMock(fixtures);
		return {
			url: mock.url,
			stop: mock.stop,
		};
	}

	const { mock, url } = await startLlmock([
		createAnthropicFixture({}, { content: PROMPT_RESPONSE }),
	]);
	return {
		url,
		stop: () => stopLlmock(mock),
	};
}

async function createHangingAnthropicServer(): Promise<{
	url: string;
	stop: () => Promise<void>;
	waitForRequest: () => Promise<void>;
}> {
	const sockets = new Set<Socket>();
	let requestCount = 0;
	const server = createServer((req) => {
		requestCount += 1;
		req.on("data", () => {});
		req.on("error", () => {});
	});
	server.on("connection", (socket) => {
		sockets.add(socket);
		socket.on("close", () => {
			sockets.delete(socket);
		});
	});
	await new Promise<void>((resolve, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", () => {
			server.off("error", reject);
			resolve();
		});
	});
	const address = server.address() as AddressInfo;
	return {
		url: `http://127.0.0.1:${address.port}`,
		waitForRequest: () =>
			waitFor(() => requestCount, {
				timeoutMs: 15_000,
				isReady: (count) => count > 0,
			}).then(() => undefined),
		stop: async () => {
			for (const socket of sockets) {
				socket.destroy();
			}
			await new Promise<void>((resolve, reject) => {
				server.close((error) => {
					if (error) {
						reject(error);
						return;
					}
					resolve();
				});
			});
		},
	};
}

async function createSlowResponseMock(mockKind: MockKind): Promise<{
	url: string;
	stop: () => Promise<void>;
	waitForRequest: () => Promise<void>;
}> {
	if (mockKind !== "openai") {
		throw new Error(`slow-response mock is unsupported for ${mockKind}`);
	}

	const mock = await startResponsesMock([
		{
			name: "slow-cleanup-response",
			predicate: () => true,
			delayMs: 60_000,
			response: {
				id: "resp_cleanup_slow",
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

	return {
		url: mock.url,
		stop: mock.stop,
		waitForRequest: () =>
			waitFor(() => mock.requests.length, {
				timeoutMs: 15_000,
				isReady: (count) => count > 0,
			}).then(() => undefined),
	};
}

async function createActivePromptMock(
	agent: SessionCleanupAgent,
): Promise<{
	url: string;
	stop: () => Promise<void>;
	waitForRequest: () => Promise<void>;
}> {
	if (agent.activePromptMock === "slow_response") {
		return createSlowResponseMock(agent.mockKind);
	}
	return createHangingAnthropicServer();
}

function registerSharedCleanupCoverage(agents: SessionCleanupAgent[]): void {
	test.each(agents)(
		"$label closeSession() frees session resources after a completed prompt and is idempotent",
		async (agent) => {
			const mock = await createTextMock(agent.mockKind);
			const vm = await agent.createVm(mock.url);
			try {
				const baselineZombieTimers = zombieTimerCount(vm);
				const { sessionId } = await agent.createSession(vm, mock.url);
				const sessionState = await getSessionState(vm, sessionId);
				expect(sessionState.pid).toBeTypeOf("number");

				const { response, text } = await vm.prompt(sessionId, PROMPT_TEXT);
				expect(response.error).toBeUndefined();
				expect(text).toContain(PROMPT_RESPONSE);

				await closeSessionAndWait(vm, sessionId);
				await waitForSessionResources(
					[sessionState.pid!],
					baselineZombieTimers,
					vm,
				);

				await expect(closeSessionAndWait(vm, sessionId)).resolves.toBeUndefined();
				await waitForSessionResources(
					[sessionState.pid!],
					baselineZombieTimers,
					vm,
				);
			} finally {
				await vm.dispose();
				await mock.stop();
			}
		},
		120_000,
	);

	test.each(agents)(
		"$label active-prompt cleanup frees sockets, FDs, and processes",
		async (agent) => {
			const promptMock = await createActivePromptMock(agent);
			const vm = await agent.createVm(promptMock.url);
			try {
				const baselineZombieTimers = zombieTimerCount(vm);
				const { sessionId } = await agent.createSession(vm, promptMock.url);
				const sessionState = await getSessionState(vm, sessionId);
				expect(sessionState.pid).toBeTypeOf("number");

				const promptPromise = vm.prompt(sessionId, PROMPT_TEXT);
				await promptMock.waitForRequest();

				const sessionPids = await waitFor(
					() => collectHostProcessTree(sessionState.pid!),
					{
						isReady: (pids) => pids.length > 0,
					},
				);
				const resourcesBeforeClose = await waitFor(
					() => snapshotSessionResources(sessionState.pid!),
					{
						isReady: (snapshot) => snapshot.socketLinks.length > 0,
					},
				);
				expect(resourcesBeforeClose.pids).toEqual(sessionPids);
				expect(resourcesBeforeClose.fdLinks.length).toBeGreaterThan(0);

				if (agent.activePromptTermination === "cancel_then_close") {
					const cancelResponse = await vm.cancelSession(sessionId);
					expect(cancelResponse.error).toBeUndefined();
				} else {
					vm.closeSession(sessionId);
				}

				const promptOutcome = await Promise.race([
					promptPromise.then(
						(result) => ({ kind: "resolved" as const, result }),
						(error) => ({ kind: "rejected" as const, error }),
					),
					new Promise<{ kind: "timeout" }>((resolve) =>
						setTimeout(() => resolve({ kind: "timeout" }), 15_000),
					),
				]);
				expect(promptOutcome.kind).not.toBe("timeout");
				if (promptOutcome.kind === "resolved") {
					const stopReason = (
						promptOutcome.result.response.result as
							| { stopReason?: string }
							| undefined
					)?.stopReason;
					expect(
						promptOutcome.result.response.error !== undefined ||
							stopReason === "cancelled",
					).toBe(true);
				} else {
					expect(promptOutcome.error).toBeInstanceOf(Error);
				}

				if (agent.activePromptTermination === "cancel_then_close") {
					await closeSessionAndWait(vm, sessionId);
				}
				await waitForSessionResources(
					sessionPids,
					baselineZombieTimers,
					vm,
				);
			} finally {
				await vm.dispose();
				await promptMock.stop();
			}
		},
		120_000,
	);
}

describe("session cleanup", () => {
	registerSharedCleanupCoverage(PI_AGENTS);

	test("Pi SDK returns to baseline after five sequential createSession()/closeSession() cycles", async () => {
		const agent = PI_AGENTS[0];
		const mock = await createTextMock(agent.mockKind);
		const vm = await agent.createVm(mock.url);
		try {
			const baselineZombieTimers = zombieTimerCount(vm);

			for (let index = 0; index < 5; index += 1) {
				const { sessionId } = await agent.createSession(vm, mock.url);
				const sessionState = await getSessionState(vm, sessionId);
				expect(sessionState.pid).toBeTypeOf("number");
				const { response, text } = await vm.prompt(sessionId, PROMPT_TEXT);
				expect(response.error).toBeUndefined();
				expect(text).toContain(PROMPT_RESPONSE);

				await closeSessionAndWait(vm, sessionId);
				await waitForSessionResources(
					[sessionState.pid!],
					baselineZombieTimers,
					vm,
				);
			}
		} finally {
			await vm.dispose();
			await mock.stop();
		}
	}, 120_000);

	test("Pi CLI returns to baseline after three concurrent sessions are closed", async () => {
		const agent = PI_AGENTS[1];
		const mock = await createTextMock(agent.mockKind);
		const vm = await agent.createVm(mock.url);
		try {
			const baselineZombieTimers = zombieTimerCount(vm);
			const sessions = await Promise.all(
				Array.from({ length: 3 }, () => agent.createSession(vm, mock.url)),
			);
			const sessionStates = await Promise.all(
				sessions.map(({ sessionId }) => getSessionState(vm, sessionId)),
			);
			expect(sessionStates.every((state) => typeof state.pid === "number")).toBe(
				true,
			);

			const activePids = sessionStates.map((state) => state.pid!);
			const dedicatedSessionPids = uniqueSessionRootPids(sessionStates);
			expect(activePids.length).toBe(3);

			const closeResults = await Promise.allSettled(
				sessions.map(({ sessionId }) => closeSessionAndWait(vm, sessionId)),
			);
			for (const result of closeResults) {
				if (result.status === "rejected") {
					expect(isSharedRuntimeCloseRaceError(result.reason)).toBe(true);
				}
			}
			await waitForSessionResources(dedicatedSessionPids, baselineZombieTimers, vm);
		} finally {
			await vm.dispose();
			await mock.stop();
		}
	}, 120_000);
});

describe.skipIf(registrySkipReason)("session cleanup with registry-backed agents", () => {
	registerSharedCleanupCoverage(REGISTRY_AGENTS);
});
