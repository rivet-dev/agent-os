import { execFileSync } from "node:child_process";
import {
	createServer,
	type IncomingMessage,
	type ServerResponse,
} from "node:http";
import { readlink, readdir } from "node:fs/promises";
import { resolve } from "node:path";
import claude from "@rivet-dev/agent-os-claude";
import codex from "@rivet-dev/agent-os-codex-agent";
import opencode from "@rivet-dev/agent-os-opencode";
import pi from "@rivet-dev/agent-os-pi";
import piCli from "@rivet-dev/agent-os-pi-cli";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import { NativeSidecarKernelProxy } from "../src/sidecar/rpc-client.js";
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

const CODEX_CLEANUP_AGENT = REGISTRY_AGENTS.find(
	(agent) => agent.agentType === "codex",
);

if (!CODEX_CLEANUP_AGENT) {
	throw new Error("missing Codex cleanup agent fixture");
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
		closeAgentSession(
			session: unknown,
			vm: unknown,
			sessionId: string,
		): Promise<void>;
	};
	_closedSessionIds: {
		has(sessionId: string): boolean;
		size: number;
		limit: number;
	};
	_sessions: Map<string, unknown>;
	_sessionClosePromises: Map<string, Promise<void>>;
	_closeSessionInternal(sessionId: string): Promise<void>;
	_sidecarSession: unknown;
	_sidecarVm: unknown;
};

type HostProcessRow = {
	pid: number;
	ppid: number;
};

function stubSessionEntry(sessionId: string): Record<string, unknown> {
	return {
		sessionId,
		agentType: "stub-agent",
		processId: "",
		pid: null,
		closed: false,
		modes: null,
		configOptions: [],
		capabilities: {},
		agentInfo: null,
		highestSequenceNumber: null,
		events: [],
		eventHandlers: new Set(),
		permissionHandlers: new Set(),
		configOverrides: new Map(),
		pendingPermissionReplies: new Map(),
	};
}

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
	await waitForTrackedSessionClose(vm, sessionId);
}

async function waitForTrackedSessionClose(
	vm: AgentOs,
	sessionId: string,
): Promise<void> {
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
	return collectProcessTree(readHostProcesses(), rootPid);
}

function collectProcessTree(
	rows: HostProcessRow[],
	rootPid: number,
): number[] {
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

	const backdoor = vm as SidecarBackdoor;
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
	const pids = [...new Set((await readKernelProcesses(vm)).map(({ pid }) => pid))];
	const links = (await Promise.all(pids.map((pid) => listFdLinks(pid)))).flat();
	return {
		pids,
		processCount: pids.length,
		fdCount: links.length,
		socketCount: links.filter((link) => link.startsWith("socket:[")).length,
	};
}

