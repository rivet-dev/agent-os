/**
 * Cross-agent GigaCode ACP startup benchmark.
 *
 * Runs Claude Code, Pi, and OpenCode through GigaCode's OpenCode HTTP
 * compatibility API against the same LLMock response used by
 * `agent-session.bench.ts`.
 *
 * Compare this benchmark with the raw AgentOS ACP numbers in:
 *   scripts/benchmarks/results/agent-session.json
 *
 * The directly comparable boundaries are:
 *   sessionCreate  ACP process spawn + initialize + session/new completion
 *   firstPrompt    completed ACP prompt against the local LLMock server
 *
 * GigaCode also reports logicalSessionCreate, modelSelect, messageRoundTrip,
 * and readyThroughFirstPrompt. Those are GigaCode HTTP/Rivet costs and should
 * not be folded into the raw ACP sessionCreate number. The raw benchmark makes
 * a fresh VM and filesystem probe per sample; this benchmark deliberately
 * reuses the daemon's warm canonical workspace actor.
 *
 * Usage:
 *   pnpm exec tsx scripts/benchmarks/gigacode-agent-session.bench.ts
 *   pnpm exec tsx scripts/benchmarks/gigacode-agent-session.bench.ts --agents=claude,pi
 *   pnpm exec tsx scripts/benchmarks/gigacode-agent-session.bench.ts --agents=claude --workspace=$PWD
 *   pnpm exec tsx scripts/benchmarks/gigacode-agent-session.bench.ts --discover-models
 */

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import os, { tmpdir } from "node:os";
import { resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { LLMock } from "@copilotkit/llmock";

type AgentName = "claude" | "pi" | "codex" | "opencode";
type Stage = "logicalSessionCreate" | "firstMessage" | "sessionLog";

type Timings = {
	logicalSessionCreate: number;
	sessionCreate: number;
	modelSelect?: number;
	firstPrompt: number;
	messageRoundTrip: number;
	readyThroughFirstPrompt: number;
};

type Sample = {
	timings: Timings;
	modelID: string;
	providerRequests: number;
	responseText: string;
};

type Failure = {
	attempt: number;
	warmup: boolean;
	stage: Stage;
	error: string;
};

type MetricStats = {
	mean: number;
	p50: number;
	p95: number;
	min: number;
	max: number;
};

type RunningGigacode = {
	apiUrl: string;
	child: ChildProcess;
	stateDir: string;
	daemonReadyMs: number;
	modelCatalogReadyMs: number;
};

type Provider = {
	id: string;
	models?: Record<string, { id?: string; name?: string }>;
};

const ROOT = resolve(import.meta.dirname, "../..");
const ENTRYPOINT = resolve(ROOT, "experiments/gigacode/gigacode.ts");
const TSX_IMPORT = createRequire(
	resolve(ROOT, "experiments/gigacode/package.json"),
).resolve("tsx");
const ALL_AGENTS: AgentName[] = ["claude", "pi", "codex", "opencode"];
const DEFAULT_AGENTS: AgentName[] = ALL_AGENTS;
// Keep these identical to agent-session.bench.ts for a meaningful comparison.
const RESPONSE_TEXT = "AGENTOS_SESSION_BENCH_OK";
const PROMPT = `Reply with exactly: ${RESPONSE_TEXT}`;

const args = process.argv.slice(2);
const valueArg = (name: string, fallback: string) =>
	args.find((value) => value.startsWith(`--${name}=`))?.split("=")[1] ?? fallback;
const iterations = Number.parseInt(valueArg("iterations", "3"), 10);
const warmup = Number.parseInt(valueArg("warmup", "1"), 10);
const promptTimeoutMs = Number.parseInt(valueArg("prompt-timeout-ms", "60000"), 10);
const agents = valueArg("agents", DEFAULT_AGENTS.join(",")).split(",") as AgentName[];
const requestedWorkspace = valueArg("workspace", "");
const discoverModels = args.includes("--discover-models");

for (const agent of agents) {
	if (!ALL_AGENTS.includes(agent)) {
		throw new Error(`Unknown agent ${agent}; expected ${ALL_AGENTS.join(",")}`);
	}
}
if (!Number.isInteger(iterations) || iterations < 1) {
	throw new Error("--iterations must be a positive integer");
}
if (!Number.isInteger(warmup) || warmup < 0) {
	throw new Error("--warmup must be a non-negative integer");
}
if (!Number.isInteger(promptTimeoutMs) || promptTimeoutMs < 1) {
	throw new Error("--prompt-timeout-ms must be a positive integer");
}

function round(value: number): number {
	return Math.round(value * 100) / 100;
}

function stats(values: number[]): MetricStats {
	const sorted = [...values].sort((a, b) => a - b);
	const percentile = (value: number) =>
		sorted[Math.max(0, Math.ceil((value / 100) * sorted.length) - 1)];
	return {
		mean: round(values.reduce((sum, value) => sum + value, 0) / values.length),
		p50: round(percentile(50)),
		p95: round(percentile(95)),
		min: round(sorted[0]),
		max: round(sorted.at(-1) ?? sorted[0]),
	};
}

function summarize(samples: Sample[]) {
	const metrics = [
		"logicalSessionCreate",
		"sessionCreate",
		"modelSelect",
		"firstPrompt",
		"messageRoundTrip",
		"readyThroughFirstPrompt",
	] as const;
	return Object.fromEntries(
		metrics.flatMap((metric) => {
			const values = samples.flatMap((sample) => {
				const value = sample.timings[metric];
				return typeof value === "number" ? [value] : [];
			});
			return values.length > 0 ? [[metric, stats(values)]] : [];
		}),
	);
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs: number): Promise<T> {
	let timer: NodeJS.Timeout | undefined;
	try {
		return await Promise.race([
			promise,
			new Promise<T>((_, reject) => {
				timer = setTimeout(
					() => reject(new Error(`${label} timed out after ${timeoutMs}ms`)),
					timeoutMs,
				);
			}),
		]);
	} finally {
		if (timer) clearTimeout(timer);
	}
}

async function freePort(): Promise<number> {
	const server = createServer();
	await new Promise<void>((resolveReady, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", resolveReady);
	});
	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("could not reserve benchmark port");
	}
	await new Promise<void>((resolveClosed, reject) =>
		server.close((error) => (error ? reject(error) : resolveClosed())),
	);
	return address.port;
}

