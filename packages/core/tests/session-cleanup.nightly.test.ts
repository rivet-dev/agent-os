// Nightly: projects the complete registry command bundle.
import { execFileSync } from "node:child_process";
import { readdir, readlink } from "node:fs/promises";
import {
	createServer,
	type IncomingMessage,
	type ServerResponse,
} from "node:http";
import { resolve } from "node:path";
import claude from "@agentos-software/claude-code";
import opencode from "@agentos-software/opencode";
import pi from "@agentos-software/pi";
import piCli from "@agentos-software/pi-cli";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import { NativeSidecarKernelProxy } from "../src/sidecar/rpc-client.js";
import { getAgentOsKernel } from "../src/test/runtime.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";
import {
	createVmWorkspace as createOpenCodeWorkspace,
	createVmOpenCodeHome,
} from "./helpers/opencode-helper.js";
import { REGISTRY_SOFTWARE } from "./helpers/registry-commands.js";
import { promptResultText } from "./helpers/session-result.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const PROMPT_TEXT = "Reply with exactly cleanup-ok.";
const PROMPT_RESPONSE = "cleanup-ok";

type MockKind = "anthropic";

type SessionCleanupAgent = {
	agentType: string;
	label: string;
	mockKind: MockKind;
	activePromptTermination: "close" | "cancel_then_close";
	activePromptMock: "hang";
	createVm: (mockUrl: string) => Promise<AgentOs>;
	openTestSession: (
		vm: AgentOs,
		mockUrl: string,
	) => Promise<{ sessionId: string }>;
};

let cleanupSessionSequence = 0;
function nextCleanupSessionId(agent: string): string {
	cleanupSessionSequence += 1;
	return `${agent}-cleanup-${cleanupSessionSequence}`;
}

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
				mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
				software: [pi],
			}),
		openTestSession: async (vm, mockUrl) => {
			const homeDir = await createVmPiHome(vm, mockUrl);
			const workspaceDir = await createVmPiWorkspace(vm);
			const sessionId = nextCleanupSessionId("pi");
			await vm.openSession({
				sessionId,
				agent: "pi",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
					PI_SKIP_VERSION_CHECK: "1",
				},
			});
			return { sessionId };
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
				mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
				software: [piCli],
			}),
		openTestSession: async (vm, mockUrl) => {
			const homeDir = await createVmPiHome(vm, mockUrl);
			const workspaceDir = await createVmPiWorkspace(vm);
			const sessionId = nextCleanupSessionId("pi-cli");
			await vm.openSession({
				sessionId,
				agent: "pi-cli",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
					PI_SKIP_VERSION_CHECK: "1",
				},
			});
			return { sessionId };
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
				mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
				software: [claude, ...REGISTRY_SOFTWARE],
			}),
		openTestSession: async (vm, mockUrl) => {
			const sessionId = nextCleanupSessionId("claude");
			await vm.openSession({
				sessionId,
				agent: "claude",
				cwd: "/home/agentos",
				env: {
					ANTHROPIC_API_KEY: "mock-key",
					ANTHROPIC_BASE_URL: mockUrl,
				},
			});
			return { sessionId };
		},
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
				mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
				software: [opencode, ...REGISTRY_SOFTWARE],
			}),
		openTestSession: async (vm, mockUrl) => {
			const homeDir = await createVmOpenCodeHome(vm, mockUrl);
			const workspaceDir = await createOpenCodeWorkspace(vm);
			const sessionId = nextCleanupSessionId("opencode");
			await vm.openSession({
				sessionId,
				agent: "opencode",
				cwd: workspaceDir,
				env: {
					HOME: homeDir,
					ANTHROPIC_API_KEY: "mock-key",
				},
			});
			return { sessionId };
		},
	},
];

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

async function createVmPiWorkspace(vm: AgentOs): Promise<string> {
	const workspaceDir = "/home/agentos/workspace";
	await vm.mkdir(workspaceDir, { recursive: true });
	return workspaceDir;
}

type SidecarBackdoor = {
	_sidecarClient: {
		getProcessSnapshot(
			session: unknown,
			vm: unknown,
		): Promise<
			Array<{
				pid: number;
				ppid: number;
				status: "running" | "exited" | "stopped";
			}>
		>;
		getZombieTimerCount(
			session: unknown,
			vm: unknown,
		): Promise<{ count: number }>;
	};
	_sidecarSession: unknown;
	_sidecarVm: unknown;
};