async function snapshotHostProcessResources(rootPid: number): Promise<{
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

async function zombieTimerCount(vm: AgentOs): Promise<number> {
	if (!(getAgentOsKernel(vm) instanceof NativeSidecarKernelProxy)) {
		return getAgentOsKernel(vm).zombieTimerCount;
	}

	const backdoor = vm as SidecarBackdoor;
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
		if (req.method !== "POST" || req.url !== "/v1/messages") {
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

async function createSlowResponseMock(mockKind: MockKind): Promise<{
	url: string;
	stop: () => Promise<void>;
	waitForRequest: () => Promise<void>;
}> {
	if (mockKind !== "openai") {
		throw new Error(`slow-response mock is unsupported for ${mockKind}`);
	}

	const requestSignal = createDeferredSignal();
	const server = createServer(async (req, res) => {
		if (req.method !== "POST" || req.url !== "/v1/responses") {
			writeJson(res, 404, { error: "not_found" });
			return;
		}

		try {
			await readJsonBody(req);
			requestSignal.resolve();
			await new Promise((resolve) => setTimeout(resolve, 60_000));
			writeJson(res, 200, {
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
			});
		} catch (error) {
			writeJson(res, 500, {
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

async function assertActivePromptCleanup(
	agent: SessionCleanupAgent,
): Promise<void> {
	const promptMock = await createActivePromptMock(agent);
	const vm = await agent.createVm(promptMock.url);
	try {
		const baselineSessionCount = vm.listSessions().length;
		const baselineZombieTimers = await zombieTimerCount(vm);
		const baselineVmResources = await snapshotVmResources(vm);
		const { sessionId } = await agent.createSession(vm, promptMock.url);
		const sessionState = await getSessionState(vm, sessionId);
		expect(sessionState.pid).toBeTypeOf("number");

		const promptPromise = vm.prompt(sessionId, PROMPT_TEXT);
		await promptMock.waitForRequest();
		const resourcesBeforeClose = await snapshotHostProcessResources(
			sessionState.pid!,
		);
		expect(resourcesBeforeClose.pids).toContain(sessionState.pid!);
		expect(resourcesBeforeClose.fdLinks.length).toBeGreaterThan(0);
		expect(resourcesBeforeClose.socketLinks.length).toBeGreaterThan(0);

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
		expect(vm.listSessions()).toHaveLength(baselineSessionCount);
		expect(
			vm.listSessions().some((entry) => entry.sessionId === sessionId),
		).toBe(false);
		await assertSessionResourcesReleased(
			[sessionState.pid!],
			baselineZombieTimers,
			vm,
			baselineVmResources,
		);
	} finally {
		await vm.dispose();
		await promptMock.stop();
	}
}

function registerSharedCleanupCoverage(agents: SessionCleanupAgent[]): void {
	test.each(agents)(
		"$label closeSession() frees session resources after a completed prompt and is idempotent",
		async (agent) => {
			const mock = await createTextMock(agent.mockKind);
			const vm = await agent.createVm(mock.url);
			try {
				const baselineSessionCount = vm.listSessions().length;
				const baselineZombieTimers = await zombieTimerCount(vm);
				const baselineVmResources = await snapshotVmResources(vm);
				const { sessionId } = await agent.createSession(vm, mock.url);
				const sessionState = await getSessionState(vm, sessionId);
				expect(sessionState.pid).toBeTypeOf("number");

				const { response, text } = await vm.prompt(sessionId, PROMPT_TEXT);
				expect(response.error).toBeUndefined();
				expect(text).toContain(PROMPT_RESPONSE);
				const resourcesBeforeClose = await snapshotSessionResources(
					vm,
					sessionState.pid!,
				);
				expect(resourcesBeforeClose.pids).toContain(sessionState.pid!);
				expect(resourcesBeforeClose.fdLinks.length).toBeGreaterThan(0);
				const vmResourcesBeforeClose = await snapshotVmResources(vm);
				expect(vmResourcesBeforeClose.processCount).toBeGreaterThanOrEqual(
					baselineVmResources.processCount + 1,
				);
				expect(vmResourcesBeforeClose.fdCount).toBeGreaterThan(
					baselineVmResources.fdCount,
				);

				await closeSessionAndWait(vm, sessionId);
				expect(vm.listSessions()).toHaveLength(baselineSessionCount);
				await assertSessionResourcesReleased(
					[sessionState.pid!],
					baselineZombieTimers,
					vm,
					baselineVmResources,
				);

				await expect(closeSessionAndWait(vm, sessionId)).resolves.toBeUndefined();
				expect(vm.listSessions()).toHaveLength(baselineSessionCount);
				await assertSessionResourcesReleased(
					[sessionState.pid!],
					baselineZombieTimers,
					vm,
					baselineVmResources,
				);
			} finally {
				await vm.dispose();
				await mock.stop();
			}
		},
		300_000,
	);

	test.each(agents)(
		"$label active-prompt cleanup frees sockets, FDs, and processes",
		async (agent) => assertActivePromptCleanup(agent),
		300_000,
	);
}

describe("session cleanup", () => {
	registerSharedCleanupCoverage(PI_AGENTS);

	test("closed session tombstones stay bounded across 10,000 sequential closes", async () => {
		const vm = await AgentOs.create();
		const backdoor = vm as SidecarBackdoor;
		const originalCloseAgentSession =
			backdoor._sidecarClient.closeAgentSession.bind(backdoor._sidecarClient);
		backdoor._sidecarClient.closeAgentSession = async () => {};

		try {
			const retentionLimit = backdoor._closedSessionIds.limit;
			const closedSessionCount = 10_000;
			expect(retentionLimit).toBeGreaterThan(0);
			expect(closedSessionCount).toBeGreaterThan(retentionLimit);

			for (let index = 0; index < closedSessionCount; index += 1) {
				const sessionId = `synthetic-session-${index}`;
				backdoor._sessions.set(sessionId, stubSessionEntry(sessionId));
				await backdoor._closeSessionInternal(sessionId);
			}

			expect(backdoor._closedSessionIds.size).toBeLessThanOrEqual(
				retentionLimit,
			);

			const recentSessionId = `synthetic-session-${closedSessionCount - 1}`;
			expect(backdoor._closedSessionIds.has(recentSessionId)).toBe(true);
			expect(() => vm.closeSession(recentSessionId)).not.toThrow();

			const evictedSessionId = "synthetic-session-0";
			expect(backdoor._closedSessionIds.has(evictedSessionId)).toBe(false);
			expect(() => vm.closeSession(evictedSessionId)).toThrow(
				`Session not found: ${evictedSessionId}`,
			);
		} finally {
			backdoor._sidecarClient.closeAgentSession = originalCloseAgentSession;
			await vm.dispose();
		}
	}, 30_000);

	test("Pi SDK returns to baseline after five sequential createSession()/closeSession() cycles", async () => {
		const agent = PI_AGENTS[0];
		const mock = await createTextMock(agent.mockKind);
		const vm = await agent.createVm(mock.url);
		try {
			const baselineSessionCount = vm.listSessions().length;
			const baselineZombieTimers = await zombieTimerCount(vm);
			const baselineVmResources = await snapshotVmResources(vm);

			for (let index = 0; index < 5; index += 1) {
				const { sessionId } = await agent.createSession(vm, mock.url);
				const sessionState = await getSessionState(vm, sessionId);
				expect(sessionState.pid).toBeTypeOf("number");
				const { response, text } = await vm.prompt(sessionId, PROMPT_TEXT);
				expect(response.error).toBeUndefined();
				expect(text).toContain(PROMPT_RESPONSE);

				await closeSessionAndWait(vm, sessionId);
				expect(vm.listSessions()).toHaveLength(baselineSessionCount);
				await assertSessionResourcesReleased(
					[sessionState.pid!],
					baselineZombieTimers,
					vm,
					baselineVmResources,
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
			const baselineSessionCount = vm.listSessions().length;
			const baselineZombieTimers = await zombieTimerCount(vm);
			const baselineVmResources = await snapshotVmResources(vm);
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
			expect(vm.listSessions()).toHaveLength(baselineSessionCount);
			await assertSessionResourcesReleased(
				dedicatedSessionPids,
				baselineZombieTimers,
				vm,
				baselineVmResources,
			);
		} finally {
			await vm.dispose();
			await mock.stop();
		}
	}, 300_000);
});

describe("session cleanup with registry-backed agents", () => {
	registerSharedCleanupCoverage(REGISTRY_AGENTS);

	test(
		"Codex active-prompt cleanup frees sockets, FDs, and processes",
		async () => {
			await assertActivePromptCleanup(CODEX_CLEANUP_AGENT);
		},
		300_000,
	);
});