async function waitFor(
	condition: () => Promise<boolean>,
	label: string,
	timeoutMs = 120_000,
	child?: ChildProcess,
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		if (await condition()) return;
		if (child?.exitCode !== null || child.signalCode !== null) {
			throw new Error(
				`${label} failed because GigaCode exited (code=${child.exitCode ?? "none"}, signal=${child.signalCode ?? "none"})`,
			);
		}
		await new Promise((resolveWait) => setTimeout(resolveWait, 100));
	}
	throw new Error(`${label} timed out after ${timeoutMs}ms`);
}

async function seedModelCache(stateDir: string): Promise<void> {
	const anthropicModel = {
		id: "anthropic/claude-sonnet-4-20250514",
		name: "Claude Sonnet 4 LLMock",
	};
	await writeFile(
		resolve(stateDir, "models.json"),
		`${JSON.stringify({
			version: 2,
			updatedAt: Date.now(),
			providers: {
				claude: {
					defaultModel: "default",
					models: [{ id: "default", name: "Claude Code" }],
				},
				codex: {
					defaultModel: "default",
					models: [{ id: "default", name: "Codex" }],
				},
				pi: {
					defaultModel: anthropicModel.id,
					models: [anthropicModel],
				},
				opencode: {
					defaultModel: anthropicModel.id,
					models: [anthropicModel],
				},
			},
		}, null, 2)}\n`,
		{ mode: 0o600 },
	);
}

async function configurePiHome(path: string, mockUrl: string): Promise<void> {
	await mkdir(resolve(path, "agent"), { recursive: true });
	await writeFile(
		resolve(path, "agent/models.json"),
		`${JSON.stringify({
			providers: {
				anthropic: { baseUrl: mockUrl, apiKey: "agentos-benchmark-key" },
			},
		})}\n`,
	);
}

