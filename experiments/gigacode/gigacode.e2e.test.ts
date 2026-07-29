import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import {
	mkdir,
	mkdtemp,
	open,
	readFile,
	rm,
	writeFile,
} from "node:fs/promises";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import { homedir, tmpdir } from "node:os";
import { resolve } from "node:path";
import { type Fixture, LLMock } from "@copilotkit/llmock";
import { createOpencodeClient } from "@opencode-ai/sdk";
import { createOpencodeClient as createV2OpencodeClient } from "@opencode-ai/sdk/v2";
import { createClient } from "@rivet-dev/agentos/client";
import { afterAll, afterEach, beforeAll, describe, expect, test } from "vitest";
import { TmuxTerminal } from "./tmux-terminal";

const ROOT = resolve(import.meta.dirname, "../..");
const NATIVE_SESSION_PATH = ROOT.replace(/^\/+/, "");
const ENTRYPOINT = resolve(import.meta.dirname, "gigacode.ts");
const TSX_IMPORT = createRequire(import.meta.url).resolve("tsx");
const OPENCODE_BIN = resolve(import.meta.dirname, "node_modules/.bin/opencode");
const GLOBAL_GIGACODE_BIN = resolve(homedir(), ".local/bin/gigacode");
const RESPONSE_TEXT = "GIGACODE_E2E_OK";
const GENERATED_SESSION_TITLE = "ACP session title generation";
const QUEUE_FIRST_RESPONSE = "GIGACODE_QUEUE_FIRST_OK";
const QUEUE_SECOND_RESPONSE = "GIGACODE_QUEUE_SECOND_OK";
const CANCEL_FOLLOWUP_RESPONSE = "GIGACODE_CANCEL_FOLLOWUP_OK";
const TOOL_FINAL_RESPONSE = "GIGACODE_TOOL_FINAL_OK";
const EDIT_TOOL_FINAL_RESPONSE = "GIGACODE_EDIT_TOOL_FINAL_OK";
const BASH_TOOL_FINAL_RESPONSE = "GIGACODE_BASH_TOOL_FINAL_OK";
const BASH_TOOL_OUTPUT = "GIGACODE_BASH_TOOL_EXECUTION_OK";
const BASH_TOOL_COMMAND = `printf ${BASH_TOOL_OUTPUT}`;
const BASH_TOOL_ITERATIONS = Number(
	process.env.GIGACODE_E2E_BASH_TOOL_ITERATIONS ?? "1",
);
const OPENCODE_PATCH_FINAL_RESPONSE = "GIGACODE_OPENCODE_PATCH_FINAL_OK";
const OPENCODE_PATCH_CONTENT = "GIGACODE_OPENCODE_PATCH_OK";
const TOOL_PROBE_FILENAME = `.gigacode-permission-probe-${process.pid}.txt`;
const TOOL_PROBE_PATH = `/workspace/${TOOL_PROBE_FILENAME}`;
const EDIT_TOOL_PROBE_FILENAME = `.gigacode-edit-probe-${process.pid}.txt`;
const EDIT_TOOL_PROBE_PATH = `/workspace/${EDIT_TOOL_PROBE_FILENAME}`;
const OPENCODE_PATCH_PROBE_FILENAME = `.gigacode-opencode-patch-${process.pid}.txt`;
const OPENCODE_PATCH_PROBE_PATH = `/workspace/${OPENCODE_PATCH_PROBE_FILENAME}`;
const TUI_QUEUE_FIRST_RESPONSE = "GIGACODE_TUI_QUEUE_FIRST_OK";
const TUI_QUEUE_SECOND_RESPONSE = "GIGACODE_TUI_QUEUE_SECOND_OK";
const MAX_DAEMON_LOG_BYTES = 1024 * 1024;

type RunningDaemon = {
	apiUrl: string;
	env: NodeJS.ProcessEnv;
	logs: () => string;
	stateDir: string;
};

type StartedGigacode = {
	cliOutput: string;
	daemon: RunningDaemon;
};

function rivetClient(endpoint: string): any {
	return createClient<never>({ endpoint } as never) as any;
}

async function freePort(): Promise<number> {
	const server = createServer();
	await new Promise<void>((resolveReady, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", resolveReady);
	});
	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("could not allocate a local test port");
	}
	await new Promise<void>((resolveClosed, reject) =>
		server.close((error) => (error ? reject(error) : resolveClosed())),
	);
	return address.port;
}

function appendLog(current: string, chunk: Buffer): string {
	const next = current + chunk.toString("utf8");
	return next.length > MAX_DAEMON_LOG_BYTES
		? next.slice(-MAX_DAEMON_LOG_BYTES)
		: next;
}

async function within<T>(
	promise: Promise<T>,
	label: string,
	daemon: RunningDaemon,
	timeoutMs = 30_000,
): Promise<T> {
	let timer: NodeJS.Timeout | undefined;
	try {
		return await Promise.race([
			promise,
			new Promise<T>((_, reject) => {
				timer = setTimeout(
					() =>
						reject(
							new Error(
								`${label} timed out after ${timeoutMs}ms:\n${daemon.logs()}`,
							),
						),
					timeoutMs,
				);
			}),
		]);
	} finally {
		if (timer) clearTimeout(timer);
	}
}

async function startGigacode(mockUrl: string): Promise<StartedGigacode> {
	const apiPort = await freePort();
	const rivetPort = await freePort();
	const stateDir = await mkdtemp(resolve(tmpdir(), "gigacode-e2e-"));
	const claudeConfigDir = resolve(stateDir, "host-claude");
	const codexHome = resolve(stateDir, "host-codex");
	const piHome = resolve(stateDir, "host-pi");
	const opencodeConfigDir = resolve(stateDir, "host-opencode-config");
	const opencodeDataDir = resolve(stateDir, "host-opencode-data");
	await mkdir(claudeConfigDir);
	await mkdir(codexHome);
	await mkdir(piHome);
	await mkdir(opencodeConfigDir);
	await mkdir(opencodeDataDir);
	if (process.env.GIGACODE_E2E_PRESEED_MODELS === "1") {
		await writeFile(
			resolve(stateDir, "models.json"),
			`${JSON.stringify({
				version: 2,
				updatedAt: Date.now(),
				providers: {
					claude: {
						defaultModel: "default",
						models: [
							{ id: "default", name: "Default (recommended)" },
							{ id: "sonnet", name: "Sonnet" },
						],
					},
					codex: {
						defaultModel: "gpt-5.4",
						models: [
							{ id: "gpt-5.4", name: "gpt-5.4" },
							{ id: "gpt-5.4-mini", name: "gpt-5.4-mini" },
						],
					},
					pi: {
						defaultModel: "anthropic/claude-sonnet-4-6",
						models: [
							{
								id: "anthropic/claude-sonnet-4-6",
								name: "anthropic/Claude Sonnet 4.6",
							},
							{
								id: "anthropic/claude-haiku-4-5",
								name: "anthropic/Claude Haiku 4.5",
							},
						],
						variants: {
							high: {
								name: "Thinking: high",
								configId: "thought_level",
								value: "high",
							},
						},
					},
					opencode: {
						defaultModel: "anthropic/claude-sonnet-4-20250514",
						models: [
							{
								id: "anthropic/claude-sonnet-4-20250514",
								name: "Claude Sonnet 4 LLMock",
							},
							{
								id: "opencode/big-pickle",
								name: "OpenCode Zen/Big Pickle",
							},
						],
						variants: {
							high: {
								name: "Thinking: high",
								configId: "variant",
								value: "high",
							},
						},
					},
				},
			}, null, 2)}\n`,
			{ mode: 0o600 },
		);
	}
	await writeFile(
		resolve(claudeConfigDir, ".credentials.json"),
		`${JSON.stringify({
			gigacodeTest: "claude-host-credentials",
			claudeAiOauth: {
				accessToken: "sk-ant-oat01-gigacode-e2e",
				refreshToken: "sk-ant-ort01-gigacode-e2e",
				expiresAt: Date.now() + 60 * 60 * 1_000,
				scopes: ["user:inference"],
				subscriptionType: "max",
				rateLimitTier: null,
			},
		})}\n`,
	);
	await writeFile(
		resolve(codexHome, "auth.json"),
		'{"gigacodeTest":"codex-host-credentials","OPENAI_API_KEY":"mock-key"}\n',
	);
	await writeFile(
		resolve(piHome, "settings.json"),
		'{"gigacodeTest":"pi-host-configuration"}\n',
	);
	await writeFile(
		resolve(opencodeConfigDir, "opencode.json"),
		`${JSON.stringify({
			$schema: "https://opencode.ai/config.json",
			autoupdate: false,
			share: "disabled",
			model: "anthropic/claude-sonnet-4-20250514",
			provider: {
				anthropic: {
					options: { baseURL: `${mockUrl}/v1` },
					models: {
						"claude-sonnet-4-20250514": {
							name: "Claude Sonnet 4 LLMock",
							variants: { low: {}, high: {} },
						},
					},
				},
			},
		})}\n`,
	);
	await writeFile(
		resolve(opencodeDataDir, "auth.json"),
		'{"anthropic":{"type":"api","key":"gigacode-opencode-mock-key"}}\n',
	);
	const sidecarPath =
		process.env.AGENTOS_SIDECAR_BIN ??
		resolve(ROOT, "target/debug/agentos-sidecar");
	if (!existsSync(sidecarPath)) {
		throw new Error(
			`Gigacode E2E requires ${sidecarPath}; build the native AgentOS sidecar first`,
		);
	}

	const mockPort = Number(new URL(mockUrl).port);
	const env: NodeJS.ProcessEnv = {
		...process.env,
		AGENTOS_SIDECAR_BIN: sidecarPath,
		GIGACODE_PORT: String(apiPort),
		GIGACODE_RIVET_PORT: String(rivetPort),
		GIGACODE_STATE_DIR: stateDir,
		GIGACODE_WORKSPACE: ROOT,
		// Source-tree tests should exercise the packages just built in this checkout,
		// not a potentially stale global GigaCode installation. Packaged-install
		// validation can opt into its flat software directory explicitly.
		GIGACODE_SOFTWARE_DIR: process.env.GIGACODE_E2E_SOFTWARE_DIR,
		GIGACODE_LOOPBACK_EXEMPT_PORTS: String(mockPort),
		GIGACODE_NETWORK_PERMISSION: "allow",
		GIGACODE_CLAUDE_CONFIG_DIR: claudeConfigDir,
		GIGACODE_CODEX_HOME: codexHome,
		GIGACODE_PI_HOME: piHome,
		GIGACODE_OPENCODE_CONFIG_DIR: opencodeConfigDir,
		GIGACODE_OPENCODE_DATA_DIR: opencodeDataDir,
		GIGACODE_OPENCODE_BIN: OPENCODE_BIN,
		GIGACODE_DISABLE_OPEN_URL: "1",
		GIGACODE_SESSION_ENV_JSON: JSON.stringify({
			ANTHROPIC_API_KEY: "mock-key",
			ANTHROPIC_BASE_URL: mockUrl,
			OPENAI_API_KEY: "mock-key",
			OPENAI_BASE_URL: `${mockUrl}/v1`,
			CLAUDE_CODE_TRACE_ADAPTER_MESSAGES:
				process.env.GIGACODE_E2E_TRACE === "1" ? "1" : "0",
			...(process.env.GIGACODE_E2E_TRACE === "1"
				? {
						AGENTOS_DEBUG_HTTP_BRIDGE: "1",
						AGENTOS_NET_BRIDGE_TRACE: "1",
						DEBUG_CLAUDE_AGENT_SDK: "1",
						CLAUDE_CODE_DEBUG_LOGS_DIR: "/home/agentos/.claude/debug",
						OPENCODE_PRINT_LOGS: "1",
						OPENCODE_LOG_LEVEL: "DEBUG",
					}
				: {}),
			GIGACODE_TUI_SHELL_VALUE: "GIGACODE_TUI_SHELL_OK",
		}),
		GIGACODE_PI_API_KEY: "mock-key",
		GIGACODE_PI_BASE_URL: mockUrl,
	};
	const daemon: RunningDaemon = {
		apiUrl: `http://127.0.0.1:${apiPort}/opencode`,
		env,
		logs: () => {
			try {
				return readFileSync(resolve(stateDir, "daemon.log"), "utf8");
			} catch (error) {
				if ((error as NodeJS.ErrnoException).code === "ENOENT") return "";
				throw error;
			}
		},
		stateDir,
	};
	const child = spawn(
		process.execPath,
		[
			"--import",
			TSX_IMPORT,
			ENTRYPOINT,
			"run",
			"--model",
			"claude/default",
			"Return the deterministic test response.",
		],
		{
			cwd: ROOT,
			stdio: ["ignore", "pipe", "pipe"],
			env,
		},
	);
	let output = "";
	let startupProgressSeen = false;
	let resolveStartupProgress!: () => void;
	let rejectStartupProgress!: (error: Error) => void;
	const startupProgress = new Promise<void>(
		(resolveProgress, rejectProgress) => {
			resolveStartupProgress = resolveProgress;
			rejectStartupProgress = rejectProgress;
		},
	);
	const observeOutput = (chunk: Buffer) => {
		output = appendLog(output, chunk);
		if (
			!startupProgressSeen &&
			output.includes("[gigacode] loading AgentOS and RivetKit modules")
		) {
			startupProgressSeen = true;
			resolveStartupProgress();
		}
	};
	child.stdout?.on("data", (chunk: Buffer) => {
		observeOutput(chunk);
	});
	child.stderr?.on("data", (chunk: Buffer) => {
		observeOutput(chunk);
	});
	const exited = new Promise<number | null>((resolveExit, reject) => {
		child.once("error", reject);
		child.once("exit", (status) => {
			if (!startupProgressSeen) {
				rejectStartupProgress(
					new Error(
						`Gigacode exited before streaming daemon startup progress:\n${output}`,
					),
				);
			}
			resolveExit(status);
		});
	});
	await within(startupProgress, "live daemon startup progress", daemon, 60_000);
	expect(child.exitCode).toBeNull();
	const startupCatalogMarker =
		process.env.GIGACODE_E2E_PRESEED_MODELS === "1"
			? "[gigacode] model catalog is ready from cache"
			: "[gigacode] discovering models from AgentOS harnesses";
	await waitForCondition(
		async () => daemon.logs().includes(startupCatalogMarker),
		"startup model catalog",
		daemon,
		60_000,
	);
	expect(
		await fetch(`${daemon.apiUrl}/global/health`)
			.then((response) => response.ok)
			.catch(() => false),
	).toBe(false);

	const status = await within(
		exited,
		"gigacode run with daemon autospawn",
		daemon,
		240_000,
	).catch((error) => {
		child.kill("SIGKILL");
		return stopDaemon(daemon).then(
			() => Promise.reject(error),
			(cleanupError) =>
				Promise.reject(
					new Error(
						`${String(error)}\nstartup cleanup failed: ${String(cleanupError)}`,
					),
				),
		);
	});
	if (status !== 0) {
		const error = new Error(
			`gigacode run exited with status ${status}:\n${output}\n${daemon.logs()}`,
		);
		await stopDaemon(daemon).catch((cleanupError) => {
			error.message += `\nstartup cleanup failed: ${String(cleanupError)}`;
		});
		throw error;
	}
	const health = await fetch(`${daemon.apiUrl}/global/health`, {
		signal: AbortSignal.timeout(1_000),
	});
	if (!health.ok) {
		const error = new Error(
			`Gigacode client exited without a healthy daemon:\n${output}\n${daemon.logs()}`,
		);
		await stopDaemon(daemon).catch((cleanupError) => {
			error.message += `\nstartup cleanup failed: ${String(cleanupError)}`;
		});
		throw error;
	}
	return { cliOutput: output, daemon };
}

