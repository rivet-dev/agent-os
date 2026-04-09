import { execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { readlink, readdir } from "node:fs/promises";
import type { AddressInfo, Socket } from "node:net";
import { resolve } from "node:path";
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

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const PROMPT_TEXT = "Reply with exactly cleanup-ok.";
const PROMPT_RESPONSE = "cleanup-ok";

const AGENTS = [
	{ agentType: "pi", software: pi, label: "Pi SDK" },
	{ agentType: "pi-cli", software: piCli, label: "Pi CLI" },
] as const;

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

async function createVmWorkspace(vm: AgentOs): Promise<string> {
	const workspaceDir = "/home/user/workspace";
	await vm.mkdir(workspaceDir, { recursive: true });
	return workspaceDir;
}

async function createAgentVm(
	agent: (typeof AGENTS)[number],
	mockUrl: string,
): Promise<AgentOs> {
	return AgentOs.create({
		loopbackExemptPorts: [Number(new URL(mockUrl).port)],
		moduleAccessCwd: MODULE_ACCESS_CWD,
		software: [agent.software],
	});
}

async function createAgentSession(
	vm: AgentOs,
	agent: (typeof AGENTS)[number],
	mockUrl: string,
): Promise<{ sessionId: string }> {
	const homeDir = await createVmPiHome(vm, mockUrl);
	const workspaceDir = await createVmWorkspace(vm);
	return vm.createSession(agent.agentType, {
		cwd: workspaceDir,
		env: {
			HOME: homeDir,
			ANTHROPIC_API_KEY: "mock-key",
			ANTHROPIC_BASE_URL: mockUrl,
			PI_SKIP_VERSION_CHECK: "1",
		},
	});
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
	await Promise.resolve();
	await backdoor._sessionClosePromises.get(sessionId);
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

async function createTextMock(): Promise<{
	url: string;
	stop: () => Promise<void>;
}> {
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

describe("session cleanup", () => {
	test.each(AGENTS)(
		"$label closeSession() frees session resources after a completed prompt and is idempotent",
		async (agent) => {
			const mock = await createTextMock();
			const vm = await createAgentVm(agent, mock.url);
			try {
				const baselineZombieTimers = zombieTimerCount(vm);
				const { sessionId } = await createAgentSession(vm, agent, mock.url);
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

	test("Pi SDK returns to baseline after five sequential createSession()/closeSession() cycles", async () => {
		const agent = AGENTS[0];
		const mock = await createTextMock();
		const vm = await createAgentVm(agent, mock.url);
		try {
			const baselineZombieTimers = zombieTimerCount(vm);

			for (let index = 0; index < 5; index += 1) {
				const { sessionId } = await createAgentSession(vm, agent, mock.url);
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
		const agent = AGENTS[1];
		const mock = await createTextMock();
		const vm = await createAgentVm(agent, mock.url);
		try {
			const baselineZombieTimers = zombieTimerCount(vm);
			const sessions = await Promise.all(
				Array.from({ length: 3 }, () => createAgentSession(vm, agent, mock.url)),
			);
			const sessionStates = await Promise.all(
				sessions.map(({ sessionId }) => getSessionState(vm, sessionId)),
			);
			expect(sessionStates.every((state) => typeof state.pid === "number")).toBe(
				true,
			);

			const activePids = sessionStates.map((state) => state.pid!);
			expect(activePids.length).toBe(3);

			await Promise.all(
				sessions.map(({ sessionId }) => closeSessionAndWait(vm, sessionId)),
			);
			await waitForSessionResources(activePids, baselineZombieTimers, vm);
		} finally {
			await vm.dispose();
			await mock.stop();
		}
	}, 120_000);

	test.each(AGENTS)(
		"$label closeSession() during an active prompt cancels the prompt and frees sockets, FDs, and processes",
		async (agent) => {
			const hanging = await createHangingAnthropicServer();
			const vm = await createAgentVm(agent, hanging.url);
			try {
				const baselineZombieTimers = zombieTimerCount(vm);
				const { sessionId } = await createAgentSession(vm, agent, hanging.url);
				const sessionState = await getSessionState(vm, sessionId);
				expect(sessionState.pid).toBeTypeOf("number");

				const promptPromise = vm.prompt(sessionId, PROMPT_TEXT);
				await hanging.waitForRequest();

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

				vm.closeSession(sessionId);
				await waitForSessionResources(
					sessionPids,
					baselineZombieTimers,
					vm,
				);

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
					expect(promptOutcome.result.response.error).toBeDefined();
				} else {
					expect(promptOutcome.error).toBeInstanceOf(Error);
				}
			} finally {
				await vm.dispose();
				await hanging.stop();
			}
		},
		120_000,
	);
});