async function startGigacode(mockUrl: string): Promise<RunningGigacode> {
	const apiPort = await freePort();
	const rivetPort = await freePort();
	const stateDir = await mkdtemp(resolve(tmpdir(), "gigacode-agent-session-bench-"));
	const workspaceDir = requestedWorkspace
		? resolve(requestedWorkspace)
		: resolve(stateDir, "workspace");
	if (requestedWorkspace) {
		if (!(await stat(workspaceDir).catch(() => undefined))?.isDirectory()) {
			throw new Error(`--workspace must name an existing directory: ${workspaceDir}`);
		}
	} else {
		await mkdir(workspaceDir, { recursive: true });
	}
	const piHome = resolve(stateDir, "pi");
	const claudeHome = resolve(stateDir, "claude");
	const codexHome = resolve(stateDir, "codex");
	const opencodeConfig = resolve(stateDir, "opencode-config");
	const opencodeData = resolve(stateDir, "opencode-data");
	await Promise.all(
		[piHome, claudeHome, codexHome, opencodeConfig, opencodeData].map((path) =>
			mkdir(path, { recursive: true }),
		),
	);
	if (!discoverModels) await seedModelCache(stateDir);
	await configurePiHome(piHome, mockUrl);
	await writeFile(
		resolve(opencodeConfig, "opencode.json"),
		`${JSON.stringify({
			autoupdate: false,
			share: "disabled",
			snapshot: false,
			model: "anthropic/claude-sonnet-4-20250514",
			provider: {
				anthropic: {
					options: { baseURL: `${mockUrl}/v1` },
					models: {
						"claude-sonnet-4-20250514": {
							name: "Claude Sonnet 4 LLMock",
						},
					},
				},
			},
		})}\n`,
	);
	await writeFile(
		resolve(opencodeData, "auth.json"),
		'{"anthropic":{"type":"api","key":"agentos-benchmark-key"}}\n',
	);

	const sidecar =
		process.env.AGENTOS_SIDECAR_BIN ?? resolve(ROOT, "target/release/agentos-sidecar");
	if (!existsSync(sidecar)) {
		throw new Error(`missing release benchmark artifact: ${sidecar}`);
	}

	const started = performance.now();
	const child = spawn(process.execPath, ["--import", TSX_IMPORT, ENTRYPOINT, "daemon"], {
		cwd: ROOT,
		stdio: ["ignore", "pipe", "pipe"],
		env: {
			...process.env,
			AGENTOS_SIDECAR_BIN: sidecar,
			GIGACODE_PORT: String(apiPort),
			GIGACODE_RIVET_PORT: String(rivetPort),
			GIGACODE_STATE_DIR: stateDir,
			GIGACODE_WORKSPACE: workspaceDir,
			GIGACODE_LOOPBACK_EXEMPT_PORTS: String(new URL(mockUrl).port),
			GIGACODE_NETWORK_PERMISSION: "allow",
			GIGACODE_CLAUDE_CONFIG_DIR: claudeHome,
			GIGACODE_CODEX_HOME: codexHome,
			GIGACODE_PI_HOME: piHome,
			GIGACODE_OPENCODE_CONFIG_DIR: opencodeConfig,
			GIGACODE_OPENCODE_DATA_DIR: opencodeData,
			GIGACODE_DISABLE_OPEN_URL: "1",
			GIGACODE_SESSION_ENV_JSON: JSON.stringify({
				ANTHROPIC_API_KEY: "agentos-benchmark-key",
				ANTHROPIC_BASE_URL: mockUrl,
				OPENAI_API_KEY: "agentos-benchmark-key",
				OPENAI_BASE_URL: `${mockUrl}/v1`,
				PI_SKIP_VERSION_CHECK: "1",
			}),
		},
	});
	child.stdout?.pipe(process.stderr);
	child.stderr?.pipe(process.stderr);
	const apiUrl = `http://127.0.0.1:${apiPort}/opencode`;
	try {
		await waitFor(
			async () =>
				await fetch(`${apiUrl}/global/health`)
					.then((response) => response.ok)
					.catch(() => false),
			"GigaCode HTTP API",
			120_000,
			child,
		);
		const daemonReadyMs = performance.now() - started;
		await waitFor(
			async () => {
				const health = await fetch(`${apiUrl}/global/health`)
					.then(
						(response) =>
							response.json() as Promise<{ modelCatalogStage?: string }>,
					)
					.catch(() => undefined);
				return (
					health?.modelCatalogStage?.startsWith("model catalog is ready") === true
				);
			},
			"GigaCode model catalog",
			120_000,
			child,
		);
		return {
			apiUrl,
			child,
			stateDir,
			daemonReadyMs: round(daemonReadyMs),
			modelCatalogReadyMs: round(performance.now() - started),
		};
	} catch (error) {
		child.kill("SIGTERM");
		await Promise.race([
			new Promise<void>((resolveExit) => child.once("exit", () => resolveExit())),
			new Promise<void>((resolveTimeout) => setTimeout(resolveTimeout, 10_000)),
		]);
		if (child.exitCode === null) child.kill("SIGKILL");
		await rm(stateDir, { recursive: true, force: true });
		throw error;
	}
}