async function stopDaemon(daemon: RunningDaemon | undefined): Promise<void> {
	if (!daemon) return;
	await terminateDaemon(daemon);
	if (process.env.GIGACODE_E2E_KEEP_STATE !== "1") {
		await rm(daemon.stateDir, { recursive: true, force: true });
	}
}

async function terminateDaemon(daemon: RunningDaemon): Promise<void> {
	const health = await fetch(`${daemon.apiUrl}/global/health`)
		.then((response) => response.json() as Promise<{ rivetEndpoint?: string }>)
		.catch(() => undefined);
	const rawPid = await readFile(
		resolve(daemon.stateDir, "daemon.pid"),
		"utf8",
	).catch((error: NodeJS.ErrnoException) => {
		if (error.code === "ENOENT") return undefined;
		throw error;
	});
	if (!rawPid) return;
	const pid = Number(rawPid.trim());
	process.kill(pid, "SIGTERM");
	const deadline = Date.now() + 36_000;
	while (Date.now() < deadline) {
		try {
			process.kill(pid, 0);
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ESRCH") break;
			throw error;
		}
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	try {
		process.kill(pid, 0);
		throw new Error(`Gigacode daemon shutdown timed out:\n${daemon.logs()}`);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
	}
	if (health?.rivetEndpoint) {
		const engine = await fetch(`${health.rivetEndpoint}/health`, {
			signal: AbortSignal.timeout(1_000),
		}).catch(() => undefined);
		if (engine?.ok) {
			throw new Error(
				`Gigacode daemon exited but its Rivet engine remained at ${health.rivetEndpoint}`,
			);
		}
	}
}

async function restartDaemon(daemon: RunningDaemon): Promise<void> {
	await terminateDaemon(daemon);
	const log = await open(resolve(daemon.stateDir, "daemon.log"), "a", 0o600);
	const child = spawn(
		process.execPath,
		["--import", TSX_IMPORT, ENTRYPOINT, "daemon"],
		{
			cwd: ROOT,
			detached: true,
			stdio: ["ignore", log.fd, log.fd],
			env: daemon.env,
		},
	);
	child.unref();
	await log.close();
	const deadline = Date.now() + 120_000;
	while (Date.now() < deadline) {
		try {
			const response = await fetch(`${daemon.apiUrl}/global/health`, {
				signal: AbortSignal.timeout(1_000),
			});
			if (response.ok) {
				const health = (await response.json()) as {
					rivetStartupStage?: string;
				};
				if (health.rivetStartupStage === "Rivet runtime is ready") return;
			}
		} catch {
			// The old socket may still be closing or the restarted engine is starting.
		}
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	throw new Error(`Gigacode daemon did not restart:\n${daemon.logs()}`);
}

async function readSessionLog(
	daemon: RunningDaemon,
	sessionId: string,
	ready: (records: Array<Record<string, unknown>>) => boolean = () => true,
): Promise<Array<Record<string, unknown>>> {
	const path = resolve(daemon.stateDir, "session-logs", `${sessionId}.jsonl`);
	const deadline = Date.now() + 10_000;
	let raw = "";
	while (Date.now() < deadline) {
		try {
			raw = await readFile(path, "utf8");
			if (raw.trim()) {
				const records = raw
					.trim()
					.split("\n")
					.map((line) => JSON.parse(line) as Record<string, unknown>);
				if (ready(records)) return records;
			}
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
		}
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	throw new Error(`session log was not written at ${path}:\n${raw}`);
}

async function activeActorIds(daemon: RunningDaemon): Promise<string[]> {
	const healthResponse = await fetch(`${daemon.apiUrl}/global/health`);
	if (!healthResponse.ok) {
		throw new Error(`Gigacode health failed: ${healthResponse.status}`);
	}
	const health = (await healthResponse.json()) as { rivetEndpoint: string };
	const response = await fetch(
		`${health.rivetEndpoint}/actors?namespace=default&name=vm`,
	);
	if (!response.ok) {
		throw new Error(`Rivet actor list failed: ${response.status}`);
	}
	const body = (await response.json()) as {
		actors?: Array<{ actor_id: string; destroy_ts?: number | null }>;
	};
	return (body.actors ?? [])
		.filter((actor) => !actor.destroy_ts)
		.map((actor) => actor.actor_id)
		.sort();
}

async function sessionActorId(
	daemon: RunningDaemon,
	sessionId: string,
): Promise<string> {
	const records = await readSessionLog(daemon, sessionId, (items) =>
		items.some(
			(record) =>
				record.event === "rivet.actor.resolved" &&
				typeof record.actorId === "string",
		),
	);
	const actorId = records.find(
		(record) => record.event === "rivet.actor.resolved",
	)?.actorId;
	if (typeof actorId !== "string") {
		throw new Error(`session ${sessionId} did not log its workspace actor ID`);
	}
	return actorId;
}

async function waitForCondition(
	condition: () => Promise<boolean> | boolean,
	label: string,
	daemon: RunningDaemon,
	timeoutMs = 90_000,
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		if (await condition()) return;
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	throw new Error(`${label} timed out after ${timeoutMs}ms:\n${daemon.logs()}`);
}

async function replyToSessionPermission(
	daemon: RunningDaemon,
	sessionId: string,
): Promise<string> {
	let permissionId = "";
	await waitForCondition(
		async () => {
			const response = await fetch(`${daemon.apiUrl}/permission`);
			const permissions = (await response.json()) as Array<{
				id: string;
				sessionID: string;
			}>;
			permissionId =
				permissions.find((item) => item.sessionID === sessionId)?.id ?? "";
			return Boolean(permissionId);
		},
		"tool permission request",
		daemon,
		90_000,
	);
	const response = await fetch(
		`${daemon.apiUrl}/permission/${encodeURIComponent(permissionId)}/reply`,
		{
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ reply: "once" }),
		},
	);
	if (!response.ok) {
		throw new Error(
			`permission ${permissionId} reply failed: ${await response.text()}`,
		);
	}
	return permissionId;
}

async function replyToSessionPermissionIfRequested(
	daemon: RunningDaemon,
	sessionId: string,
	completion: PromiseLike<unknown>,
): Promise<string | undefined> {
	let settled = false;
	void Promise.resolve(completion).then(
		() => {
			settled = true;
		},
		() => {
			settled = true;
		},
	);
	let permissionId = "";
	await waitForCondition(
		async () => {
			const permissions = (await (
				await fetch(`${daemon.apiUrl}/permission`)
			).json()) as Array<{ id: string; sessionID: string }>;
			permissionId =
				permissions.find((item) => item.sessionID === sessionId)?.id ?? "";
			return Boolean(permissionId) || settled;
		},
		"tool permission or completion",
		daemon,
		90_000,
	);
	if (!permissionId) return undefined;
	const response = await fetch(
		`${daemon.apiUrl}/permission/${encodeURIComponent(permissionId)}/reply`,
		{
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ reply: "once" }),
		},
	);
	if (!response.ok) {
		throw new Error(
			`permission ${permissionId} reply failed: ${await response.text()}`,
		);
	}
	return permissionId;
}

async function postPromptAsync(
	daemon: RunningDaemon,
	sessionId: string,
	text: string,
	model = { providerID: "claude", modelID: "default" },
): Promise<Response> {
	return fetch(`${daemon.apiUrl}/session/${sessionId}/prompt_async`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({
			model,
			parts: [{ type: "text", text }],
		}),
	});
}

async function readSseUntil(
	response: Response,
	predicate: (value: unknown) => boolean,
	timeoutMs = 10_000,
): Promise<Array<{ id?: string; value: unknown }>> {
	if (!response.body) throw new Error("SSE response has no body");
	const reader = response.body.getReader();
	const decoder = new TextDecoder();
	const records: Array<{ id?: string; value: unknown }> = [];
	let buffer = "";
	const deadline = Date.now() + timeoutMs;
	try {
		while (Date.now() < deadline) {
			const remaining = deadline - Date.now();
			let timer: NodeJS.Timeout | undefined;
			const result = await Promise.race([
				reader.read(),
				new Promise<never>((_, reject) => {
					timer = setTimeout(
						() => reject(new Error("SSE read timed out")),
						remaining,
					);
				}),
			]).finally(() => {
				if (timer) clearTimeout(timer);
			});
			if (result.done) break;
			buffer += decoder.decode(result.value, { stream: true });
			for (;;) {
				const boundary = buffer.indexOf("\n\n");
				if (boundary === -1) break;
				const block = buffer.slice(0, boundary);
				buffer = buffer.slice(boundary + 2);
				const data = block
					.split("\n")
					.filter((line) => line.startsWith("data:"))
					.map((line) => line.slice(5).trimStart())
					.join("\n");
				if (!data) continue;
				const id = block
					.split("\n")
					.find((line) => line.startsWith("id:"))
					?.slice(3)
					.trim();
				const value: unknown = JSON.parse(data);
				records.push({ ...(id ? { id } : {}), value });
				if (predicate(value)) return records;
			}
		}
		throw new Error(
			`SSE predicate was not satisfied: ${JSON.stringify(records)}`,
		);
	} finally {
		await reader.cancel().catch(() => undefined);
	}
}

