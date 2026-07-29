/**
 * Direct ACP vs GigaCode startup benchmark (Pi + LLMock).
 *
 * Both lanes reuse one warm AgentOS VM/workspace actor. This isolates the cost
 * of creating an ACP adapter session and sending its first mocked prompt from
 * daemon compilation, model-catalog refresh, and real model latency.
 *
 * Usage:
 *   pnpm exec tsx scripts/benchmarks/gigacode-session.bench.ts
 *   pnpm exec tsx scripts/benchmarks/gigacode-session.bench.ts --iterations=5 --warmup=1
 *   pnpm exec tsx scripts/benchmarks/gigacode-session.bench.ts --workspace=$PWD
 */

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import os, { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { AgentOs } from "@rivet-dev/agentos-core";
import { LLMock } from "@copilotkit/llmock";

const ROOT = resolve(import.meta.dirname, "../..");
const ENTRYPOINT = resolve(ROOT, "experiments/gigacode/gigacode.ts");
const TSX_IMPORT = createRequire(
	resolve(ROOT, "experiments/gigacode/package.json"),
).resolve("tsx");
const RESPONSE_TEXT = "GIGACODE_STARTUP_BENCH_OK";
const PROMPT = `Reply with exactly: ${RESPONSE_TEXT}`;

const argv = process.argv.slice(2);
const arg = (name: string, fallback: string) =>
	argv.find((value) => value.startsWith(`--${name}=`))?.split("=")[1] ?? fallback;
const iterations = Number.parseInt(arg("iterations", "3"), 10);
const warmup = Number.parseInt(arg("warmup", "1"), 10);
const requestedWorkspace = arg("workspace", "");
if (!Number.isInteger(iterations) || iterations < 1) {
	throw new Error("--iterations must be a positive integer");
}
if (!Number.isInteger(warmup) || warmup < 0) {
	throw new Error("--warmup must be a non-negative integer");
}

type Sample = {
	logicalSessionCreate?: number;
	piConfigPrepare?: number;
	acpSessionCreate: number;
	modelSelect: number;
	firstPrompt: number;
	readyThroughFirstPrompt: number;
};

function round(value: number): number {
	return Math.round(value * 100) / 100;
}

function stats(values: number[]) {
	const sorted = [...values].sort((a, b) => a - b);
	const percentile = (p: number) =>
		sorted[Math.max(0, Math.ceil((p / 100) * sorted.length) - 1)];
	return {
		mean: round(values.reduce((sum, value) => sum + value, 0) / values.length),
		p50: round(percentile(50)),
		p95: round(percentile(95)),
		min: round(sorted[0]),
		max: round(sorted.at(-1) ?? sorted[0]),
	};
}

function summarize(samples: Sample[]) {
	const keys = [
		"logicalSessionCreate",
		"piConfigPrepare",
		"acpSessionCreate",
		"modelSelect",
		"firstPrompt",
		"readyThroughFirstPrompt",
	] as const;
	return Object.fromEntries(
		keys.flatMap((key) => {
			const values = samples.flatMap((sample) =>
				typeof sample[key] === "number" ? [sample[key]] : [],
			);
			return values.length ? [[key, stats(values)]] : [];
		}),
	);
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
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		if (await condition()) return;
		await new Promise((resolveWait) => setTimeout(resolveWait, 100));
	}
	throw new Error(`${label} timed out after ${timeoutMs}ms`);
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

async function directLane(
	mockUrl: string,
): Promise<{ model: string; samples: Sample[] }> {
	if (
		requestedWorkspace &&
		!(await stat(resolve(requestedWorkspace)).catch(() => undefined))?.isDirectory()
	) {
		throw new Error(
			`--workspace must name an existing directory: ${resolve(requestedWorkspace)}`,
		);
	}
	const pi = (await import("@agentos-software/pi")).default;
	const sidecar = await AgentOs.createSidecar();
	const databaseDir = await mkdtemp(join(tmpdir(), "agentos-session-bench-db-"));
	const vm = await AgentOs.create({
		sidecar: { kind: "explicit", handle: sidecar },
		database: {
			type: "sqlite_file",
			path: join(databaseDir, "agentos.sqlite"),
		},
		software: [pi],
		...(requestedWorkspace
			? {
					mounts: [
						{
							path: "/home/agentos/workspace",
							plugin: {
								id: "host_dir",
								config: {
									hostPath: resolve(requestedWorkspace),
									readOnly: false,
								},
							},
							readOnly: false,
						},
					],
				}
			: {}),
		limits: { jsRuntime: { v8HeapLimitMb: 512 } },
		loopbackExemptPorts: [Number(new URL(mockUrl).port)],
		permissions: {
			fs: "allow",
			network: "allow",
			childProcess: "allow",
			process: "allow",
			env: "allow",
		},
	});
	const cwd = "/home/agentos/workspace";
	await vm.mkdir(cwd, { recursive: true });
	await vm.mkdir("/home/agentos/.pi/agent", { recursive: true });
	await vm.writeFile(
		"/home/agentos/.pi/agent/models.json",
		JSON.stringify({
			providers: {
				anthropic: { baseUrl: mockUrl, apiKey: "agentos-benchmark-key" },
			},
		}),
	);
	const env = {
		HOME: "/home/agentos",
		ANTHROPIC_API_KEY: "agentos-benchmark-key",
		ANTHROPIC_BASE_URL: mockUrl,
		PI_SKIP_VERSION_CHECK: "1",
	};
	let model = "";
	const samples: Sample[] = [];
	try {
		for (let attempt = 0; attempt < warmup + iterations; attempt++) {
			const totalStarted = performance.now();
			const sessionStarted = performance.now();
			const sessionId = `bench-pi-${attempt}`;
			await vm.openSession({ sessionId, agent: "pi", cwd, env });
			const acpSessionCreate = performance.now() - sessionStarted;
			const modelOption = (await vm.getSessionConfig({ sessionId })).options.find(
				(candidate) => candidate.category === "model",
			) as
				| { id: string; options?: Array<{ value?: string }> }
				| undefined;
			if (!model) {
				model =
					modelOption?.options?.find((candidate) =>
						candidate.value?.startsWith("anthropic/"),
					)?.value ?? "";
				if (!model) throw new Error("Pi advertised no Anthropic model");
			}
			if (!modelOption) throw new Error("Pi advertised no model selector");
			const modelStarted = performance.now();
			await vm.setSessionConfigOption({
				sessionId,
				configId: modelOption.id,
				value: model,
			});
			const modelSelect = performance.now() - modelStarted;
			const promptStarted = performance.now();
			const prompted = await vm.prompt({
				sessionId,
				content: [{ type: "text", text: PROMPT }],
			});
			const firstPrompt = performance.now() - promptStarted;
			const text =
				prompted.message?.content
					.filter((block) => block.type === "text")
					.map((block) => block.text)
					.join("") ?? "";
			if (!text.includes(RESPONSE_TEXT)) {
				throw new Error(`direct prompt failed: ${JSON.stringify(prompted)}`);
			}
			const sample = {
				acpSessionCreate: round(acpSessionCreate),
				modelSelect: round(modelSelect),
				firstPrompt: round(firstPrompt),
				readyThroughFirstPrompt: round(performance.now() - totalStarted),
			};
			await vm.deleteSession({ sessionId });
			if (attempt >= warmup) samples.push(sample);
			console.error(
				`  direct ${attempt < warmup ? "warmup" : attempt - warmup + 1}: session=${sample.acpSessionCreate}ms total=${sample.readyThroughFirstPrompt}ms`,
			);
		}
		return { model, samples };
	} finally {
		await vm.dispose();
		await sidecar.dispose();
		await rm(databaseDir, { recursive: true, force: true });
	}
}

type RunningGigacode = {
	apiUrl: string;
	child: ChildProcess;
	stateDir: string;
	daemonReadyMs: number;
	modelCatalogReadyMs: number;
};

async function startGigacode(mockUrl: string): Promise<RunningGigacode> {
	const apiPort = await freePort();
	const rivetPort = await freePort();
	const stateDir = await mkdtemp(resolve(tmpdir(), "gigacode-session-bench-"));
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
	await configurePiHome(piHome, mockUrl);
	await writeFile(
		resolve(opencodeConfig, "opencode.json"),
		`${JSON.stringify({
			autoupdate: false,
			share: "disabled",
			model: "anthropic/claude-sonnet-4-6",
			provider: { anthropic: { options: { baseURL: `${mockUrl}/v1` } } },
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
	await waitFor(
		async () =>
			await fetch(`${apiUrl}/global/health`)
				.then((response) => response.ok)
				.catch(() => false),
		"GigaCode HTTP API",
	);
	const daemonReadyMs = performance.now() - started;
	await waitFor(async () => {
		const health = await fetch(`${apiUrl}/global/health`)
			.then((response) => response.json() as Promise<{ modelCatalogStage?: string }>)
			.catch(() => undefined);
		return health?.modelCatalogStage === "model catalog is ready";
	}, "GigaCode model catalog");
	return {
		apiUrl,
		child,
		stateDir,
		daemonReadyMs: round(daemonReadyMs),
		modelCatalogReadyMs: round(performance.now() - started),
	};
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

async function sessionEvents(stateDir: string, sessionId: string) {
	const raw = await readFile(
		resolve(stateDir, "session-logs", `${sessionId}.jsonl`),
		"utf8",
	);
	return raw
		.trim()
		.split("\n")
		.filter(Boolean)
		.map((line) => JSON.parse(line) as Record<string, unknown>);
}

async function gigacodeLane(
	running: RunningGigacode,
	model: string,
): Promise<Sample[]> {
	const samples: Sample[] = [];
	for (let attempt = 0; attempt < warmup + iterations; attempt++) {
		const totalStarted = performance.now();
		const logicalStarted = performance.now();
		const createdResponse = await fetch(`${running.apiUrl}/session`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ title: "GigaCode startup benchmark" }),
		});
		if (!createdResponse.ok) {
			throw new Error(`GigaCode session create failed: ${await createdResponse.text()}`);
		}
		const created = (await createdResponse.json()) as { id?: string };
		if (!created.id) throw new Error("GigaCode session create returned no id");
		const logicalSessionCreate = performance.now() - logicalStarted;
		const promptedResponse = await fetch(
			`${running.apiUrl}/session/${created.id}/message`,
			{
				method: "POST",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({
					model: { providerID: "pi", modelID: model },
					parts: [{ type: "text", text: PROMPT }],
				}),
			},
		);
		const readyThroughFirstPrompt = performance.now() - totalStarted;
		const promptedText = await promptedResponse.text();
		if (!promptedResponse.ok || !promptedText.includes(RESPONSE_TEXT)) {
			throw new Error(`GigaCode prompt failed (${promptedResponse.status}): ${promptedText}`);
		}
		const events = await sessionEvents(running.stateDir, created.id);
		const duration = (event: string) => {
			const value = events.findLast((record) => record.event === event)?.durationMs;
			if (typeof value !== "number") throw new Error(`missing ${event} timing for ${created.id}`);
			return value;
		};
		const piConfig = events.findLast(
			(record) => record.event === "pi.configuration.prepared",
		)?.durationMs;
		const sample: Sample = {
			logicalSessionCreate: round(logicalSessionCreate),
			...(typeof piConfig === "number" ? { piConfigPrepare: round(piConfig) } : {}),
			acpSessionCreate: round(duration("agentos.session.created")),
			modelSelect: round(duration("agentos.session.model.selected")),
			firstPrompt: round(duration("agentos.prompt.completed")),
			readyThroughFirstPrompt: round(readyThroughFirstPrompt),
		};
		await fetch(`${running.apiUrl}/session/${created.id}`, { method: "DELETE" });
		if (attempt >= warmup) samples.push(sample);
		console.error(
			`  gigacode ${attempt < warmup ? "warmup" : attempt - warmup + 1}: session=${sample.acpSessionCreate}ms total=${sample.readyThroughFirstPrompt}ms`,
		);
	}
	return samples;
}

async function main() {
	const mock = new LLMock({ port: 0, logLevel: "silent" });
	mock.addFixtures([
		{ match: { predicate: () => true }, response: { content: RESPONSE_TEXT } },
	]);
	const mockUrl = await mock.start();
	let running: RunningGigacode | undefined;
	try {
		console.error("\n=== direct ACP (Pi) ===");
		const direct = await directLane(mockUrl);
		console.error("\n=== GigaCode (Pi) ===");
		running = await startGigacode(mockUrl);
		const gigacode = await gigacodeLane(running, direct.model);
		const directStats = summarize(direct.samples);
		const gigacodeStats = summarize(gigacode);
		const directAcpP50 = directStats.acpSessionCreate.p50;
		const gigacodeAcpP50 = gigacodeStats.acpSessionCreate.p50;
		console.log(
			JSON.stringify(
				{
					benchmark: "gigacode-vs-direct-acp-startup",
					timestamp: new Date().toISOString(),
					hardware: {
						cpu: os.cpus()[0]?.model ?? "unknown",
						cores: os.availableParallelism(),
						ramBytes: os.totalmem(),
						node: process.version,
					},
					iterations,
					warmup,
					model: direct.model,
					mockServer: true,
					measurementBoundaries: {
						logicalSessionCreate: "POST /session metadata and canonical actor resolution",
						acpSessionCreate: "adapter process spawn, ACP initialize, and session/new",
						modelSelect: "ACP session model selection",
						firstPrompt: "first ACP prompt through the local LLMock response",
						readyThroughFirstPrompt: "session creation through first complete response",
					},
					daemon: {
						httpReady: running.daemonReadyMs,
						modelCatalogReady: running.modelCatalogReadyMs,
					},
					lanes: {
						direct: { samples: direct.samples, stats: directStats },
						gigacode: { samples: gigacode, stats: gigacodeStats },
					},
					derived: {
						acpSessionOverheadMs: round(gigacodeAcpP50 - directAcpP50),
						acpSessionOverheadRatio: round(gigacodeAcpP50 / directAcpP50),
					},
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