async function stopGigacode(running: RunningGigacode): Promise<void> {
	running.child.kill("SIGTERM");
	await Promise.race([
		new Promise<void>((resolveExit) => running.child.once("exit", () => resolveExit())),
		new Promise<void>((resolveTimeout) => setTimeout(resolveTimeout, 10_000)),
	]);
	if (running.child.exitCode === null) running.child.kill("SIGKILL");
	await rm(running.stateDir, { recursive: true, force: true });
}

function selectModel(
	agent: AgentName,
	provider: Provider,
	defaultModel?: string,
): string {
	const models = Object.values(provider.models ?? {});
	const configuredDefault = models.find((model) => model.id === defaultModel);
	const preferred =
		configuredDefault ??
		(agent === "claude"
			? models.find((model) => model.id === "default")
			: agent === "pi"
				? models.find((model) => model.id?.startsWith("anthropic/"))
				: agent === "opencode"
					? models.find((model) => model.id?.startsWith("anthropic/"))
					: models.find((model) => model.id?.includes("codex")));
	const modelID = (preferred ?? models[0])?.id;
	if (!modelID) throw new Error(`${agent} exposed no model IDs`);
	return modelID;
}

async function sessionEvents(stateDir: string, sessionId: string) {
	const path = resolve(stateDir, "session-logs", `${sessionId}.jsonl`);
	let raw = "";
	await waitFor(async () => {
		raw = await readFile(path, "utf8").catch(() => "");
		return raw.includes('"event":"prompt.completed"');
	}, `${sessionId} completed session log`, 5_000);
	return raw
		.trim()
		.split("\n")
		.filter(Boolean)
		.map((line) => JSON.parse(line) as Record<string, unknown>);
}

async function runAttempt(
	running: RunningGigacode,
	agent: AgentName,
	modelID: string,
	mock: LLMock,
): Promise<Sample> {
	let stage: Stage = "logicalSessionCreate";
	let sessionId: string | undefined;
	const totalStarted = performance.now();
	try {
		const logicalStarted = performance.now();
		const createdResponse = await within(
			fetch(`${running.apiUrl}/session`, {
				method: "POST",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({ title: `${agent} GigaCode ACP benchmark` }),
			}),
			`${agent} logical session creation`,
			30_000,
		);
		const logicalSessionCreate = performance.now() - logicalStarted;
		if (!createdResponse.ok) {
			throw new Error(`session create failed: ${await createdResponse.text()}`);
		}
		const created = (await createdResponse.json()) as { id?: string };
		sessionId = created.id;
		if (!sessionId) throw new Error("session create returned no id");

		stage = "firstMessage";
		const requestsBefore = mock.getRequests().length;
		const messageStarted = performance.now();
		const promptedResponse = await within(
			fetch(`${running.apiUrl}/session/${sessionId}/message`, {
				method: "POST",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({
					model: { providerID: agent, modelID },
					parts: [{ type: "text", text: PROMPT }],
				}),
			}),
			`${agent} first GigaCode message`,
			promptTimeoutMs,
		);
		const messageRoundTrip = performance.now() - messageStarted;
		const responseBody = await promptedResponse.text();
		if (!promptedResponse.ok || !responseBody.includes(RESPONSE_TEXT)) {
			throw new Error(
				`prompt failed (${promptedResponse.status}): ${responseBody.slice(0, 4_000)}`,
			);
		}
		const providerRequests = mock.getRequests().length - requestsBefore;
		if (providerRequests === 0) {
			throw new Error(
				`${agent} prompt did not reach LLMock; refusing to report external provider traffic as a mock benchmark`,
			);
		}

		stage = "sessionLog";
		const events = await sessionEvents(running.stateDir, sessionId);
		const duration = (event: string) => {
			const value = events.findLast((record) => record.event === event)?.durationMs;
			if (typeof value !== "number") {
				throw new Error(`missing ${event} timing for ${sessionId}`);
			}
			return round(value);
		};
		const selected = events.findLast(
			(record) => record.event === "agentos.session.model.selected",
		)?.durationMs;
		return {
			timings: {
				logicalSessionCreate: round(logicalSessionCreate),
				sessionCreate: duration("agentos.session.created"),
				...(typeof selected === "number" ? { modelSelect: round(selected) } : {}),
				firstPrompt: duration("agentos.prompt.completed"),
				messageRoundTrip: round(messageRoundTrip),
				readyThroughFirstPrompt: round(performance.now() - totalStarted),
			},
			modelID,
			providerRequests,
			responseText: RESPONSE_TEXT,
		};
	} catch (error) {
		throw Object.assign(new Error(errorMessage(error)), { stage });
	} finally {
		if (sessionId) {
			await fetch(`${running.apiUrl}/session/${sessionId}`, { method: "DELETE" }).catch(
				() => undefined,
			);
		}
	}
}