describe("Gigacode OpenCode remote API", () => {
	let mock: LLMock;
	let daemon: RunningDaemon;
	let cliOutput: string;
	let cliSessionId: string;
	let sdkSessionId: string;
	let piModelID: string;
	let piModelName: string;
	const harnessModelIDs = new Map<string, string>();
	const harnessVariantIDs = new Map<string, string>();

	beforeAll(async () => {
		const fixtures: Fixture[] = [
			{
				match: { userMessage: "GIGACODE_CANCEL_TOOL_STREAM" },
				response: {
					toolCalls: [
						{
							name: "bash",
							arguments: JSON.stringify({ command: "sleep 20" }),
						},
					],
				},
			},
			{
				match: {
					predicate: (request) =>
						request.messages.at(-1)?.role === "tool" &&
						JSON.stringify(request.messages).includes(OPENCODE_PATCH_CONTENT),
				},
				response: { content: OPENCODE_PATCH_FINAL_RESPONSE },
			},
			{
				match: { userMessage: "GIGACODE_OPENCODE_PATCH_TOOL" },
				response: {
					toolCalls: [
						{
							name: "write",
							arguments: JSON.stringify({
								filePath: OPENCODE_PATCH_PROBE_PATH,
								content: OPENCODE_PATCH_CONTENT,
							}),
						},
					],
				},
			},
			{
				match: {
					predicate: (request) =>
						request.messages.at(-1)?.role === "tool" &&
						JSON.stringify(request.messages).includes("GIGACODE_EDIT_AFTER"),
				},
				response: { content: EDIT_TOOL_FINAL_RESPONSE },
			},
			{
				match: {
					predicate: (request) =>
						request.messages.at(-1)?.role === "tool" &&
						JSON.stringify(request.messages).includes(
							"GIGACODE_EDIT_TOOL_STREAM",
						) &&
						!JSON.stringify(request.messages).includes("GIGACODE_EDIT_AFTER"),
				},
				response: {
					toolCalls: [
						{
							name: "Edit",
							arguments: JSON.stringify({
								file_path: EDIT_TOOL_PROBE_PATH,
								old_string: "GIGACODE_EDIT_BEFORE",
								new_string: "GIGACODE_EDIT_AFTER",
							}),
						},
					],
				},
			},
			{
				match: { userMessage: "GIGACODE_EDIT_TOOL_STREAM" },
				response: {
					toolCalls: [
						{
							name: "Read",
							arguments: JSON.stringify({
								file_path: EDIT_TOOL_PROBE_PATH,
							}),
						},
					],
				},
			},
			{
				match: {
					predicate: (request) =>
						request.messages.at(-1)?.role === "tool" &&
						JSON.stringify(request.messages).includes("GIGACODE_BASH_TOOL_STREAM"),
				},
				response: { content: BASH_TOOL_FINAL_RESPONSE },
			},
			{
				match: { userMessage: "GIGACODE_BASH_TOOL_STREAM" },
				response: {
					toolCalls: [
						{
							name: "bash",
							arguments: JSON.stringify({
								command: BASH_TOOL_COMMAND,
							}),
						},
					],
				},
			},
			{
				match: {
					predicate: (request) =>
						request.messages.at(-1)?.role === "tool" &&
						JSON.stringify(request.messages).includes("GIGACODE_TOOL_STREAM"),
				},
				response: { content: TOOL_FINAL_RESPONSE },
			},
			{
				match: { userMessage: "GIGACODE_TOOL_STREAM" },
				response: {
					toolCalls: [
						{
							name: "Write",
							arguments: JSON.stringify({
								file_path: TOOL_PROBE_PATH,
								content: "GIGACODE_TOOL_EXECUTION_OK",
							}),
						},
					],
				},
				chunkSize: 8,
				streamingProfile: { tps: 50 },
			},
			{
				match: { userMessage: "GIGACODE_QUEUE_FIRST" },
				response: { content: QUEUE_FIRST_RESPONSE },
				latency: 750,
			},
			{
				match: { userMessage: "GIGACODE_QUEUE_SECOND" },
				response: { content: QUEUE_SECOND_RESPONSE },
			},
			{
				match: {
					predicate: (request) =>
						JSON.stringify(request.messages.at(-1)).includes(
							"GIGACODE_CANCEL_FOLLOWUP",
						),
				},
				response: { content: CANCEL_FOLLOWUP_RESPONSE },
			},
			{
				match: { userMessage: "GIGACODE_CANCEL_SLOW" },
				response: { content: "THIS_RESPONSE_MUST_BE_CANCELLED" },
				latency: 10_000,
			},
			{
				match: { userMessage: "GIGACODE_TUI_CANCEL" },
				response: { content: "THIS_TUI_RESPONSE_MUST_BE_CANCELLED" },
				latency: 10_000,
			},
			{
				match: { userMessage: "GIGACODE_TUI_QUEUE_FIRST" },
				response: { content: TUI_QUEUE_FIRST_RESPONSE },
				latency: 5_000,
			},
			{
				match: { userMessage: "GIGACODE_TUI_QUEUE_SECOND" },
				response: { content: TUI_QUEUE_SECOND_RESPONSE },
			},
			{
				match: { userMessage: "GIGACODE_TEXT_DELTA_STREAM" },
				response: { content: RESPONSE_TEXT },
				chunkSize: 1,
				streamingProfile: { ttft: 25, tps: 4_000 },
			},
			{
				match: {
					predicate: (request) =>
						JSON.stringify(request.messages).includes(
							"Generate a title for this conversation:",
						),
				},
				response: { content: GENERATED_SESSION_TITLE },
			},
			{
				match: { predicate: () => true },
				response: { content: RESPONSE_TEXT },
			},
		];
		mock = new LLMock({ port: 0, logLevel: "silent" });
		mock.addFixtures(fixtures);
		const mockUrl = await mock.start();
		({ cliOutput, daemon } = await startGigacode(mockUrl));
		const providers = await createOpencodeClient({
			baseUrl: daemon.apiUrl,
		}).provider.list();
		for (const provider of providers.data?.all ?? []) {
			const models = Object.values(
				(provider as { models?: Record<string, { id?: string; name?: string; variants?: Record<string, unknown> }> })
					.models ?? {},
			);
			const selected =
				provider.id === "opencode"
					? (models.find(
							(model) =>
								model.id ===
								providers.data?.default?.[provider.id],
						) ?? models.find((model) => model.id?.startsWith("anthropic/")))
					: provider.id === "codex"
						? models.find((model) => model.id?.includes("codex"))
						: provider.id === "pi"
							? (models.find(
									(model) =>
										model.id === "anthropic/claude-3-5-haiku-latest",
								) ?? models.find((model) => model.id?.startsWith("anthropic/")))
							: models.find((model) => model.id === "default");
			const model = selected ?? models[0];
			if (model?.id) {
				harnessModelIDs.set(provider.id, model.id);
				if (provider.id === "pi") {
					piModelID = model.id;
					piModelName = model.name ?? model.id;
				}
			}
			const variants = Object.keys(model?.variants ?? {});
			if (variants.length > 0) {
				harnessVariantIDs.set(
					provider.id,
					variants.find((variant) => variant === "high") ?? variants[0],
				);
			}
		}
		const sessions = (await (
			await fetch(
				`${daemon.apiUrl}/session?directory=${encodeURIComponent(ROOT)}`,
			)
		).json()) as Array<{ id: string }>;
		cliSessionId = sessions[0]?.id as string;
	}, 300_000);

	afterAll(async () => {
		await stopDaemon(daemon);
		await mock?.stop();
		await rm(resolve(ROOT, TOOL_PROBE_FILENAME), { force: true });
		await rm(resolve(ROOT, EDIT_TOOL_PROBE_FILENAME), { force: true });
		await rm(resolve(ROOT, OPENCODE_PATCH_PROBE_FILENAME), { force: true });
	}, 45_000);

	afterEach(async () => {
		if (!daemon) return;
		const response = await fetch(`${daemon.apiUrl}/session`, {
			signal: AbortSignal.timeout(5_000),
		}).catch(() => undefined);
		if (!response?.ok) return;
		const sessions = (await response.json()) as Array<{ id: string }>;
		const retained = new Set(
			[cliSessionId, sdkSessionId].filter(
				(sessionId): sessionId is string => typeof sessionId === "string",
			),
		);
		await Promise.allSettled(
			sessions
				.filter((session) => !retained.has(session.id))
				.map((session) =>
					fetch(`${daemon.apiUrl}/session/${session.id}`, {
						method: "DELETE",
						signal: AbortSignal.timeout(15_000),
					}),
				),
		);
	}, 45_000);

	test("autospawns the daemon and prompts through the Gigacode CLI", async () => {
		expect(cliOutput).toContain(RESPONSE_TEXT);
		expect(cliOutput).toContain("[gigacode] starting the local daemon; log:");
		expect(cliOutput).toContain(
			"[gigacode] loading AgentOS and RivetKit modules",
		);
		expect(cliOutput).toContain(
			"[gigacode] SQLite session coordinator is ready",
		);
		expect(cliOutput).toContain(
			process.env.GIGACODE_E2E_PRESEED_MODELS === "1"
				? "[gigacode] model catalog is ready from cache"
				: "[gigacode] discovering models from AgentOS harnesses",
		);
		expect(cliOutput).toContain("[gigacode] model catalog is ready");
		expect(cliOutput).not.toContain('"service":"gigacode"');
		const startupOutput = daemon.logs();
		expect(startupOutput).toContain("[gigacode] OpenCode API is listening at");
		expect(startupOutput).toContain(
			"[gigacode] loading AgentOS and RivetKit modules",
		);
		expect(startupOutput).toContain(
			"[gigacode] SQLite session coordinator is ready",
		);
		const catalogReadyAt = startupOutput.indexOf(
			"[gigacode] model catalog is ready",
		);
		const apiListeningAt = startupOutput.indexOf(
			"[gigacode] OpenCode API is listening at",
		);
		expect(catalogReadyAt).toBeGreaterThan(-1);
		expect(apiListeningAt).toBeGreaterThan(catalogReadyAt);
		expect(mock.getRequests().length).toBeGreaterThan(0);
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const listed = await within(
			client.session.list(),
			"CLI session.list",
			daemon,
		);
		expect(listed.error).toBeUndefined();
		expect(listed.data).toHaveLength(1);
		expect(
			(listed.data?.[0] as { providerID?: string } | undefined)?.providerID,
		).toBe("claude");
		expect(listed.data?.[0]?.id).toBe(cliSessionId);
	}, 90_000);

	test("serves the exact v2 SDK bootstrap, file search, and debugger command", async () => {
		const client = createV2OpencodeClient({
			baseUrl: daemon.apiUrl,
			directory: ROOT,
		});
		const health = await within(
			client.global.health(),
			"v2 global.health",
			daemon,
		);
		expect(health.error).toBeUndefined();
		expect(health.data?.healthy).toBe(true);
		const providers = await within(
			client.provider.list(),
			"v2 provider.list",
			daemon,
		);
		expect(providers.error).toBeUndefined();
		expect(providers.data?.all).toHaveLength(4);
		expect(providers.data?.all[0]?.source).toBe("env");
		const files = await within(
			client.v2.fs.find({
				location: { directory: ROOT },
				query: "gigacode.ts",
				type: "file",
				limit: "10",
			}),
			"v2 fs.find",
			daemon,
		);
		expect(files.error).toBeUndefined();
		expect(files.data?.data.map((entry) => entry.path)).toContain(
			"experiments/gigacode/gigacode.ts",
		);
		const commands = await within(
			client.command.list(),
			"v2 command.list",
			daemon,
		);
		expect(commands.data).toContainEqual(
			expect.objectContaining({ name: "gigacode-debugger" }),
		);
		const sessions = await client.session.list();
		const commandSessionId = sessions.data?.[0]?.id;
		expect(commandSessionId).toBeTruthy();
		const executed = await fetch(
			`${daemon.apiUrl}/session/${commandSessionId}/command`,
			{
				method: "POST",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({
					command: "gigacode-debugger",
					arguments: "",
				}),
			},
		);
		expect(executed.ok, await executed.clone().text()).toBe(true);
		expect(await executed.text()).toContain("Rivet inspector");
		const diff = await fetch(`${daemon.apiUrl}/vcs/diff?mode=git`);
		expect(diff.ok).toBe(true);
		expect(await diff.json()).toEqual([]);
	}, 30_000);

	test("replays SSE events by ID and filters session events by directory", async () => {
		const sessions = (await (
			await fetch(
				`${daemon.apiUrl}/session?directory=${encodeURIComponent(ROOT)}`,
			)
		).json()) as Array<{ id: string }>;
		const rootSessionId = sessions[0]?.id;
		expect(rootSessionId).toBeTruthy();
		const eventUrl = `${daemon.apiUrl}/event?directory=${encodeURIComponent(ROOT)}`;
		const createdResponse = await fetch(eventUrl);
		const createdEvents = readSseUntil(createdResponse, (value) => {
			const event = value as {
				type?: string;
				properties?: { sessionID?: string; info?: { title?: string } };
			};
			return (
				event.type === "session.created" &&
				event.properties?.info?.title === "SSE created"
			);
		});
		const createdSession = (await (
			await fetch(eventUrl.replace("/event?", "/session?"), {
				method: "POST",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({ title: "SSE created" }),
			})
		).json()) as { id: string };
		const createdEvent = (await createdEvents).find((record) => {
			const event = record.value as {
				type?: string;
				properties?: { sessionID?: string };
			};
			return event.type === "session.created";
		});
		expect(
			(createdEvent?.value as { properties?: { sessionID?: string } })
				.properties?.sessionID,
		).toBe(createdSession.id);
		await fetch(`${daemon.apiUrl}/session/${createdSession.id}`, {
			method: "DELETE",
		});
		const firstResponse = await fetch(eventUrl);
		const firstEvents = readSseUntil(firstResponse, (value) => {
			const event = value as {
				type?: string;
				properties?: { info?: { title?: string } };
			};
			return (
				event.type === "session.updated" &&
				event.properties?.info?.title === "SSE first"
			);
		});
		await fetch(`${daemon.apiUrl}/session/${rootSessionId}`, {
			method: "PATCH",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ title: "SSE first" }),
		});
		const firstRecords = await firstEvents;
		const firstUpdated = firstRecords.find((record) => {
			const event = record.value as { type?: string };
			return event.type === "session.updated";
		});
		expect(firstUpdated?.id).toBeTruthy();
		expect((firstUpdated?.value as { id?: string } | undefined)?.id).toBe(
			firstUpdated?.id,
		);

		await fetch(`${daemon.apiUrl}/session/${rootSessionId}`, {
			method: "PATCH",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ title: "SSE replayed" }),
		});
		const replayResponse = await fetch(eventUrl, {
			headers: { "last-event-id": firstUpdated?.id as string },
		});
		const replayed = await readSseUntil(replayResponse, (value) => {
			const event = value as {
				type?: string;
				properties?: { info?: { title?: string } };
			};
			return (
				event.type === "session.updated" &&
				event.properties?.info?.title === "SSE replayed"
			);
		});
		expect(
			replayed.some(
				(record) => Number(record.id) > Number(firstUpdated?.id as string),
			),
		).toBe(true);

		const otherDirectory = resolve(daemon.stateDir, "sse-other");
		await mkdir(otherDirectory);
		const otherSession = (await (
			await fetch(
				`${daemon.apiUrl}/session?directory=${encodeURIComponent(otherDirectory)}`,
				{
					method: "POST",
					headers: { "content-type": "application/json" },
					body: JSON.stringify({ title: "SSE other" }),
				},
			)
		).json()) as { id: string };
		const filteredResponse = await fetch(eventUrl);
		const filteredEvents = readSseUntil(filteredResponse, (value) => {
			const event = value as {
				type?: string;
				properties?: { info?: { title?: string } };
			};
			return (
				event.type === "session.updated" &&
				event.properties?.info?.title === "SSE root final"
			);
		});
		await fetch(`${daemon.apiUrl}/session/${otherSession.id}`, {
			method: "PATCH",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ title: "SSE must be filtered" }),
		});
		await fetch(`${daemon.apiUrl}/session/${rootSessionId}`, {
			method: "PATCH",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ title: "SSE root final" }),
		});
		const filtered = await filteredEvents;
		expect(JSON.stringify(filtered)).not.toContain("SSE must be filtered");
		const removed = await fetch(`${daemon.apiUrl}/session/${otherSession.id}`, {
			method: "DELETE",
		});
		expect(removed.ok).toBe(true);
	}, 60_000);

	test("returns native-compatible session metadata while idle", async () => {
		const sessionShape = {
			path: NATIVE_SESSION_PATH,
			cost: 0,
			tokens: {
				input: 0,
				output: 0,
				reasoning: 0,
				cache: { read: 0, write: 0 },
			},
		};
		const created = (await (
			await fetch(
				`${daemon.apiUrl}/session?directory=${encodeURIComponent(ROOT)}`,
				{
					method: "POST",
					headers: { "content-type": "application/json" },
					body: JSON.stringify({ title: "Session metadata compatibility" }),
				},
			)
		).json()) as { id: string };
		expect(created).toMatchObject(sessionShape);

		const retrieved = await fetch(
			`${daemon.apiUrl}/session/${created.id}?directory=${encodeURIComponent(ROOT)}`,
		).then((response) => response.json());
		expect(retrieved).toMatchObject(sessionShape);
		const listed = (await fetch(
			`${daemon.apiUrl}/session?directory=${encodeURIComponent(ROOT)}`,
		).then((response) => response.json())) as Array<{ id: string }>;
		expect(listed.find((session) => session.id === created.id)).toMatchObject(
			sessionShape,
		);
		expect(
			await fetch(`${daemon.apiUrl}/session/status`).then((response) =>
				response.json(),
			),
		).toEqual({});

		await fetch(`${daemon.apiUrl}/session/${created.id}`, { method: "DELETE" });
	}, 30_000);

	test("opens a shell in the cwd workspace actor without creating another", async () => {
		const actorsBefore = await activeActorIds(daemon);
		const child = spawn(
			process.execPath,
			["--import", TSX_IMPORT, ENTRYPOINT, "shell"],
			{
				cwd: ROOT,
				stdio: ["pipe", "pipe", "pipe"],
				env: daemon.env,
			},
		);
		let output = "";
		child.stdout?.on("data", (chunk: Buffer) => {
			output = appendLog(output, chunk);
		});
		child.stderr?.on("data", (chunk: Buffer) => {
			output = appendLog(output, chunk);
		});
		child.stdin?.end("printf 'GIGACODE_SHELL_OK\\n'\nexit\n");
		const status = await within(
			new Promise<number | null>((resolveExit, reject) => {
				child.once("error", reject);
				child.once("exit", resolveExit);
			}),
			"gigacode shell",
			daemon,
			120_000,
		);
		expect(status, output).toBe(0);
		expect(output).toContain("GIGACODE_SHELL_OK");

		const deadline = Date.now() + 10_000;
		let actorsAfter = await activeActorIds(daemon);
		while (
			Date.now() < deadline &&
			JSON.stringify(actorsAfter) !== JSON.stringify(actorsBefore)
		) {
			await new Promise((resolve) => setTimeout(resolve, 100));
			actorsAfter = await activeActorIds(daemon);
		}
		expect(actorsAfter).toEqual(actorsBefore);
	}, 120_000);

	test("lists harnesses, creates and lists a Rivet actor, and prompts through AgentOS", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const healthResponse = await fetch(`${daemon.apiUrl}/global/health`);
		expect(healthResponse.ok).toBe(true);
		const health = (await healthResponse.json()) as {
			rivetEndpoint: string;
			workspaceRoot: string;
			rivetStartupStage: string;
			modelCatalogStage: string;
		};
		expect(health).toMatchObject({
			workspaceRoot: "/workspace",
			rivetStartupStage: "Rivet runtime is ready",
		});

		expect(health.modelCatalogStage).toMatch(/^model catalog is ready/);
		const providers = await within(
			client.provider.list(),
			"provider.list",
			daemon,
		);
		expect(providers.error).toBeUndefined();
		expect(providers.data?.all.map((provider) => provider.id).sort()).toEqual([
			"claude",
			"codex",
			"opencode",
			"pi",
		]);
		for (const provider of providers.data?.all ?? []) {
			const models = Object.values(
				(
					provider as {
						models?: Record<
							string,
							{
								id?: string;
								name?: string;
								variants?: Record<string, Record<string, unknown>>;
							}
						>;
					}
				).models ?? {},
			);
			expect(
				models.length,
				`${provider.id} did not expose its native model catalog:\n${daemon.logs()}`,
			).toBeGreaterThan(provider.id === "codex" ? 0 : 1);
			const preferred =
				provider.id === "opencode"
					? models.find((model) => model.id?.startsWith("anthropic/"))
					: provider.id === "codex"
						? models.find((model) => model.id?.includes("codex"))
						: provider.id === "pi"
							? (models.find(
									(model) => model.id === "anthropic/claude-3-5-haiku-latest",
								) ?? models.find((model) => model.id?.startsWith("anthropic/")))
							: models.find((model) => model.id === "default");
			const selected = preferred ?? models[0];
			expect(selected?.id, `${provider.id} model IDs`).toBeTruthy();
			harnessModelIDs.set(provider.id, selected?.id as string);
			if (provider.id === "pi" || provider.id === "opencode") {
				const variants = Object.keys(selected?.variants ?? {});
				expect(
					variants.length,
					`${provider.id} did not expose ACP config selectors as variants`,
				).toBeGreaterThan(0);
				harnessVariantIDs.set(
					provider.id,
					variants.find((variant) => variant === "high") ??
						(variants[0] as string),
				);
			}
		}
		const piProvider = providers.data?.all.find(
			(provider) => provider.id === "pi",
		) as
			| { models?: Record<string, { id?: string; name?: string }> }
			| undefined;
		const piModels = Object.values(piProvider?.models ?? {});
		expect(piModels.length, daemon.logs()).toBeGreaterThan(1);
		expect(piModels.some((model) => model.id?.startsWith("anthropic/"))).toBe(
			true,
		);
		const piTestModel =
			piModels.find(
				(model) => model.id === "anthropic/claude-3-5-haiku-latest",
			) ??
			piModels.find((model) => model.id?.startsWith("anthropic/")) ??
			piModels[0];
		piModelID = piTestModel?.id as string;
		piModelName = piTestModel?.name as string;
		expect(piModelID).toBeTruthy();
		expect(piModelName).toBeTruthy();
		const cache = JSON.parse(
			await readFile(resolve(daemon.stateDir, "models.json"), "utf8"),
		) as { version?: number; providers?: Record<string, unknown> };
		expect(cache.version).toBe(2);
		expect(Object.keys(cache.providers ?? {}).sort()).toEqual([
			"claude",
			"codex",
			"opencode",
			"pi",
		]);

		const initial = await within(
			client.session.list(),
			"initial session.list",
			daemon,
		);
		expect(initial.error).toBeUndefined();
		expect(initial.data?.map((session) => session.id)).toEqual([cliSessionId]);

		const created = await within(
			client.session.create({ body: { title: "LLMock E2E" } }),
			"session.create",
			daemon,
			60_000,
		);
		expect(created.error, daemon.logs()).toBeUndefined();
		const sessionId = created.data?.id;
		expect(sessionId).toBeTruthy();
		expect(created.data).toMatchObject({
			path: NATIVE_SESSION_PATH,
			cost: 0,
			tokens: {
				input: 0,
				output: 0,
				reasoning: 0,
				cache: { read: 0, write: 0 },
			},
		});
		expect(
			await fetch(`${daemon.apiUrl}/session/status`).then((response) =>
				response.json(),
			),
		).toEqual({});
		sdkSessionId = sessionId as string;
		const sdkActorId = await sessionActorId(daemon, sdkSessionId);
		const cliActorId = await sessionActorId(daemon, cliSessionId);
		expect(sdkSessionId).not.toBe(sdkActorId);
		expect(sdkActorId).toBe(cliActorId);

		const listed = await within(
			client.session.list(),
			"populated session.list",
			daemon,
		);
		expect(listed.data?.map((session) => session.id)).toContain(sessionId);
		const coordinators = (await (
			await fetch(
				`${health.rivetEndpoint}/actors?namespace=default&name=coordinator`,
			)
		).json()) as {
			actors?: Array<{ actor_id: string; destroy_ts?: number | null }>;
		};
		const coordinatorId = coordinators.actors?.find(
			(actor) => !actor.destroy_ts,
		)?.actor_id;
		expect(coordinatorId).toBeTruthy();
		const coordinatorSessions = await rivetClient(health.rivetEndpoint)
			.coordinator.getForId(coordinatorId as string)
			.listSessions();
		expect(
			coordinatorSessions.map((session: { id: string }) => session.id),
		).toEqual(expect.arrayContaining([cliSessionId, sdkSessionId]));

		const actor = rivetClient(health.rivetEndpoint).vm.getForId(sdkActorId);
		expect(
			new TextDecoder().decode(
				await actor.readFile("/home/agentos/.claude/.credentials.json"),
			),
		).toContain("claude-host-credentials");
		expect(
			new TextDecoder().decode(
				await actor.readFile("/home/agentos/.codex/auth.json"),
			),
		).toContain("codex-host-credentials");
		expect(
			new TextDecoder().decode(
				await actor.readFile("/home/agentos/.pi/settings.json"),
			),
		).toContain("pi-host-configuration");
		// OpenCode receives bounded config/auth contents through its documented
		// environment inputs. Do not mount the full XDG data directory: it can
		// contain a multi-gigabyte opencode.db unrelated to ACP credentials.
		expect(
			await actor.exists("/home/agentos/.local/share/opencode/auth.json"),
		).toBe(false);
		expect(
			new TextDecoder().decode(
				await actor.readFile("/workspace/experiments/gigacode/package.json"),
			),
		).toContain("@rivet-dev/agentos-experiment-gigacode");
		const hostDirFdProbe = await actor.exec(
			`node -e "const fs=require('fs');const p='/home/agentos/.pi/host-dir-fd-probe';const fd=fs.openSync(p,'wx');fs.writeFileSync(fd,'first');fs.closeSync(fd);fs.appendFileSync(p,'-second');process.stdout.write(fs.readFileSync(p,'utf8'))"`,
		);
		expect(hostDirFdProbe, JSON.stringify(hostDirFdProbe)).toMatchObject({
			exitCode: 0,
			stdout: "first-second",
		});
		expect(await actor.exists("/host/etc/hosts")).toBe(false);
		for (const harness of ["claude", "codex", "pi"] as const) {
			await actor.writeFile(
				`/home/agentos/.${harness}/gigacode-write-through`,
				`${harness} credential refresh persistence`,
			);
			expect(
				await readFile(
					resolve(daemon.stateDir, `host-${harness}/gigacode-write-through`),
					"utf8",
				),
			).toBe(`${harness} credential refresh persistence`);
		}
		const modelEventResponse = await fetch(`${daemon.apiUrl}/event`);
		const modelEvents = readSseUntil(
			modelEventResponse,
			(value) => {
				const event = value as {
					type?: string;
					properties?: {
						sessionID?: string;
						info?: { model?: { id?: string } };
					};
				};
				return (
					event.type === "session.updated" &&
					event.properties?.sessionID === sessionId &&
					event.properties?.info?.model?.id === "default"
				);
			},
			90_000,
		);
		for (const turn of ["first", "second"]) {
			const prompted = await within(
				client.session.prompt({
					path: { id: sessionId as string },
					body: {
						model: { providerID: "claude", modelID: "default" },
						parts: [
							{
								type: "text",
								text: `Return the deterministic test response for the ${turn} turn.`,
							},
						],
					},
				}),
				`${turn} session.prompt`,
				daemon,
				60_000,
			).catch((error) => {
				throw new Error(
					`${String(error)}\nLLMock requests: ${JSON.stringify(mock.getRequests())}`,
				);
			});
			expect(prompted.error).toBeUndefined();
			expect(JSON.stringify(prompted.data)).toContain(RESPONSE_TEXT);
		}
		expect(
			(await modelEvents).some((record) => {
				const event = record.value as {
					type?: string;
					properties?: { sessionID?: string };
				};
				return (
					event.type === "session.updated" &&
					event.properties?.sessionID === sessionId
				);
			}),
		).toBe(true);
		expect(mock.getRequests().length).toBeGreaterThan(0);

		const messages = await within(
			client.session.messages({ path: { id: sessionId as string } }),
			"session.messages",
			daemon,
		);
		expect(messages.error).toBeUndefined();
		expect(messages.data?.map((message) => message.info.role)).toEqual([
			"user",
			"assistant",
			"user",
			"assistant",
		]);
		const messageIds =
			messages.data?.map((message) => message.info.id as string) ?? [];
		expect(messageIds).toEqual([...messageIds].sort());
		expect(
			messageIds.every((id) => /^msg_[0-9a-f]{12}[0-9A-Za-z]{14}$/.test(id)),
		).toBe(true);
		const records = await readSessionLog(
			daemon,
			sdkSessionId,
			(items) =>
				items.filter((record) => record.event === "prompt.completed").length ===
				2,
		);
		expect(
			records.filter((record) => record.event === "prompt.completed"),
		).toHaveLength(2);
		expect(
			records.filter((record) => record.event === "agentos.session.created"),
		).toHaveLength(1);
	}, 180_000);

	test("automatically names an untitled session after its first prompt", async () => {
		const createdResponse = await fetch(`${daemon.apiUrl}/session`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: "{}",
		});
		expect(createdResponse.status, await createdResponse.clone().text()).toBe(200);
		const created = (await createdResponse.json()) as { id: string; title: string };
		expect(created.title).toMatch(/^New session - /);
		const titleEvents = readSseUntil(
			await fetch(`${daemon.apiUrl}/event`),
			(value) => {
				const event = value as {
					type?: string;
					properties?: { sessionID?: string; info?: { title?: string } };
				};
				return (
					event.type === "session.updated" &&
					event.properties?.sessionID === created.id &&
					event.properties.info?.title === GENERATED_SESSION_TITLE
				);
			},
			90_000,
		);

		const prompted = await postPromptAsync(
			daemon,
			created.id,
			"Investigate ACP session title generation.",
		);
		expect(prompted.status, await prompted.clone().text()).toBe(204);
		await waitForCondition(
			async () => {
				const session = (await (
					await fetch(`${daemon.apiUrl}/session/${created.id}`)
				).json()) as { title?: string };
				return session.title === GENERATED_SESSION_TITLE;
			},
			"generated session title",
			daemon,
			90_000,
		);
		expect(
			(await titleEvents).some((record) => {
				const event = record.value as {
					type?: string;
					properties?: { sessionID?: string; info?: { title?: string } };
				};
				return (
					event.type === "session.updated" &&
					event.properties?.sessionID === created.id &&
					event.properties.info?.title === GENERATED_SESSION_TITLE
				);
			}),
		).toBe(true);
		const records = await readSessionLog(
			daemon,
			created.id,
			(items) =>
				items.some((record) => record.event === "session.title.generated"),
		);
		expect(
			records.some(
				(record) =>
					record.event === "session.title.generated" &&
					record.title === GENERATED_SESSION_TITLE,
			),
		).toBe(true);
	}, 120_000);

	test("keeps ACP sessions live when switching between GigaCode sessions", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const sessionIds: string[] = [];
		for (const title of ["First live ACP session", "Second live ACP session"]) {
			const created = await within(
				client.session.create({ body: { title } }),
				`${title} session.create`,
				daemon,
				60_000,
			);
			expect(created.error).toBeUndefined();
			sessionIds.push(created.data?.id as string);
		}
		const [firstSessionId, secondSessionId] = sessionIds as [string, string];
		for (const sessionId of [firstSessionId, secondSessionId, firstSessionId]) {
			const prompted = await within(
				client.session.prompt({
					path: { id: sessionId },
					body: {
						model: { providerID: "claude", modelID: "default" },
						parts: [
							{ type: "text", text: "Return the deterministic response." },
						],
					},
				}),
				`live session prompt ${sessionId}`,
				daemon,
				60_000,
			);
			expect(prompted.error).toBeUndefined();
		}

		const hasCreatedActorSession = (records: Array<Record<string, unknown>>) =>
			records.some((record) => record.event === "agentos.session.created");
		const firstLog = await readSessionLog(
			daemon,
			firstSessionId,
			hasCreatedActorSession,
		);
		const secondLog = await readSessionLog(
			daemon,
			secondSessionId,
			hasCreatedActorSession,
		);
		expect(
			firstLog.filter((record) => record.event === "agentos.session.created"),
		).toHaveLength(1);
		expect(
			secondLog.filter((record) => record.event === "agentos.session.created"),
		).toHaveLength(1);
		expect(
			[...firstLog, ...secondLog].some(
				(record) => record.event === "agentos.session.deactivated",
			),
		).toBe(false);

		for (const sessionId of sessionIds) {
			await client.session.delete({ path: { id: sessionId } });
		}
	}, 180_000);

	test("queues promptAsync turns in FIFO order and reports busy until both finish", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const created = await within(
			client.session.create({ body: { title: "FIFO E2E" } }),
			"FIFO session.create",
			daemon,
			60_000,
		);
		const sessionId = created.data?.id as string;
		const first = await postPromptAsync(
			daemon,
			sessionId,
			"GIGACODE_QUEUE_FIRST",
		);
		expect(first.status, await first.clone().text()).toBe(204);
		const second = await postPromptAsync(
			daemon,
			sessionId,
			"GIGACODE_QUEUE_SECOND",
		);
		expect(second.status, await second.clone().text()).toBe(204);

		const earlyStatus = (await (
			await fetch(`${daemon.apiUrl}/session/status`)
		).json()) as Record<string, { type: string }>;
		expect(earlyStatus[sessionId]?.type).toBe("busy");
		const earlyMessages = (await (
			await fetch(`${daemon.apiUrl}/session/${sessionId}/message`)
		).json()) as Array<{ info: { role: string }; parts: unknown[] }>;
		expect(earlyMessages.map((message) => message.info.role)).toEqual([
			"user",
			"assistant",
			"user",
		]);
		expect(earlyMessages[1]?.parts).toEqual([]);

		await waitForCondition(
			async () => {
				const statuses = (await (
					await fetch(`${daemon.apiUrl}/session/status`)
				).json()) as Record<string, { type: string }>;
				return statuses[sessionId] === undefined;
			},
			"FIFO session idle",
			daemon,
			120_000,
		);
		const completed = (await (
			await fetch(`${daemon.apiUrl}/session/${sessionId}/message`)
		).json()) as Array<{
			info: { id: string; role: string };
			parts: Array<{ type?: string; text?: string }>;
		}>;
		expect(completed[1]?.parts.map((part) => part.text).join("")).toContain(
			QUEUE_FIRST_RESPONSE,
		);
		expect(completed[3]?.parts.map((part) => part.text).join("")).toContain(
			QUEUE_SECOND_RESPONSE,
		);
		const log = await readSessionLog(
			daemon,
			sessionId,
			(records) =>
				records.filter((record) => record.event === "prompt.completed")
					.length === 2,
		);
		const completedPrompts = log.filter(
			(record) => record.event === "prompt.completed",
		);
		expect(completedPrompts).toHaveLength(2);
		expect(completedPrompts[0]?.messageId).toBe(completed[1]?.info.id);
		expect(completedPrompts[1]?.messageId).toBe(completed[3]?.info.id);
	}, 180_000);

	test("exposes and applies ACP config selectors as model variants", async () => {
		await waitForCondition(
			async () => {
				const health = (await fetch(`${daemon.apiUrl}/global/health`).then(
					(response) => response.json(),
				)) as { modelCatalogStage?: string };
				return health.modelCatalogStage?.startsWith("model catalog is ready") ?? false;
			},
			"model catalog refresh for variant assertions",
			daemon,
			180_000,
		);
		const providerResponse = await fetch(`${daemon.apiUrl}/provider`);
		expect(providerResponse.ok, await providerResponse.clone().text()).toBe(
			true,
		);
		const providers = (await providerResponse.json()) as {
			all?: Array<{
				id?: string;
				models?: Record<
					string,
					{
						id?: string;
						variants?: Record<string, Record<string, unknown>>;
					}
				>;
			}>;
		};
		const variantHarnesses =
			process.env.GIGACODE_E2E_PRESEED_MODELS === "1"
				? ["pi", "opencode"]
				: ["claude", "codex", "pi", "opencode"];
		for (const harness of variantHarnesses) {
			const provider = providers.all?.find((entry) => entry.id === harness);
			const variants = Object.values(provider?.models ?? {}).flatMap((model) =>
				Object.keys(model.variants ?? {}),
			);
			expect(
				variants.length,
				`${harness} did not expose ACP config selectors as variants`,
			).toBeGreaterThan(0);
		}

		const pi = providers.all?.find((entry) => entry.id === "pi");
		const model = Object.values(pi?.models ?? {}).find(
			(entry) => "high" in (entry.variants ?? {}),
		);
		expect(model?.id).toBeTruthy();
		const variant = Object.keys(model?.variants ?? {}).find(
			(candidate) => candidate === "high",
		);
		expect(variant).toBe("high");

		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const created = await client.session.create({
			body: { title: "ACP variant E2E" },
		});
		const sessionId = created.data?.id as string;
		const prompted = await within(
			client.session.prompt({
				path: { id: sessionId },
				body: {
					model: {
						providerID: "pi",
						modelID: model?.id as string,
					},
					variant,
					parts: [{ type: "text", text: "GIGACODE_VARIANT_E2E" }],
				} as any,
			}),
			"variant session.prompt",
			daemon,
			90_000,
		);
		expect(prompted.error, daemon.logs()).toBeUndefined();
		const messages = await client.session.messages({ path: { id: sessionId } });
		expect(
			(messages.data?.[0]?.info as { model?: { variant?: string } }).model
				?.variant,
		).toBe(variant);
		expect((messages.data?.[1]?.info as { variant?: string }).variant).toBe(
			variant,
		);
		const selected = await client.session.get({ path: { id: sessionId } });
		expect(
			(selected.data as { model?: { variant?: string } } | undefined)?.model
				?.variant,
		).toBe(variant);
		const log = await readSessionLog(daemon, sessionId, (records) =>
			records.some(
				(record) => record.event === "agentos.session.variant.selected",
			),
		);
		expect(
			log.find((record) => record.event === "agentos.session.variant.selected"),
		).toMatchObject({ variant, configId: "thought_level", value: "high" });
	}, 180_000);

	test("streams native message.part.delta text events", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const eventResponse = fetch(`${daemon.apiUrl}/event`);
		const created = await within(
			client.session.create({ body: { title: "Text delta E2E" } }),
			"text delta session.create",
			daemon,
			60_000,
		);
		const sessionId = created.data?.id as string;
		let streamedText = "";
		const deltas = eventResponse.then((response) =>
			readSseUntil(
				response,
				(value) => {
					const event = value as {
						type?: string;
						properties?: {
							sessionID?: string;
							delta?: string;
							field?: string;
						};
					};
					if (
						event.type !== "message.part.delta" ||
						event.properties?.sessionID !== sessionId ||
						event.properties.field !== "text" ||
						typeof event.properties.delta !== "string"
					) {
						return false;
					}
					streamedText += event.properties.delta;
					return streamedText.includes(RESPONSE_TEXT);
				},
				90_000,
			),
		);
		const prompted = await within(
			client.session.prompt({
				path: { id: sessionId },
				body: {
					model: { providerID: "claude", modelID: "default" },
					parts: [{ type: "text", text: "GIGACODE_TEXT_DELTA_STREAM" }],
				},
			}),
			"text delta session.prompt",
			daemon,
			90_000,
		);
		expect(prompted.error, daemon.logs()).toBeUndefined();
		expect(JSON.stringify(prompted.data)).toContain(RESPONSE_TEXT);
		expect(
			(await within(deltas, "native text delta event", daemon, 90_000)).some(
				(record) =>
					(record.value as { type?: string }).type === "message.part.delta",
			),
		).toBe(true);
	}, 180_000);

	test("streams a tool call, answers its permission, and records the result", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const eventResponse = fetch(`${daemon.apiUrl}/event`);
		const created = await within(
			client.session.create({ body: { title: "Tool and permission E2E" } }),
			"tool session.create",
			daemon,
			60_000,
		);
		const sessionId = created.data?.id as string;
		expect(created.data).toMatchObject({
			path: NATIVE_SESSION_PATH,
			cost: 0,
			tokens: {
				input: 0,
				output: 0,
				reasoning: 0,
				cache: { read: 0, write: 0 },
			},
		});
		expect(
			await fetch(`${daemon.apiUrl}/session/status`).then((response) =>
				response.json(),
			),
		).toEqual({});
		// Session creation flushes the SSE response, so the reader is registered
		// before the first prompt can emit its model-selection update.
		const modelSelected = readSseUntil(
			await eventResponse,
			(value) => {
				const event = value as {
					type?: string;
					properties?: {
						sessionID?: string;
						info?: { model?: { id?: string } };
					};
				};
				return (
					event.type === "session.updated" &&
					event.properties?.sessionID === sessionId &&
					event.properties?.info?.model?.id === "default"
				);
			},
			90_000,
		);
		const toolEvents = fetch(`${daemon.apiUrl}/event`).then((response) =>
			readSseUntil(
				response,
				(value) => {
					const event = value as {
						type?: string;
						properties?: {
							sessionID?: string;
							part?: { tool?: string; state?: { status?: string } };
						};
					};
					return (
						event.type === "message.part.updated" &&
						event.properties?.sessionID === sessionId &&
						event.properties.part?.tool === "write" &&
						event.properties.part.state?.status === "completed"
					);
				},
				90_000,
			),
		);
		const prompt = client.session.prompt({
			path: { id: sessionId },
			body: {
				model: { providerID: "claude", modelID: "default" },
				parts: [{ type: "text", text: "GIGACODE_TOOL_STREAM" }],
			},
		});
		// The prompt must stay in flight while permission polling and reply run.
		// Waiting for an unrelated SSE assertion first can deadlock the ACP turn at
		// its permission request and manufacture a prompt timeout.
		const permissionReply = replyToSessionPermission(daemon, sessionId);
		const [modelEvents, permissionId] = await Promise.all([
			within(
				modelSelected,
				"first model session.updated",
				daemon,
				90_000,
			),
			permissionReply,
		]);
		expect(
			modelEvents.some((record) => {
				const event = record.value as {
					type?: string;
					properties?: { sessionID?: string };
				};
				return (
					event.type === "session.updated" &&
					event.properties?.sessionID === sessionId
				);
			}),
		).toBe(true);
		expect(permissionId).toMatch(/^per_[0-9a-f]{32}$/);
		const result = await within(prompt, "tool session.prompt", daemon, 90_000);
		expect(result.error, daemon.logs()).toBeUndefined();
		expect(JSON.stringify(result.data)).toContain(TOOL_FINAL_RESPONSE);
		const streamedToolStatuses = (await within(
			toolEvents,
			"write tool event lifecycle",
			daemon,
			90_000,
		))
			.map((record) => record.value)
			.filter(
				(event: any) =>
					event.type === "message.part.updated" &&
					event.properties?.sessionID === sessionId &&
					event.properties?.part?.tool === "write",
			)
			.map((event: any) => event.properties.part.state.status)
			.filter((status, index, statuses) => status !== statuses[index - 1]);
		expect(streamedToolStatuses).toEqual(["pending", "running", "completed"]);
		const messages = await within(
			client.session.messages({ path: { id: sessionId } }),
			"tool session.messages",
			daemon,
		);
		const assistant = messages.data?.[1];
		const tool = assistant?.parts.find((part) => part.type === "tool") as
			| {
					tool?: string;
					state?: Record<string, unknown>;
					metadata?: {
						acp?: {
							kind?: string;
							title?: string;
							rawInput?: Record<string, unknown>;
							content?: unknown;
							rawOutput?: unknown;
							locations?: unknown;
						};
					};
			  }
			| undefined;
		expect(tool, JSON.stringify(messages.data, null, 2)).toMatchObject({
			tool: "write",
			state: {
				status: "completed",
				input: {
					filePath: TOOL_PROBE_PATH,
					content: "GIGACODE_TOOL_EXECUTION_OK",
				},
				output: "Wrote file successfully.",
				metadata: {
					diagnostics: {},
					filepath: TOOL_PROBE_PATH,
					exists: false,
					truncated: false,
				},
			},
			metadata: {
				acp: {
					kind: "edit",
					rawInput: {
						file_path: TOOL_PROBE_PATH,
						content: "GIGACODE_TOOL_EXECUTION_OK",
					},
				},
			},
		});
		expect(tool?.metadata?.acp?.title).toBeTruthy();
		expect(
			tool?.metadata?.acp?.content !== undefined ||
				tool?.metadata?.acp?.rawOutput !== undefined,
		).toBe(true);
		expect(JSON.stringify(assistant)).toContain("GIGACODE_TOOL_EXECUTION_OK");
		expect(JSON.stringify(assistant)).toContain(TOOL_FINAL_RESPONSE);
		const pending = await fetch(`${daemon.apiUrl}/permission`).then(
			(response) => response.json() as Promise<Array<{ sessionID: string }>>,
		);
		expect(pending.some((item) => item.sessionID === sessionId)).toBe(false);
	}, 180_000);

	test("normalizes ACP edits like native OpenCode edit tools", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const created = await within(
			client.session.create({ body: { title: "Native edit tool shape" } }),
			"edit tool session.create",
			daemon,
			60_000,
		);
		const sessionId = created.data?.id as string;
		const setup = await within(
			client.session.shell({
				path: { id: sessionId },
				body: {
					agent: "build",
					model: { providerID: "claude", modelID: "default" },
					command: `printf GIGACODE_EDIT_BEFORE > ${EDIT_TOOL_PROBE_PATH}`,
				},
			}),
			"edit tool probe setup",
			daemon,
			30_000,
		);
		expect(setup.error, daemon.logs()).toBeUndefined();
		expect(
			await readFile(resolve(ROOT, EDIT_TOOL_PROBE_FILENAME), "utf8"),
		).toBe("GIGACODE_EDIT_BEFORE");
		const prompt = client.session.prompt({
			path: { id: sessionId },
			body: {
				model: { providerID: "claude", modelID: "default" },
				parts: [{ type: "text", text: "GIGACODE_EDIT_TOOL_STREAM" }],
			},
		});
		await replyToSessionPermissionIfRequested(daemon, sessionId, prompt);
		const result = await within(
			prompt,
			"edit tool session.prompt",
			daemon,
			90_000,
		);
		expect(result.error, daemon.logs()).toBeUndefined();
		expect(JSON.stringify(result.data)).toContain(EDIT_TOOL_FINAL_RESPONSE);
		expect(
			await readFile(resolve(ROOT, EDIT_TOOL_PROBE_FILENAME), "utf8"),
		).toBe("GIGACODE_EDIT_AFTER");
		const messages = await within(
			client.session.messages({ path: { id: sessionId } }),
			"edit tool session.messages",
			daemon,
		);
		const tool = messages.data
			?.flatMap((message) => message.parts)
			.find((part) => part.type === "tool" && part.tool === "edit");
		expect(tool, JSON.stringify(messages.data)).toMatchObject({
			tool: "edit",
			state: {
				status: "completed",
				input: {
					filePath: EDIT_TOOL_PROBE_PATH,
					oldString: "GIGACODE_EDIT_BEFORE",
					newString: "GIGACODE_EDIT_AFTER",
				},
				output: "Edit applied successfully.",
				metadata: {
					diagnostics: {},
					filediff: {
						file: EDIT_TOOL_PROBE_PATH,
						additions: 1,
						deletions: 1,
					},
					truncated: false,
				},
			},
			metadata: {
				acp: {
					kind: "edit",
					rawInput: {
						file_path: EDIT_TOOL_PROBE_PATH,
						old_string: "GIGACODE_EDIT_BEFORE",
						new_string: "GIGACODE_EDIT_AFTER",
					},
				},
			},
		});
		expect(JSON.stringify(tool)).toContain("GIGACODE_EDIT_BEFORE");
		expect(JSON.stringify(tool)).toContain("GIGACODE_EDIT_AFTER");
	}, 180_000);

	test("normalizes ACP command execution like native OpenCode bash tools", async () => {
		for (let iteration = 0; iteration < BASH_TOOL_ITERATIONS; iteration += 1) {
			const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
			const created = await within(
				client.session.create({
					body: { title: `Native bash tool shape ${iteration + 1}` },
				}),
				"bash tool session.create",
				daemon,
				60_000,
			);
			const sessionId = created.data?.id as string;
			const modelID = harnessModelIDs.get("opencode");
			expect(modelID).toBeTruthy();
			const toolEvents = fetch(`${daemon.apiUrl}/event`).then((response) =>
				readSseUntil(
					response,
					(value) => {
						const event = value as {
							type?: string;
							properties?: {
								sessionID?: string;
								part?: { tool?: string; state?: { status?: string } };
							};
						};
						return (
							event.type === "message.part.updated" &&
							event.properties?.sessionID === sessionId &&
							event.properties.part?.tool === "bash" &&
							event.properties.part.state?.status === "completed"
						);
					},
					90_000,
				),
			);
			const prompt = client.session.prompt({
				path: { id: sessionId },
				body: {
					model: { providerID: "opencode", modelID: modelID as string },
					parts: [{ type: "text", text: "GIGACODE_BASH_TOOL_STREAM" }],
				},
			});
			await replyToSessionPermissionIfRequested(daemon, sessionId, prompt);
			const result = await within(
				prompt,
				"bash tool session.prompt",
				daemon,
				90_000,
			);
			expect(result.error, daemon.logs()).toBeUndefined();
			expect(result.data).toBeDefined();
			const streamedToolStatuses = (await within(
				toolEvents,
				"bash tool event lifecycle",
				daemon,
				90_000,
			))
				.map((record) => record.value)
				.filter(
					(event: any) =>
						event.type === "message.part.updated" &&
						event.properties?.sessionID === sessionId &&
						event.properties?.part?.tool === "bash",
				)
				.map((event: any) => event.properties.part.state.status)
				.filter((status, index, statuses) => status !== statuses[index - 1]);
			expect(streamedToolStatuses).toEqual([
				"pending",
				"running",
				"completed",
			]);
			const messages = await within(
				client.session.messages({ path: { id: sessionId } }),
				"bash tool session.messages",
				daemon,
			);
			const tool = messages.data?.[1]?.parts.find(
				(part) => part.type === "tool",
			);
			expect(tool).toMatchObject({
				tool: "bash",
				state: {
					status: "completed",
					input: { command: BASH_TOOL_COMMAND },
					output: BASH_TOOL_OUTPUT,
					title: BASH_TOOL_COMMAND,
					metadata: {
						output: BASH_TOOL_OUTPUT,
						exit: 0,
						truncated: false,
					},
				},
				metadata: {
					acp: { kind: "execute" },
				},
			});
		}
	}, 180_000);

	test("marks an aborted ACP tool part as interrupted", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const created = await within(
			client.session.create({ body: { title: "Aborted tool lifecycle" } }),
			"aborted tool session.create",
			daemon,
			60_000,
		);
		const sessionId = created.data?.id as string;
		const modelID = harnessModelIDs.get("opencode");
		expect(modelID).toBeTruthy();
		const submitted = await postPromptAsync(
			daemon,
			sessionId,
			"GIGACODE_CANCEL_TOOL_STREAM",
			{ providerID: "opencode", modelID: modelID as string },
		);
		expect(submitted.status, await submitted.clone().text()).toBe(204);
		await waitForCondition(
			async () => {
				const messages = (await (
					await fetch(`${daemon.apiUrl}/session/${sessionId}/message`)
				).json()) as Array<{ parts?: Array<{ tool?: string; state?: { status?: string } }> }>;
				return messages.some((message) =>
					message.parts?.some(
						(part) =>
							part.tool === "bash" &&
							(part.state?.status === "pending" ||
								part.state?.status === "running"),
					),
				);
			},
			"pending ACP bash tool",
			daemon,
			90_000,
		);
		const aborted = await within(
			client.session.abort({ path: { id: sessionId } }),
			"aborted tool session.abort",
			daemon,
			60_000,
		);
		expect(aborted.error).toBeUndefined();
		expect(aborted.data).toBe(true);
		const messages = await within(
			client.session.messages({ path: { id: sessionId } }),
			"aborted tool session.messages",
			daemon,
		);
		const tool = messages.data?.[1]?.parts.find(
			(part) => part.type === "tool" && part.tool === "bash",
		);
		expect(tool).toMatchObject({
			state: {
				status: "completed",
				output: expect.stringContaining("User aborted the command"),
				metadata: { interrupted: true },
			},
		});
	}, 180_000);

	test("preserves native OpenCode write tool results", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const created = await within(
			client.session.create({ body: { title: "Native write shape" } }),
			"write session.create",
			daemon,
			60_000,
		);
		const sessionId = created.data?.id as string;
		const modelID = harnessModelIDs.get("opencode");
		expect(modelID).toBeTruthy();
		const prompt = client.session.prompt({
			path: { id: sessionId },
			body: {
				model: { providerID: "opencode", modelID: modelID as string },
				parts: [{ type: "text", text: "GIGACODE_OPENCODE_PATCH_TOOL" }],
			},
		});
		await replyToSessionPermissionIfRequested(daemon, sessionId, prompt);
		const result = await within(
			prompt,
			"write session.prompt",
			daemon,
			90_000,
		);
		expect(result.error, daemon.logs()).toBeUndefined();
		expect(result.data).toBeDefined();
		const messages = await within(
			client.session.messages({ path: { id: sessionId } }),
			"write session.messages",
			daemon,
		);
		expect(
			await readFile(resolve(ROOT, OPENCODE_PATCH_PROBE_FILENAME), "utf8").catch(
				(error) =>
					Promise.reject(
						new Error(
							`${String(error)}\nmessages=${JSON.stringify(messages.data)}`,
						),
					),
			),
		).toBe(OPENCODE_PATCH_CONTENT);
		const tool = messages.data
			?.flatMap((message) => message.parts)
			.find((part) => part.type === "tool");
		expect(tool, JSON.stringify(messages.data)).toMatchObject({
			tool: "write",
			state: {
				status: "completed",
				input: {
					filePath: OPENCODE_PATCH_PROBE_PATH,
					content: OPENCODE_PATCH_CONTENT,
				},
				output: "Wrote file successfully.",
				metadata: {
					diagnostics: {},
					filepath: OPENCODE_PATCH_PROBE_PATH,
					exists: false,
					truncated: false,
				},
			},
			metadata: {
				acp: {
					kind: "edit",
					rawInput: {
						filePath: OPENCODE_PATCH_PROBE_PATH,
						content: OPENCODE_PATCH_CONTENT,
					},
				},
			},
		});
	}, 180_000);

	test("assigns globally unique public IDs to repeated ACP permissions", async () => {
		const directories = [
			resolve(daemon.stateDir, "permission-a"),
			resolve(daemon.stateDir, "permission-b"),
		];
		await Promise.all(directories.map((directory) => mkdir(directory)));
		const sessions = await Promise.all(
			directories.map(async (directory, index) => {
				const response = await fetch(
					`${daemon.apiUrl}/session?directory=${encodeURIComponent(directory)}`,
					{
						method: "POST",
						headers: { "content-type": "application/json" },
						body: JSON.stringify({ title: `Permission collision ${index}` }),
					},
				);
				expect(response.ok, await response.clone().text()).toBe(true);
				return (await response.json()) as { id: string };
			}),
		);
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const runPermissionFlow = async (session: { id: string }) => {
			// Do not await the synchronous prompt endpoint before answering its
			// permission: the ACP turn cannot finish until this reply arrives.
			const prompt = client.session.prompt({
				path: { id: session.id },
				body: {
					model: { providerID: "claude", modelID: "default" },
					parts: [{ type: "text", text: "GIGACODE_TOOL_STREAM" }],
				},
			});
			let publicId = "";
			await waitForCondition(
				async () => {
					const permissions = (await fetch(`${daemon.apiUrl}/permission`).then(
						(response) => response.json(),
					)) as Array<{ id: string; sessionID: string }>;
					publicId =
						permissions.find(
							(permission) => permission.sessionID === session.id,
						)?.id ?? "";
					return Boolean(publicId);
				},
				`permission request for ${session.id}`,
				daemon,
				120_000,
			);
			const response = await fetch(
				`${daemon.apiUrl}/permission/${encodeURIComponent(publicId)}/reply`,
				{
					method: "POST",
					headers: { "content-type": "application/json" },
					body: JSON.stringify({ reply: "once" }),
				},
			);
			expect(response.ok, await response.clone().text()).toBe(true);
			const result = await within(
				prompt,
				`permission prompt for ${session.id}`,
				daemon,
				120_000,
			);
			expect(result.error, daemon.logs()).toBeUndefined();
			return publicId;
		};
		// Claude commonly reuses the private ACP ID "permission". Two real flows
		// must still receive different public IDs, even when actor transport cannot
		// reliably hold simultaneous cold sessions open.
		const publicIds = [];
		for (const session of sessions) {
			publicIds.push(await runPermissionFlow(session));
		}
		expect(new Set(publicIds).size).toBe(2);
		expect(
			publicIds.every((permissionId) =>
				/^per_[0-9a-f]{32}$/.test(permissionId),
			),
		).toBe(true);
		await Promise.all(
			sessions.map((session) =>
				fetch(`${daemon.apiUrl}/session/${session.id}`, { method: "DELETE" }),
			),
		);
	}, 300_000);

	test("cancels an active turn only after ACP confirms quiescence or containment", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const created = await within(
			client.session.create({ body: { title: "Cancellation E2E" } }),
			"cancel session.create",
			daemon,
			60_000,
		);
		const sessionId = created.data?.id as string;
		const requestCount = mock.getRequests().length;
		const submitted = await postPromptAsync(
			daemon,
			sessionId,
			"GIGACODE_CANCEL_SLOW",
			{ providerID: "pi", modelID: "default" },
		);
		expect(submitted.status, await submitted.clone().text()).toBe(204);
		await waitForCondition(
			() =>
				mock.getRequests().length > requestCount &&
				JSON.stringify(mock.getRequests().slice(requestCount)).includes(
					"GIGACODE_CANCEL_SLOW",
				),
			"slow LLMock request",
			daemon,
			90_000,
		);
		const aborted = await within(
			client.session.abort({ path: { id: sessionId } }),
			"session.abort",
			daemon,
		);
		expect(aborted.error).toBeUndefined();
		expect(aborted.data).toBe(true);
		await waitForCondition(
			async () => {
				const statuses = (await (
					await fetch(`${daemon.apiUrl}/session/status`)
				).json()) as Record<string, { type: string }>;
				return statuses[sessionId] === undefined;
			},
			"cancelled session idle",
			daemon,
			90_000,
		);
		let messages = (await (
			await fetch(`${daemon.apiUrl}/session/${sessionId}/message`)
		).json()) as Array<{ info: { role: string; error?: { name?: string } } }>;
		expect(messages[1]?.info.error?.name).toBe("MessageAbortedError");

		const resumed = await within(
			client.session.prompt({
				path: { id: sessionId },
				body: {
					model: { providerID: "pi", modelID: "default" },
					parts: [{ type: "text", text: "GIGACODE_CANCEL_FOLLOWUP" }],
				},
			}),
			"post-cancel session.prompt",
			daemon,
			90_000,
		);
		expect(resumed.error, daemon.logs()).toBeUndefined();
		const resumedJson = JSON.stringify(resumed.data);
		if (!resumedJson.includes(CANCEL_FOLLOWUP_RESPONSE)) {
			const recentMockMessages = mock
				.getRequests()
				.slice(-4)
				.map((request) => ({
					fixture: request.response.fixture?.match,
					messages: request.body?.messages?.slice(-3),
				}));
			throw new Error(
				`post-cancel response was ${resumedJson}\nLLMock messages: ${JSON.stringify(recentMockMessages)}\n${daemon.logs()}`,
			);
		}
		messages = (await (
			await fetch(`${daemon.apiUrl}/session/${sessionId}/message`)
		).json()) as Array<{ info: { role: string; error?: { name?: string } } }>;
		expect(messages.map((message) => message.info.role)).toEqual([
			"user",
			"assistant",
			"user",
			"assistant",
		]);
		const log = await readSessionLog(daemon, sessionId, (records) =>
			records.some((record) => record.event === "prompt.completed"),
		);
		const createdSessions = log.filter(
			(record) => record.event === "agentos.session.created",
		);
		// Cancellation unloads the live adapter because ACP acknowledgement does
		// not yet guarantee that every adapter has stopped producing tool calls.
		// The follow-up resumes the same durable AgentOS session and history.
		expect(createdSessions).toHaveLength(1);
		expect(log).toContainEqual(
			expect.objectContaining({
				event: "agentos.session.resumed",
			}),
		);
		expect(log).toContainEqual(
			expect.objectContaining({
				event: "agentos.turn.cancel_started",
			}),
		);
		const cancelled = log.find(
			(record) => record.event === "agentos.turn.cancelled",
		);
		expect(cancelled?.via).toBe("session-unloaded");
		expect(
			log.some(
				(record) => record.event === "agentos.session.unloaded_after_cancel",
			),
		).toBe(true);
	}, 180_000);

	test("resumes a logical session after a daemon restart", async () => {
		const beforeClient = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const persistedBefore = await within(
			beforeClient.session.messages({ path: { id: sdkSessionId } }),
			"pre-restart session.messages",
			daemon,
		);
		expect(persistedBefore.error).toBeUndefined();
		const before = await readSessionLog(daemon, sdkSessionId);
		const createdBefore = before.filter(
			(record) => record.event === "agentos.session.created",
		);
		expect(createdBefore.length).toBeGreaterThan(0);
		const discoveryMarker =
			"[gigacode] discovering models from AgentOS harnesses";
		const discoveryCountBefore =
			daemon.logs().split(discoveryMarker).length - 1;
		expect(discoveryCountBefore).toBe(
			process.env.GIGACODE_E2E_PRESEED_MODELS === "1" ? 0 : 1,
		);

		await restartDaemon(daemon);
		expect(daemon.logs().split(discoveryMarker).length - 1).toBe(
			discoveryCountBefore,
		);
		const restartedHealth = (await (
			await fetch(`${daemon.apiUrl}/global/health`)
		).json()) as { modelCatalogStage?: string };
		expect(restartedHealth.modelCatalogStage).toContain(
			"model catalog is ready from cache",
		);
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const providerStarted = performance.now();
		const warmProviders = await within(
			client.provider.list(),
			"warm provider.list",
			daemon,
			2_000,
		);
		expect(warmProviders.error).toBeUndefined();
		expect(performance.now() - providerStarted).toBeLessThan(1_000);
		const listed = await within(
			client.session.list(),
			"resumed session.list",
			daemon,
		);
		expect(listed.data?.map((session) => session.id)).toContain(sdkSessionId);
		const persistedAfter = await within(
			client.session.messages({ path: { id: sdkSessionId } }),
			"post-restart session.messages",
			daemon,
		);
		expect(persistedAfter.data).toEqual(persistedBefore.data);

		const prompted = await within(
			client.session.prompt({
				path: { id: sdkSessionId },
				body: {
					model: { providerID: "claude", modelID: "default" },
					parts: [
						{
							type: "text",
							text: "Return the deterministic response after resuming.",
						},
					],
				},
			}),
			"resumed session.prompt",
			daemon,
			90_000,
		);
		expect(prompted.error, daemon.logs()).toBeUndefined();
		expect(JSON.stringify(prompted.data)).toContain(RESPONSE_TEXT);

		const after = await readSessionLog(
			daemon,
			sdkSessionId,
			(items) =>
				items.filter((record) => record.event === "prompt.completed").length ===
				3,
		);
		const promptStarts = after.filter(
			(record) => record.event === "agentos.prompt.started",
		);
		const createdAfter = after.filter(
			(record) => record.event === "agentos.session.created",
		);
		expect(createdAfter).toHaveLength(createdBefore.length);
		expect(promptStarts.at(-1)?.actorSessionId).toBe(
			createdBefore.at(-1)?.actorSessionId,
		);
		expect(
			after.filter((record) => record.event === "prompt.completed"),
		).toHaveLength(3);
	}, 180_000);

	test("keeps a workspace actor wakeable across consecutive daemon restarts", async () => {
		let client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const created = await within(
			client.session.create({ body: { title: "Repeated restart probe" } }),
			"repeated restart session.create",
			daemon,
		);
		expect(created.error, daemon.logs()).toBeUndefined();
		const sessionId = created.data?.id as string;

		for (const cycle of [1, 2, 3]) {
			await restartDaemon(daemon);
			client = createOpencodeClient({ baseUrl: daemon.apiUrl });
			const shell = await within(
				client.session.shell({
					path: { id: sessionId },
					body: {
						agent: "build",
						model: { providerID: "claude", modelID: "default" },
						command: `printf 'restart-${cycle}\\n'`,
					},
				}),
				`repeated restart shell ${cycle}`,
				daemon,
			);
			expect(shell.error, daemon.logs()).toBeUndefined();
			const messages = await within(
				client.session.messages({ path: { id: sessionId } }),
				`repeated restart messages ${cycle}`,
				daemon,
			);
			expect(JSON.stringify(messages.data)).toContain(`restart-${cycle}`);
		}
	}, 300_000);

	test("refreshes models only when the manual CLI command is executed", async () => {
		const discoveryMarker =
			"[gigacode] discovering models from AgentOS harnesses";
		const before = daemon.logs().split(discoveryMarker).length - 1;
		let output = "";
		const child = spawn(
			process.execPath,
			["--import", TSX_IMPORT, ENTRYPOINT, "models", "refresh"],
			{
				cwd: ROOT,
				stdio: ["ignore", "pipe", "pipe"],
				env: daemon.env,
			},
		);
		child.stdout?.on("data", (chunk: Buffer) => {
			output = appendLog(output, chunk);
		});
		child.stderr?.on("data", (chunk: Buffer) => {
			output = appendLog(output, chunk);
		});
		const status = await within(
			new Promise<number | null>((resolveExit, reject) => {
				child.once("error", reject);
				child.once("exit", resolveExit);
			}),
			"gigacode models refresh",
			daemon,
			180_000,
		);
		expect(status, output).toBe(0);
		expect(output).toContain("Gigacode model catalog refreshed");
		expect(daemon.logs().split(discoveryMarker).length - 1).toBe(before + 1);
	}, 180_000);

	test("runs and resumes all supported native ACP harnesses through the OpenCode SDK", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const harnessSessions = new Map<
			"claude" | "codex" | "opencode" | "pi",
			{ sessionId: string; actorSessionId: string; messages: unknown }
		>();
		const requestedHarness = process.env.GIGACODE_E2E_HARNESS;
		const liveHarnesses = (["claude", "codex", "opencode", "pi"] as const).filter(
			(providerID) => !requestedHarness || providerID === requestedHarness,
		);
		expect(liveHarnesses, `unknown GIGACODE_E2E_HARNESS=${requestedHarness}`).not
			.toHaveLength(0);
		for (const providerID of liveHarnesses) {
			const modelID = harnessModelIDs.get(providerID);
			expect(modelID, `${providerID} selected model`).toBeTruthy();
			const created = await within(
				client.session.create({
					body: { title: `${providerID} SDK E2E` },
				}),
				`${providerID} session.create`,
				daemon,
				60_000,
			);
			expect(created.error, daemon.logs()).toBeUndefined();
			const sessionId = created.data?.id as string;
			for (const turn of [1, 2]) {
				const requestsBefore = mock.getRequests().length;
				const prompted = await within(
					client.session.prompt({
						path: { id: sessionId },
						body: {
							model: { providerID, modelID: modelID as string },
							parts: [
								{
									type: "text",
									text: `Return the deterministic test response from ${providerID}, turn ${turn}.`,
								},
							],
						},
					}),
					`${providerID} turn ${turn}`,
					daemon,
					120_000,
				);
				expect(prompted.error, daemon.logs()).toBeUndefined();
				expect(
					(prompted.data as { info?: { error?: unknown } } | undefined)?.info
						?.error,
					daemon.logs(),
				).toBeUndefined();
				const providerRequests = mock.getRequests().slice(requestsBefore);
				expect(
					providerRequests.length,
					`${providerID} turn ${turn} did not reach LLMock`,
				).toBeGreaterThan(0);
				expect(
					JSON.stringify(prompted.data),
					`${providerID} turn ${turn} requests: ${JSON.stringify(providerRequests)}`,
				).toContain(RESPONSE_TEXT);
			}
			const messages = await within(
				client.session.messages({ path: { id: sessionId } }),
				`${providerID} messages before restart`,
				daemon,
			);
			expect(messages.error).toBeUndefined();
			expect(messages.data?.map((message) => message.info.role)).toEqual([
				"user",
				"assistant",
				"user",
				"assistant",
			]);
			const records = await readSessionLog(daemon, sessionId, (items) =>
				items.some(
					(record) => record.event === "agentos.session.model.selected",
				),
			);
			const actorSessionId = records.find(
				(record) => record.event === "agentos.session.created",
			)?.actorSessionId;
			expect(actorSessionId, `${providerID} native ACP session ID`).toEqual(
				expect.any(String),
			);
			harnessSessions.set(providerID, {
				sessionId,
				actorSessionId: actorSessionId as string,
				messages: messages.data,
			});
			expect(records).toContainEqual(
				expect.objectContaining({
					event: "agentos.session.model.selected",
					model: modelID,
				}),
			);
		}
		const persistenceProbe = await within(
			client.session.create({ body: { title: "Actor persistence probe" } }),
			"persistence probe session.create",
			daemon,
		);
		const persistenceProbeId = persistenceProbe.data?.id as string;
		await restartDaemon(daemon);
		const resumedClient = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const requiresPersistedOpenCodeState = harnessSessions.has("opencode");
		const databaseAfterRestart = await within(
			resumedClient.session.shell({
				path: { id: persistenceProbeId },
				body: {
					agent: "build",
					model: { providerID: "claude", modelID: "default" },
					command: `node -e 'const required=${JSON.stringify(requiresPersistedOpenCodeState)};if(!required){console.log(JSON.stringify({required}));process.exit(0)}const {DatabaseSync}=require("node:sqlite");const db=new DatabaseSync("/home/agentos/.local/share/opencode/opencode.db",{readOnly:true});const sessions=db.prepare("select count(*) count from session").get().count;const legacy=db.prepare("select count(*) count from message").get().count;const durable=db.prepare("select count(*) count from session_message").get().count;console.log(JSON.stringify({required,sessions,legacy,durable}));if(sessions===0)process.exit(2);if(legacy===0&&durable===0)process.exit(3)'`,
				},
			}),
			"OpenCode database after restart",
			daemon,
		);
		expect(databaseAfterRestart.error, daemon.logs()).toBeUndefined();
		const databaseProbeMessages = await within(
			resumedClient.session.messages({ path: { id: persistenceProbeId } }),
			"OpenCode database probe messages after restart",
			daemon,
		);
		const databaseProbeTool = databaseProbeMessages.data?.[1]?.parts.find(
			(part) => part.type === "tool",
		);
		expect(databaseProbeTool).toMatchObject({
			type: "tool",
			tool: "bash",
			state: {
				status: "completed",
				metadata: { exit: 0, truncated: false },
			},
		});
		if (requiresPersistedOpenCodeState) {
			expect(
				(databaseProbeTool?.state as { output?: string } | undefined)?.output,
			).toContain('"required":true');
		}
		const listed = await within(
			resumedClient.session.list(),
			"all-harness post-restart session.list",
			daemon,
		);
		const listedIDs = listed.data?.map((session) => session.id) ?? [];
		for (const [providerID, prior] of harnessSessions) {
			expect(listedIDs).toContain(prior.sessionId);
			const persisted = await within(
				resumedClient.session.messages({ path: { id: prior.sessionId } }),
				`${providerID} messages after restart`,
				daemon,
			);
			expect(persisted.data).toEqual(prior.messages);
			const modelID = harnessModelIDs.get(providerID) as string;
			const prompted = await within(
				resumedClient.session.prompt({
					path: { id: prior.sessionId },
					body: {
						model: { providerID, modelID },
						parts: [
							{
								type: "text",
								text: `Return the deterministic test response from resumed ${providerID}, turn 3.`,
							},
						],
					},
				}),
				`${providerID} resumed turn 3`,
				daemon,
				120_000,
			);
			expect(prompted.error, daemon.logs()).toBeUndefined();
			expect(JSON.stringify(prompted.data)).toContain(RESPONSE_TEXT);
			const messages = await within(
				resumedClient.session.messages({ path: { id: prior.sessionId } }),
				`${providerID} messages after resumed prompt`,
				daemon,
			);
			expect(messages.data?.map((message) => message.info.role)).toEqual([
				"user",
				"assistant",
				"user",
				"assistant",
				"user",
				"assistant",
			]);
			const records = await readSessionLog(
				daemon,
				prior.sessionId,
				(items) =>
					items.filter((record) => record.event === "prompt.completed")
						.length >= 3,
			);
			expect(
				records.filter((record) => record.event === "prompt.completed"),
			).toHaveLength(3);
			expect(
				records.filter((record) => record.event === "agentos.session.created"),
			).toHaveLength(1);
		}
	}, 900_000);

	test("rejects switching ACP harnesses within one OpenCode session", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const created = await within(
			client.session.create({ body: { title: "Harness binding E2E" } }),
			"harness binding session.create",
			daemon,
			60_000,
		);
		const sessionId = created.data?.id as string;
		const claude = await within(
			client.session.prompt({
				path: { id: sessionId },
				body: {
					model: { providerID: "claude", modelID: "default" },
					parts: [{ type: "text", text: "GIGACODE_BIND_CLAUDE" }],
				},
			}),
			"initial Claude prompt",
			daemon,
			90_000,
		);
		expect(claude.error).toBeUndefined();
		const pi = await within(
			client.session.prompt({
				path: { id: sessionId },
				body: {
					model: { providerID: "pi", modelID: piModelID },
					parts: [{ type: "text", text: "GIGACODE_REJECT_PI" }],
				},
			}),
			"rejected Claude-to-Pi prompt",
			daemon,
			120_000,
		);
		expect(pi.error).toBeDefined();
		expect(JSON.stringify(pi.error)).toContain(
			"uses the claude ACP harness and cannot switch to pi",
		);
		const messages = await within(
			client.session.messages({ path: { id: sessionId } }),
			"messages after rejected harness switch",
			daemon,
		);
		expect(messages.data?.map((message) => message.info.role)).toEqual([
			"user",
			"assistant",
		]);
		const log = await readSessionLog(daemon, sessionId);
		expect(
			log.filter((record) => record.event === "agentos.session.created"),
		).toHaveLength(1);
	}, 180_000);

	test("runs shell mode commands through the OpenCode SDK", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const created = await within(
			client.session.create({ body: { title: "Shell SDK E2E" } }),
			"shell session.create",
			daemon,
			60_000,
		);
		const sessionId = created.data?.id as string;
		const command = "printf GIGACODE_SDK_SHELL_OK && pwd";
		const shell = await within(
			client.session.shell({
				path: { id: sessionId },
				body: {
					agent: "build",
					model: { providerID: "claude", modelID: "default" },
					command,
				},
			}),
			"session.shell",
			daemon,
			90_000,
		);
		expect(shell.error, daemon.logs()).toBeUndefined();
		expect(shell.data?.role).toBe("assistant");
		const messages = await within(
			client.session.messages({ path: { id: sessionId } }),
			"shell session.messages",
			daemon,
		);
		expect(messages.data?.map((message) => message.info.role)).toEqual([
			"user",
			"assistant",
		]);
		const tool = messages.data?.[1]?.parts[0];
		expect(JSON.stringify(tool)).toContain("GIGACODE_SDK_SHELL_OK");
		expect(JSON.stringify(tool)).toContain("/workspace");
		expect(tool).toMatchObject({
			type: "tool",
			tool: "bash",
			state: {
				status: "completed",
				input: { command },
				title: command,
				metadata: { exit: 0, truncated: false },
			},
		});

		const failed = await within(
			client.session.shell({
				path: { id: sessionId },
				body: {
					agent: "build",
					model: { providerID: "claude", modelID: "default" },
					command: "printf GIGACODE_EXPECTED_FAILURE >&2; exit 7",
				},
			}),
			"failed session.shell",
			daemon,
			30_000,
		);
		expect(failed.error).toBeUndefined();
		const afterFailure = await within(
			client.session.messages({ path: { id: sessionId } }),
			"failed shell session.messages",
			daemon,
		);
		expect(afterFailure.data?.[3]?.parts[0]).toMatchObject({
			type: "tool",
			tool: "bash",
			state: {
				status: "error",
				input: { command: "printf GIGACODE_EXPECTED_FAILURE >&2; exit 7" },
				metadata: { exit: 7, truncated: false },
			},
		});
		expect(JSON.stringify(afterFailure.data?.[3])).toContain(
			"GIGACODE_EXPECTED_FAILURE",
		);
	}, 120_000);

	test("cancels a long shell command and runs another command in the same session", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const created = await within(
			client.session.create({ body: { title: "Shell cancellation E2E" } }),
			"shell cancel session.create",
			daemon,
			60_000,
		);
		const sessionId = created.data?.id as string;
		const running = fetch(`${daemon.apiUrl}/session/${sessionId}/shell`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({
				agent: "build",
				model: { providerID: "claude", modelID: "default" },
				command: "exec sleep 30",
			}),
		});
		await waitForCondition(
			async () => {
				const statuses = (await (
					await fetch(`${daemon.apiUrl}/session/status`)
				).json()) as Record<string, { type: string }>;
				return statuses[sessionId]?.type === "busy";
			},
			"shell busy status",
			daemon,
			30_000,
		);
		const aborted = await within(
			client.session.abort({ path: { id: sessionId } }),
			"shell session.abort",
			daemon,
			30_000,
		);
		expect(aborted.data).toBe(true);
		const cancelledResponse = await within(
			running,
			"cancelled shell response",
			daemon,
			30_000,
		);
		expect(cancelledResponse.ok, await cancelledResponse.clone().text()).toBe(
			true,
		);
		const resumed = await within(
			client.session.shell({
				path: { id: sessionId },
				body: {
					agent: "build",
					model: { providerID: "claude", modelID: "default" },
					command: "printf GIGACODE_AFTER_SHELL_CANCEL",
				},
			}),
			"post-cancel session.shell",
			daemon,
			30_000,
		);
		expect(resumed.error).toBeUndefined();
		const messages = await within(
			client.session.messages({ path: { id: sessionId } }),
			"post-cancel shell messages",
			daemon,
		);
		expect(JSON.stringify(messages.data)).toContain(
			"GIGACODE_AFTER_SHELL_CANCEL",
		);
	}, 120_000);

	test("uses one daemon and one actor per cwd for multiple sessions", async () => {
		const pidBefore = Number(
			(await readFile(resolve(daemon.stateDir, "daemon.pid"), "utf8")).trim(),
		);
		const otherWorkspace = resolve(daemon.stateDir, "other-workspace");
		await mkdir(otherWorkspace);
		await writeFile(
			resolve(otherWorkspace, "workspace-marker.txt"),
			"WORKSPACE_TWO",
		);
		const response = await fetch(
			`${daemon.apiUrl}/session?directory=${encodeURIComponent(otherWorkspace)}`,
			{
				method: "POST",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({ title: "Second workspace" }),
			},
		);
		expect(response.ok, await response.clone().text()).toBe(true);
		const session = (await response.json()) as {
			id: string;
			directory: string;
		};
		expect(session.directory).toBe(otherWorkspace);
		const actorsAfterFirst = await activeActorIds(daemon);
		const secondResponse = await fetch(
			`${daemon.apiUrl}/session?directory=${encodeURIComponent(otherWorkspace)}`,
			{
				method: "POST",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({ title: "Second session, same workspace" }),
			},
		);
		expect(secondResponse.ok, await secondResponse.clone().text()).toBe(true);
		const secondSession = (await secondResponse.json()) as { id: string };
		const actorsAfterSecond = await activeActorIds(daemon);
		expect(actorsAfterSecond).toEqual(actorsAfterFirst);
		const actorId = await sessionActorId(daemon, session.id);
		expect(await sessionActorId(daemon, secondSession.id)).toBe(actorId);
		const health = (await (
			await fetch(`${daemon.apiUrl}/global/health`)
		).json()) as { workspaceRoot: string };
		expect(health.workspaceRoot).toBe("/workspace");
		const pidAfter = Number(
			(await readFile(resolve(daemon.stateDir, "daemon.pid"), "utf8")).trim(),
		);
		expect(pidAfter).toBe(pidBefore);
		const actor = rivetClient(
			`http://127.0.0.1:${daemon.env.GIGACODE_RIVET_PORT}`,
		).vm.getForId(actorId);
		expect(
			new TextDecoder().decode(
				await actor.readFile("/workspace/workspace-marker.txt"),
			),
		).toBe("WORKSPACE_TWO");
		await expect(
			actor.readFile("/workspace/experiments/gigacode/package.json"),
		).rejects.toThrow();
		for (const sessionId of [session.id, secondSession.id]) {
			const deleted = await fetch(`${daemon.apiUrl}/session/${sessionId}`, {
				method: "DELETE",
			});
			expect(deleted.ok).toBe(true);
		}
		expect(await activeActorIds(daemon)).toEqual(actorsAfterSecond);
	}, 120_000);

	test("keeps three Claude turns ordered in the real OpenCode TUI", async () => {
		const terminal = await TmuxTerminal.launch({
			command: [GLOBAL_GIGACODE_BIN],
			cwd: ROOT,
			env: daemon.env,
		});
		try {
			await terminal.waitForText(/Build · .*Claude Code/, {
				timeoutMs: 30_000,
			});
			const prompts = ["ORDER_USER_1", "ORDER_USER_2", "ORDER_USER_3"];
			for (const [index, prompt] of prompts.entries()) {
				await terminal.type(prompt);
				await terminal.press("Enter");
				await terminal.waitForTextOccurrences(RESPONSE_TEXT, index + 1, {
					timeoutMs: 120_000,
				});
				await terminal.waitForTextAbsent("esc interrupt", {
					timeoutMs: 30_000,
				});
				const snapshot = await terminal.snapshot(`claude-turn-${index + 1}`);
				console.log(
					`\n--- terminal snapshot: ${snapshot.label} ---\n${snapshot.text}\n`,
				);
			}
			const snapshot = await terminal.snapshot("claude-three-turn-order");
			const responseIndexes: number[] = [];
			let responseIndex = snapshot.text.indexOf(RESPONSE_TEXT);
			while (responseIndex !== -1) {
				responseIndexes.push(responseIndex);
				responseIndex = snapshot.text.indexOf(
					RESPONSE_TEXT,
					responseIndex + RESPONSE_TEXT.length,
				);
			}
			const promptIndexes = prompts.map((prompt) =>
				snapshot.text.indexOf(prompt),
			);
			expect(responseIndexes).toHaveLength(3);
			expect(promptIndexes.every((index) => index >= 0)).toBe(true);
			expect([
				promptIndexes[0],
				responseIndexes[0],
				promptIndexes[1],
				responseIndexes[1],
				promptIndexes[2],
				responseIndexes[2],
			]).toEqual(
				[
					promptIndexes[0],
					responseIndexes[0],
					promptIndexes[1],
					responseIndexes[1],
					promptIndexes[2],
					responseIndexes[2],
				].toSorted((left, right) => left - right),
			);
			expect(snapshot.text).not.toContain("esc interrupt");

			await terminal.type("/gigacode-debugger");
			await terminal.press("Enter");
			// The first Enter accepts the custom command from autocomplete; the
			// second submits it, matching OpenCode 1.17.18's real TUI behavior.
			await terminal.press("Enter");
			await terminal.waitForText("Rivet inspector", { timeoutMs: 30_000 });
			const debuggerSnapshot = await terminal.snapshot("debugger-command");
			console.log(
				`\n--- terminal snapshot: ${debuggerSnapshot.label} ---\n${debuggerSnapshot.text}\n`,
			);
			expect(debuggerSnapshot.text).toContain("Rivet inspector");

			await terminal.type("/diff");
			await terminal.press("Enter");
			await new Promise((resolve) => setTimeout(resolve, 500));
			const diffSnapshot = await terminal.snapshot("empty-diff");
			console.log(
				`\n--- terminal snapshot: ${diffSnapshot.label} ---\n${diffSnapshot.text}\n`,
			);
			expect(diffSnapshot.text).not.toContain("panic:");
		} finally {
			await terminal.close();
		}
	}, 180_000);

	test("cancels and queues turns through the real OpenCode TUI", async () => {
		const terminal = await TmuxTerminal.launch({
			command: [GLOBAL_GIGACODE_BIN],
			cwd: ROOT,
			env: daemon.env,
		});
		try {
			await terminal.waitForText(/Build · .*Claude Code/, {
				timeoutMs: 30_000,
			});
			await terminal.type("GIGACODE_TUI_CANCEL");
			await terminal.press("Enter");
			await terminal.waitForText("esc interrupt", { timeoutMs: 90_000 });
			await terminal.press("Escape");
			await terminal.waitForText("esc again to interrupt", {
				timeoutMs: 30_000,
			});
			await terminal.press("Escape");
			await terminal.waitForTextAbsent("esc again to interrupt", {
				timeoutMs: 30_000,
				stableMs: 500,
			});
			const cancelled = await terminal.snapshot("tui-turn-cancelled");
			console.log(
				`\n--- terminal snapshot: ${cancelled.label} ---\n${cancelled.text}\n`,
			);
			expect(cancelled.text).not.toContain(
				"THIS_TUI_RESPONSE_MUST_BE_CANCELLED",
			);

			await terminal.type("GIGACODE_CANCEL_FOLLOWUP");
			await terminal.press("Enter");
			await terminal.waitForText(CANCEL_FOLLOWUP_RESPONSE, {
				timeoutMs: 120_000,
			});
			await terminal.waitForTextAbsent("esc interrupt", { timeoutMs: 30_000 });

			await terminal.type("GIGACODE_TUI_QUEUE_FIRST");
			await terminal.press("Enter");
			await terminal.waitForText("esc interrupt", { timeoutMs: 30_000 });
			await terminal.type("GIGACODE_TUI_QUEUE_SECOND");
			await terminal.press("Enter");
			await terminal.waitForText("GIGACODE_TUI_QUEUE_SECOND", {
				timeoutMs: 30_000,
			});
			const queuedMidFlight = await terminal.snapshot(
				"tui-turn-queued-mid-flight",
			);
			expect(queuedMidFlight.text).toContain("GIGACODE_TUI_QUEUE_FIRST");
			expect(queuedMidFlight.text).toContain("GIGACODE_TUI_QUEUE_SECOND");
			expect(queuedMidFlight.text).not.toContain(TUI_QUEUE_FIRST_RESPONSE);
			expect(queuedMidFlight.text).not.toContain(TUI_QUEUE_SECOND_RESPONSE);
			await terminal.waitForText(TUI_QUEUE_FIRST_RESPONSE, {
				timeoutMs: 120_000,
			});
			await terminal.waitForText(TUI_QUEUE_SECOND_RESPONSE, {
				timeoutMs: 120_000,
			});
			await terminal.waitForTextAbsent("esc interrupt", { timeoutMs: 30_000 });
			const queued = await terminal.snapshot("tui-turns-queued");
			console.log(
				`\n--- terminal snapshot: ${queued.label} ---\n${queued.text}\n`,
			);
			const order = [
				queued.text.indexOf("GIGACODE_TUI_QUEUE_FIRST"),
				queued.text.indexOf(TUI_QUEUE_FIRST_RESPONSE),
				queued.text.indexOf("GIGACODE_TUI_QUEUE_SECOND"),
				queued.text.indexOf(TUI_QUEUE_SECOND_RESPONSE),
			];
			expect(order.every((index) => index >= 0)).toBe(true);
			expect(order).toEqual([...order].sort((left, right) => left - right));
			expect(queued.text).not.toContain("THIS_TUI_RESPONSE_MUST_BE_CANCELLED");
		} finally {
			await terminal.close();
		}
	}, 300_000);

	test("drives the real OpenCode TUI, selects Pi, and receives a response", async () => {
		const terminal = await TmuxTerminal.launch({
			command: [GLOBAL_GIGACODE_BIN],
			cwd: ROOT,
			env: daemon.env,
		});
		const showSnapshot = async (label: string) => {
			const snapshot = await terminal.snapshot(label);
			console.log(`\n--- terminal snapshot: ${label} ---\n${snapshot.text}\n`);
			return snapshot.text;
		};
		try {
			await terminal.waitForText("Ask anything", { timeoutMs: 30_000 });
			expect(await showSnapshot("01-ready")).toContain("Claude Code");

			await terminal.press("Ctrl-X");
			await terminal.type("m");
			await terminal.waitForViewportText("Select model");
			await showSnapshot("02-model-selector");

			// Catalog ordering changes whenever another harness adds or refreshes a
			// model. Search by the rendered provider-qualified label so this test
			// selects Pi itself instead of whichever model happens to occupy a fixed
			// arrow-key offset in Recent.
			await terminal.type(`${piModelName} Pi`);
			// The unfiltered Recent list can already contain this exact label. Wait
			// for the selector to consume the query (and replace its placeholder)
			// before treating the matching row as the active search result.
			await terminal.waitForViewportTextAbsent("Search");
			const selectedPi = await terminal.waitForViewportText(
				`${piModelName} Pi`,
			);
			console.log(`\n--- terminal snapshot: 03-pi-highlighted ---\n${selectedPi}\n`);
			expect(selectedPi).toContain(`${piModelName} Pi`);
			// OpenCode and Pi expose the same model name in this fixture. The
			// filtered OpenCode result sorts first, followed by the Pi result.
			await terminal.press("Down");
			await terminal.press("Enter");
			const selection = await terminal.waitForViewportText(
				/Select variant|Build · .* Pi/,
			);
			if (selection.includes("Select variant")) {
				await terminal.press("Enter");
			}
			await terminal.waitForViewportText(/Build · .* Pi/);
			expect(await showSnapshot("04-pi-selected")).toMatch(/Build · .* Pi/);

			const prompt = "Return the deterministic test response.";
			await terminal.type(prompt);
			await terminal.waitForText(prompt);
			expect(await showSnapshot("05-prompt-entered")).toContain(prompt);
			await terminal.press("Enter");
			await terminal.waitForText(RESPONSE_TEXT, { timeoutMs: 120_000 });
			await terminal.waitForTextAbsent("esc interrupt", { timeoutMs: 30_000 });
			expect(await showSnapshot("06-pi-response")).toContain(RESPONSE_TEXT);
		} finally {
			await terminal.close();
		}
	}, 180_000);

	test("runs ! shell mode in the real OpenCode TUI", async () => {
		const terminal = await TmuxTerminal.launch({
			command: [GLOBAL_GIGACODE_BIN],
			cwd: ROOT,
			env: daemon.env,
		});
		try {
			await terminal.waitForText(/Build · .*Claude Code/, {
				timeoutMs: 30_000,
			});
			await terminal.type("!printenv GIGACODE_TUI_SHELL_VALUE");
			await terminal.press("Enter");
			await terminal.waitForText("GIGACODE_TUI_SHELL_OK", {
				timeoutMs: 90_000,
			});
			const snapshot = await terminal.snapshot("shell-mode-complete");
			console.log(
				`\n--- terminal snapshot: ${snapshot.label} ---\n${snapshot.text}\n`,
			);
			expect(snapshot.text).toContain("GIGACODE_TUI_SHELL_OK");
		} finally {
			await terminal.close();
		}
	}, 120_000);

	test("deletes SDK and CLI sessions from the coordinator", async () => {
		const client = createOpencodeClient({ baseUrl: daemon.apiUrl });
		const listed = await within(
			client.session.list(),
			"cleanup session.list",
			daemon,
		);
		expect(listed.data?.map((session) => session.id)).toContain(sdkSessionId);
		expect(listed.data?.map((session) => session.id)).toContain(cliSessionId);
		for (const session of listed.data ?? []) {
			const records = await readSessionLog(daemon, session.id);
			expect(records.every((record) => record.sessionId === session.id)).toBe(
				true,
			);
			const events = records.map((record) => record.event);
			expect(
				records.some(
					(record) =>
						record.event === "rivet.actor.resolved" &&
						typeof record.durationMs === "number",
				),
			).toBe(true);
			if (
				events.includes("prompt.completed") ||
				events.includes("agentos.shell.completed")
			) {
				expect(
					records.some(
						(record) =>
							(record.event === "agentos.prompt.completed" ||
								record.event === "agentos.shell.completed") &&
							typeof record.durationMs === "number",
					),
				).toBe(true);
			}
			const removed = await within(
				client.session.delete({ path: { id: session.id } }),
				`session.delete(${session.id})`,
				daemon,
			);
			expect(removed.data).toBe(true);
		}
		const final = await within(
			client.session.list(),
			"final session.list",
			daemon,
		);
		expect(final.data).toEqual([]);
		const health = (await (
			await fetch(`${daemon.apiUrl}/global/health`)
		).json()) as { rivetEndpoint: string };
		const coordinatorActors = (await (
			await fetch(
				`${health.rivetEndpoint}/actors?namespace=default&name=coordinator`,
			)
		).json()) as {
			actors?: Array<{ actor_id: string; destroy_ts?: number | null }>;
		};
		const coordinatorId = coordinatorActors.actors?.find(
			(actor) => !actor.destroy_ts,
		)?.actor_id as string;
		const coordinatorRows = await rivetClient(health.rivetEndpoint)
			.coordinator.getForId(coordinatorId)
			.listSessions();
		expect(coordinatorRows).toEqual([]);
		expect((await activeActorIds(daemon)).length).toBeGreaterThan(0);
	}, 60_000);
});