type HostProcessRow = {
	pid: number;
	ppid: number;
};

async function unloadTestSession(
	vm: AgentOs,
	sessionId: string,
): Promise<void> {
	await vm.unloadSession({ sessionId });
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
		.filter((row) => Number.isFinite(row.pid) && Number.isFinite(row.ppid));
}

function collectHostProcessTree(rootPid: number): number[] {
	return collectProcessTree(readHostProcesses(), rootPid);
}

function collectProcessTree(rows: HostProcessRow[], rootPid: number): number[] {
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

async function readKernelProcesses(vm: AgentOs): Promise<HostProcessRow[]> {
	if (!(getAgentOsKernel(vm) instanceof NativeSidecarKernelProxy)) {
		return vm.allProcesses().map(({ pid, ppid }) => ({ pid, ppid }));
	}

	const backdoor = vm as unknown as SidecarBackdoor;
	return (
		await backdoor._sidecarClient.getProcessSnapshot(
			backdoor._sidecarSession,
			backdoor._sidecarVm,
		)
	)
		.filter((process) => process.status !== "exited")
		.map(({ pid, ppid }) => ({ pid, ppid }));
}

async function collectKernelProcessTree(
	vm: AgentOs,
	rootPid: number,
): Promise<number[]> {
	return collectProcessTree(await readKernelProcesses(vm), rootPid);
}

type SessionProcessTree =
	| { kind: "kernel"; pids: number[] }
	| { kind: "host"; pids: number[] };

async function collectSessionProcessTree(
	vm: AgentOs,
	rootPid: number,
): Promise<SessionProcessTree> {
	const kernelPids = await collectKernelProcessTree(vm, rootPid);
	if (
		kernelPids.length > 0 ||
		getAgentOsKernel(vm) instanceof NativeSidecarKernelProxy
	) {
		return { kind: "kernel", pids: kernelPids };
	}
	return {
		kind: "host",
		pids: collectHostProcessTree(rootPid),
	};
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

async function snapshotSessionResources(
	vm: AgentOs,
	rootPid: number,
): Promise<{
	kind: "kernel" | "host";
	pids: number[];
	fdLinks: string[];
	socketLinks: string[];
}> {
	const tree = await collectSessionProcessTree(vm, rootPid);
	const pids = tree.pids;
	const links = (await Promise.all(pids.map((pid) => listFdLinks(pid)))).flat();
	return {
		kind: tree.kind,
		pids,
		fdLinks: links,
		socketLinks: links.filter((link) => link.startsWith("socket:[")),
	};
}

async function snapshotVmResources(vm: AgentOs): Promise<{
	pids: number[];
	processCount: number;
	fdCount: number;
	socketCount: number;
}> {
	const pids = [
		...new Set((await readKernelProcesses(vm)).map(({ pid }) => pid)),
	];
	const links = (await Promise.all(pids.map((pid) => listFdLinks(pid)))).flat();
	return {
		pids,
		processCount: pids.length,
		fdCount: links.length,
		socketCount: links.filter((link) => link.startsWith("socket:[")).length,
	};
}

async function zombieTimerCount(vm: AgentOs): Promise<number> {
	if (!(getAgentOsKernel(vm) instanceof NativeSidecarKernelProxy)) {
		return getAgentOsKernel(vm).zombieTimerCount;
	}

	const backdoor = vm as unknown as SidecarBackdoor;
	return (
		await backdoor._sidecarClient.getZombieTimerCount(
			backdoor._sidecarSession,
			backdoor._sidecarVm,
		)
	).count;
}

async function assertSessionResourcesReleased(
	rootPids: number[],
	baselineZombieTimers: number,
	vm: AgentOs,
	baselineVmResources: {
		processCount: number;
		fdCount: number;
		socketCount: number;
	},
): Promise<void> {
	const snapshots = await Promise.all(
		rootPids.map((pid) => snapshotSessionResources(vm, pid)),
	);
	for (const snapshot of snapshots) {
		expect(snapshot.pids).toHaveLength(0);
		expect(snapshot.fdLinks).toHaveLength(0);
		expect(snapshot.socketLinks).toHaveLength(0);
	}
	const vmResources = await snapshotVmResources(vm);
	expect(vmResources.processCount).toBe(baselineVmResources.processCount);
	expect(vmResources.fdCount).toBe(baselineVmResources.fdCount);
	expect(vmResources.socketCount).toBe(baselineVmResources.socketCount);
	expect(await zombieTimerCount(vm)).toBe(baselineZombieTimers);
}

function isSharedRuntimeCloseRaceError(error: unknown): boolean {
	if (!(error instanceof Error)) {
		return false;
	}

	return [
		"sidecar stdout closed while reading frame",
		"Broken pipe (os error 32)",
		"sidecar unresponsive: no protocol frames or heartbeats",
	].some((fragment) => error.message.includes(fragment));
}

function createDeferredSignal(): {
	resolve: () => void;
	wait: () => Promise<void>;
} {
	let ready = false;
	let resolvePromise!: () => void;
	const promise = new Promise<void>((resolve) => {
		resolvePromise = () => {
			if (ready) {
				return;
			}
			ready = true;
			resolve();
		};
	});
	return {
		resolve: resolvePromise,
		wait: () => (ready ? Promise.resolve() : promise),
	};
}

async function createTextMock(mockKind: MockKind): Promise<{
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

async function readJsonBody(
	req: IncomingMessage,
): Promise<Record<string, unknown>> {
	const chunks: Buffer[] = [];
	for await (const chunk of req) {
		chunks.push(Buffer.from(chunk));
	}
	return JSON.parse(Buffer.concat(chunks).toString("utf8")) as Record<
		string,
		unknown
	>;
}

function anthropicTextContent(content: unknown): string {
	if (typeof content === "string") {
		return content;
	}
	if (!Array.isArray(content)) {
		return "";
	}
	return content
		.map((block) => {
			if (!block || typeof block !== "object") {
				return "";
			}
			const textBlock = block as { type?: unknown; text?: unknown };
			return textBlock.type === "text" && typeof textBlock.text === "string"
				? textBlock.text
				: "";
		})
		.join("");
}

function getLastAnthropicUserText(body: Record<string, unknown>): string {
	const messages = body.messages;
	if (!Array.isArray(messages)) {
		return "";
	}
	for (let index = messages.length - 1; index >= 0; index -= 1) {
		const message = messages[index];
		if (!message || typeof message !== "object") {
			continue;
		}
		const chatMessage = message as { role?: unknown; content?: unknown };
		if (chatMessage.role === "user") {
			return anthropicTextContent(chatMessage.content);
		}
	}
	return "";
}

function writeAnthropicTextResponse(
	res: ServerResponse,
	content: string,
): void {
	const body = JSON.stringify({
		id: "msg_cleanup_text",
		type: "message",
		role: "assistant",
		content: [
			{
				type: "text",
				text: content,
			},
		],
		model: "claude-sonnet-4-20250514",
		stop_reason: "end_turn",
		stop_sequence: null,
		usage: {
			input_tokens: 0,
			output_tokens: 0,
		},
	});
	res.writeHead(200, {
		"content-type": "application/json",
		"content-length": Buffer.byteLength(body),
	});
	res.end(body);
}

function writeJson(
	res: ServerResponse,
	statusCode: number,
	body: Record<string, unknown>,
): void {
	const payload = JSON.stringify(body);
	res.writeHead(statusCode, {
		"content-type": "application/json",
		"content-length": Buffer.byteLength(payload),
	});
	res.end(payload);
}

function writeJsonError(
	res: ServerResponse,
	statusCode: number,
	body: Record<string, unknown>,
): void {
	const payload = JSON.stringify(body);
	res.writeHead(statusCode, {
		"content-type": "application/json",
		"content-length": Buffer.byteLength(payload),
	});
	res.end(payload);
}

async function createHangingAnthropicServer(): Promise<{
	url: string;
	stop: () => Promise<void>;
	waitForRequest: () => Promise<void>;
}> {
	const pendingResponses = new Set<ServerResponse>();
	const requestSignal = createDeferredSignal();
	const server = createServer(async (req, res) => {
		const pathname = req.url
			? new URL(req.url, "http://127.0.0.1").pathname
			: "";
		if (req.method !== "POST" || pathname !== "/v1/messages") {
			writeJsonError(res, 404, { error: "not_found" });
			return;
		}

		try {
			const body = await readJsonBody(req);

			if (getLastAnthropicUserText(body).includes(PROMPT_TEXT)) {
				requestSignal.resolve();
				pendingResponses.add(res);
				const clearPending = () => pendingResponses.delete(res);
				req.on("close", clearPending);
				res.on("close", clearPending);
				return;
			}

			writeAnthropicTextResponse(res, PROMPT_RESPONSE);
		} catch (error) {
			writeJsonError(res, 500, {
				error: "invalid_request",
				message: error instanceof Error ? error.message : String(error),
			});
		}
	});

	await new Promise<void>((resolve) => {
		server.listen(0, "127.0.0.1", () => resolve());
	});
	server.unref();
	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("mock server did not expose a TCP port");
	}

	return {
		url: `http://127.0.0.1:${address.port}`,
		waitForRequest: requestSignal.wait,
		stop: async () => {
			for (const res of pendingResponses) {
				res.destroy();
			}
			server.closeAllConnections?.();
			await new Promise<void>((resolve, reject) => {
				server.close((error) => {
					if (error) reject(error);
					else resolve();
				});
			});
		},
	};
}

async function createActivePromptMock(_agent: SessionCleanupAgent): Promise<{
	url: string;
	stop: () => Promise<void>;
	waitForRequest: () => Promise<void>;
}> {
	return createHangingAnthropicServer();
}

async function assertActivePromptCleanup(
	agent: SessionCleanupAgent,
): Promise<void> {
	const promptMock = await createActivePromptMock(agent);
	const vm = await agent.createVm(promptMock.url);
	try {
		const baselineSessionCount = (await vm.listSessions()).sessions.length;
		const baselineZombieTimers = await zombieTimerCount(vm);
		const baselineVmResources = await snapshotVmResources(vm);
		const { sessionId } = await agent.openTestSession(vm, promptMock.url);
		const openedResources = await snapshotVmResources(vm);
		const activePids = openedResources.pids.filter(
			(pid) => !baselineVmResources.pids.includes(pid),
		);
		expect(activePids.length).toBeGreaterThan(0);

		const promptPromise = vm.prompt({
			sessionId,
			content: [{ type: "text", text: PROMPT_TEXT }],
		});
		await promptMock.waitForRequest();
		const resourcesBeforeClose = await snapshotSessionResources(
			vm,
			activePids[0],
		);
		expect(resourcesBeforeClose.pids.length).toBeGreaterThan(0);

		if (agent.activePromptTermination === "cancel_then_close") {
			const cancelResponse = await vm.cancelPrompt({ sessionId });
			expect(cancelResponse.status).toBe("cancelled");
		} else {
			await vm.unloadSession({ sessionId });
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
			expect(promptOutcome.result.stopReason).toBe("cancelled");
		} else if (promptOutcome.kind === "rejected") {
			expect(promptOutcome.error).toBeInstanceOf(Error);
		}

		if (agent.activePromptTermination === "cancel_then_close") {
			await unloadTestSession(vm, sessionId);
		}
		expect((await vm.listSessions()).sessions).toHaveLength(
			baselineSessionCount + 1,
		);
		await assertSessionResourcesReleased(
			activePids,
			baselineZombieTimers,
			vm,
			baselineVmResources,
		);
		await vm.deleteSession({ sessionId });
		expect((await vm.listSessions()).sessions).toHaveLength(
			baselineSessionCount,
		);
	} finally {
		await vm.dispose();
		await promptMock.stop();
	}
}

function registerSharedCleanupCoverage(agents: SessionCleanupAgent[]): void {
	test.each(
		agents,
	)("$label unloadSession() frees runtime resources after a completed prompt and is idempotent", async (agent) => {
		const mock = await createTextMock(agent.mockKind);
		const vm = await agent.createVm(mock.url);
		try {
			const baselineSessionCount = (await vm.listSessions()).sessions.length;
			const baselineZombieTimers = await zombieTimerCount(vm);
			const baselineVmResources = await snapshotVmResources(vm);
			const { sessionId } = await agent.openTestSession(vm, mock.url);
			const openedResources = await snapshotVmResources(vm);
			const activePids = openedResources.pids.filter(
				(pid) => !baselineVmResources.pids.includes(pid),
			);
			expect(activePids.length).toBeGreaterThan(0);

			const result = await vm.prompt({
				sessionId,
				content: [{ type: "text", text: PROMPT_TEXT }],
			});
			expect(result.stopReason).toBeDefined();
			expect(promptResultText(result)).toContain(PROMPT_RESPONSE);
			const vmResourcesBeforeClose = await snapshotVmResources(vm);
			expect(vmResourcesBeforeClose.processCount).toBeGreaterThanOrEqual(
				baselineVmResources.processCount + 1,
			);

			await unloadTestSession(vm, sessionId);
			expect((await vm.listSessions()).sessions).toHaveLength(
				baselineSessionCount + 1,
			);
			await assertSessionResourcesReleased(
				activePids,
				baselineZombieTimers,
				vm,
				baselineVmResources,
			);

			await expect(unloadTestSession(vm, sessionId)).resolves.toBeUndefined();
			expect((await vm.listSessions()).sessions).toHaveLength(
				baselineSessionCount + 1,
			);
			await assertSessionResourcesReleased(
				activePids,
				baselineZombieTimers,
				vm,
				baselineVmResources,
			);
			await vm.deleteSession({ sessionId });
			expect((await vm.listSessions()).sessions).toHaveLength(
				baselineSessionCount,
			);
		} finally {
			await vm.dispose();
			await mock.stop();
		}
	}, 300_000);

	test.each(agents)(
		"$label active-prompt cleanup frees sockets, FDs, and processes",
		async (agent) => assertActivePromptCleanup(agent),
		300_000,
	);
}

describe("session cleanup", () => {
	registerSharedCleanupCoverage(PI_AGENTS);

	test("Pi SDK returns to baseline after five sequential open/unload cycles", async () => {
		const agent = PI_AGENTS[0];
		const mock = await createTextMock(agent.mockKind);
		const vm = await agent.createVm(mock.url);
		try {
			const baselineSessionCount = (await vm.listSessions()).sessions.length;
			const baselineZombieTimers = await zombieTimerCount(vm);
			const baselineVmResources = await snapshotVmResources(vm);

			for (let index = 0; index < 5; index += 1) {
				const { sessionId } = await agent.openTestSession(vm, mock.url);
				const openedResources = await snapshotVmResources(vm);
				const activePids = openedResources.pids.filter(
					(pid) => !baselineVmResources.pids.includes(pid),
				);
				expect(activePids.length).toBeGreaterThan(0);
				const result = await vm.prompt({
					sessionId,
					content: [{ type: "text", text: PROMPT_TEXT }],
				});
				expect(result.stopReason).toBeDefined();
				expect(promptResultText(result)).toContain(PROMPT_RESPONSE);

				await unloadTestSession(vm, sessionId);
				await assertSessionResourcesReleased(
					activePids,
					baselineZombieTimers,
					vm,
					baselineVmResources,
				);
				await vm.deleteSession({ sessionId });
				expect((await vm.listSessions()).sessions).toHaveLength(
					baselineSessionCount,
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
			const baselineSessionCount = (await vm.listSessions()).sessions.length;
			const baselineZombieTimers = await zombieTimerCount(vm);
			const baselineVmResources = await snapshotVmResources(vm);
			const sessions = await Promise.all(
				Array.from({ length: 3 }, () => agent.openTestSession(vm, mock.url)),
			);
			const openedResources = await snapshotVmResources(vm);
			const activePids = openedResources.pids.filter(
				(pid) => !baselineVmResources.pids.includes(pid),
			);
			expect(activePids.length).toBeGreaterThan(0);

			const closeResults = await Promise.allSettled(
				sessions.map(({ sessionId }) => unloadTestSession(vm, sessionId)),
			);
			for (const result of closeResults) {
				if (result.status === "rejected") {
					expect(isSharedRuntimeCloseRaceError(result.reason)).toBe(true);
				}
			}
			await assertSessionResourcesReleased(
				activePids,
				baselineZombieTimers,
				vm,
				baselineVmResources,
			);
			await Promise.all(
				sessions.map(({ sessionId }) => vm.deleteSession({ sessionId })),
			);
			expect((await vm.listSessions()).sessions).toHaveLength(
				baselineSessionCount,
			);
		} finally {
			await vm.dispose();
			await mock.stop();
		}
	}, 300_000);
});

describe("session cleanup with registry-backed agents", () => {
	registerSharedCleanupCoverage(REGISTRY_AGENTS);
});