async function main() {
	const mock = new LLMock({ port: 0, logLevel: "silent" });
	mock.addFixtures([
		{ match: { predicate: () => true }, response: { content: RESPONSE_TEXT } },
	]);
	const mockUrl = await mock.start();
	let running: RunningGigacode | undefined;
	try {
		running = await startGigacode(mockUrl);
		const providerResponse = await fetch(`${running.apiUrl}/provider`);
		if (!providerResponse.ok) {
			throw new Error(`provider list failed: ${await providerResponse.text()}`);
		}
		const providers = (await providerResponse.json()) as {
			all?: Provider[];
			default?: Record<string, string>;
		};
		const results: Record<
			AgentName,
			{ samples: Sample[]; failures: Failure[]; stats?: Record<string, MetricStats> }
		> = {} as never;

		for (const agent of agents) {
			const provider = providers.all?.find((candidate) => candidate.id === agent);
			if (!provider) throw new Error(`provider list omitted ${agent}`);
			const modelID = selectModel(agent, provider, providers.default?.[agent]);
			const samples: Sample[] = [];
			const failures: Failure[] = [];
			console.error(`\n=== ${agent} (${modelID}) ===`);
			for (let attempt = 0; attempt < warmup + iterations; attempt++) {
				const isWarmup = attempt < warmup;
				console.error(
					`  starting ${isWarmup ? "warmup" : `iter ${attempt - warmup + 1}`}...`,
				);
				try {
					const sample = await runAttempt(running, agent, modelID, mock);
					if (!isWarmup) samples.push(sample);
					console.error(
						`  ${isWarmup ? "warmup" : `iter ${attempt - warmup + 1}`}: session=${sample.timings.sessionCreate}ms prompt=${sample.timings.firstPrompt}ms HTTP=${sample.timings.messageRoundTrip}ms`,
					);
				} catch (error) {
					const failure = {
						attempt: isWarmup ? attempt + 1 : attempt - warmup + 1,
						warmup: isWarmup,
						stage: (error as { stage?: Stage }).stage ?? "logicalSessionCreate",
						error: errorMessage(error),
					};
					failures.push(failure);
					console.error(`  failed at ${failure.stage}: ${failure.error}`);
				}
			}
			results[agent] = { samples, failures };
			if (samples.length > 0) results[agent].stats = summarize(samples);
		}

		console.log(
			JSON.stringify(
				{
					benchmark: "gigacode-agent-acp-session-startup",
					timestamp: new Date().toISOString(),
					hardware: {
						cpu: os.cpus()[0]?.model ?? "unknown",
						cores: os.availableParallelism(),
						ramBytes: os.totalmem(),
						loadAverage: os.loadavg(),
						node: process.version,
						platform: `${os.platform()}-${os.arch()}`,
					},
					iterations,
					warmup,
					mockServer: true,
					discoverModels,
					comparison: {
						benchmark: "agent-acp-session-startup",
						script: "scripts/benchmarks/agent-session.bench.ts",
						results: "scripts/benchmarks/results/agent-session.json",
						note: "Compare sessionCreate and firstPrompt directly. Both scripts default to an empty workspace; pass the same --workspace path to both when measuring repository scan costs.",
					},
					measurementBoundaries: {
						logicalSessionCreate: "POST /session metadata plus canonical actor resolution",
						sessionCreate: "agentos.session.created: ACP spawn, initialize, and session/new",
						modelSelect: "agentos.session.model.selected",
						firstPrompt: "agentos.prompt.completed against the local LLMock response",
						messageRoundTrip: "complete POST /session/{id}/message HTTP request",
						readyThroughFirstPrompt: "logical session creation through first HTTP response",
					},
					daemon: {
						httpReady: running.daemonReadyMs,
						modelCatalogReady: running.modelCatalogReadyMs,
					},
					results,
				},
				null,
				2,
			),
		);
	} finally {
		if (running) await stopGigacode(running);
		await mock.stop();
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
