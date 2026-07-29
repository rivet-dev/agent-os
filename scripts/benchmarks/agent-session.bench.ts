/**
 * Cross-agent ACP startup benchmark.
 *
 * Measures four distinct boundaries for the supported ACP harnesses (Claude
 * Code, Pi, Codex, and OpenCode):
 *   vmCreate       AgentOS VM creation using one shared sidecar
 *   filesystemWarm first end-to-end shell/filesystem command
 *   sessionCreate  ACP process spawn + initialize + session/new completion
 *   firstPrompt    first complete model turn against a local LLMock server
 *
 * A failed agent is reported with its stage and underlying error while the
 * remaining agents continue. No real provider credentials or traffic are used.
 *
 * Usage:
 *   pnpm exec tsx scripts/benchmarks/agent-session.bench.ts
 *   pnpm exec tsx scripts/benchmarks/agent-session.bench.ts --iterations=3 --warmup=1
 *   pnpm exec tsx scripts/benchmarks/agent-session.bench.ts --agents=claude,pi
 *   pnpm exec tsx scripts/benchmarks/agent-session.bench.ts --software-dir=~/.local/share/gigacode/software
 *   pnpm exec tsx scripts/benchmarks/agent-session.bench.ts --agents=claude --workspace=$PWD
 *   pnpm exec tsx scripts/benchmarks/agent-session.bench.ts --agents=claude --all-software
 *   pnpm exec tsx scripts/benchmarks/agent-session.bench.ts --agents=claude --claude-config-dir=/tmp/claude-home
 *   pnpm exec tsx scripts/benchmarks/agent-session.bench.ts --agents=opencode --model=anthropic/claude-sonnet-4-6
 */

import { AgentOs, type AgentOsSidecar, type SoftwareInput } from "@rivet-dev/agentos-core";
import { LLMock } from "@copilotkit/llmock";
import { existsSync, mkdtempSync, rmSync, statSync } from "node:fs";
import os from "node:os";
import { join, resolve } from "node:path";
import { performance } from "node:perf_hooks";

type AgentName = "claude" | "pi" | "codex" | "opencode";
type Stage = "vmCreate" | "filesystemWarm" | "sessionCreate" | "firstPrompt";

type Timings = Record<Stage, number> & { readyThroughFirstPrompt: number };

interface Failure {
	attempt: number;
	warmup: boolean;
	stage: Stage;
	error: string;
}

interface AttemptResult {
	timings: Timings;
	agentInfo: unknown;
	processesAfterSessionCreate: number;
	providerRequests: number;
	responseText: string;
}

interface MetricStats {
	mean: number;
	p50: number;
	p95: number;
	min: number;
	max: number;
}

const ALL_AGENTS: AgentName[] = ["claude", "pi", "codex", "opencode"];
const DEFAULT_AGENTS: AgentName[] = ALL_AGENTS;
const RESPONSE_TEXT = "AGENTOS_SESSION_BENCH_OK";
const PROMPT = `Reply with exactly: ${RESPONSE_TEXT}`;

const args = process.argv.slice(2);
const valueArg = (name: string, fallback: string) =>
	args.find((value) => value.startsWith(`--${name}=`))?.split("=")[1] ?? fallback;
const iterations = Number.parseInt(valueArg("iterations", "3"), 10);
const warmup = Number.parseInt(valueArg("warmup", "1"), 10);
const promptTimeoutMs = Number.parseInt(valueArg("prompt-timeout-ms", "60000"), 10);
const requestedModel = valueArg("model", "");
const agents = valueArg("agents", DEFAULT_AGENTS.join(",")).split(",") as AgentName[];
const softwareDir = valueArg("software-dir", "");
const requestedWorkspace = valueArg("workspace", "");
const workspaceDir = requestedWorkspace ? resolve(requestedWorkspace) : "";
const loadAllSoftware = args.includes("--all-software");
const requestedClaudeConfig = valueArg("claude-config-dir", "");
const claudeConfigDir = requestedClaudeConfig
	? resolve(requestedClaudeConfig)
	: "";

for (const [flag, directory] of [
	["workspace", workspaceDir],
	["claude-config-dir", claudeConfigDir],
] as const) {
	if (directory && (!existsSync(directory) || !statSync(directory).isDirectory())) {
		throw new Error(`--${flag} must name an existing directory: ${directory}`);
	}
}

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

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

async function loadSoftware(agent: AgentName): Promise<SoftwareInput> {
	if (softwareDir) {
		const packageName = agent === "claude" ? "claude-code" : agent;
		return { packagePath: resolve(softwareDir, `${packageName}.aospkg`) };
	}
	switch (agent) {
		case "claude":
			return (await import("@agentos-software/claude-code")).default;
		case "pi":
			return (await import("@agentos-software/pi")).default;
		case "codex":
			return (await import("@agentos-software/codex")).default;
		case "opencode":
			return (await import("@agentos-software/opencode")).default;
	}
}

async function within<T>(
	promise: Promise<T>,
	label: string,
	timeoutMs: number,
): Promise<T> {
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

async function configureAgent(
	vm: AgentOs,
	agent: AgentName,
	mockUrl: string,
): Promise<{ cwd: string; env: Record<string, string> }> {
	const home = "/home/agentos";
	const cwd = `${home}/workspace`;
	await vm.mkdir(cwd, { recursive: true });
	const env: Record<string, string> = {
		HOME: home,
		CODEX_HOME: `${home}/.codex`,
		XDG_CACHE_HOME: `${home}/.cache`,
		XDG_CONFIG_HOME: `${home}/.config`,
		XDG_DATA_HOME: `${home}/.local/share`,
		XDG_STATE_HOME: `${home}/.local/state`,
		ANTHROPIC_API_KEY: "agentos-benchmark-key",
		ANTHROPIC_BASE_URL: mockUrl,
		OPENAI_API_KEY: "agentos-benchmark-key",
		OPENAI_BASE_URL: `${mockUrl}/v1`,
		PI_SKIP_VERSION_CHECK: "1",
	};
	await vm.mkdir(env.CODEX_HOME, { recursive: true });
	if (agent === "claude" && claudeConfigDir) {
		env.CLAUDE_CONFIG_DIR = "/home/agentos/.claude";
	}

	if (agent === "pi") {
		await vm.mkdir(`${home}/.pi/agent`, { recursive: true });
		await vm.writeFile(
			`${home}/.pi/agent/models.json`,
			JSON.stringify({
				providers: {
					anthropic: {
						baseUrl: mockUrl,
						apiKey: env.ANTHROPIC_API_KEY,
					},
				},
			}),
		);
	}

	if (agent === "opencode") {
		const model = requestedModel || "anthropic/claude-sonnet-4-20250514";
		const [providerID, ...modelParts] = model.split("/");
		const modelID = modelParts.join("/");
		const config = JSON.stringify({
			autoupdate: false,
			share: "disabled",
			snapshot: false,
			model,
			provider: {
				[providerID]: {
					options: { baseURL: `${mockUrl}/v1` },
					models: { [modelID]: { name: "AgentOS LLMock" } },
				},
			},
		});
		env.OPENCODE_AUTH_CONTENT = JSON.stringify({
			[providerID]: { type: "api", key: "agentos-benchmark-key" },
		});
		env.OPENCODE_CONFIG_CONTENT = config;
		await vm.mkdir(`${home}/.config/opencode`, { recursive: true });
		await vm.writeFile(
			`${home}/.config/opencode/opencode.json`,
			config,
		);
	}

	return { cwd, env };
}

async function runAttempt(
	agent: AgentName,
	sidecar: AgentOsSidecar,
	mock: LLMock,
	mockUrl: string,
): Promise<AttemptResult> {
	const databaseDir = mkdtempSync(join(os.tmpdir(), "agentos-session-bench-"));
	let stage: Stage = "vmCreate";
	let vm: AgentOs | undefined;
	let unsubscribeEvents: (() => void) | undefined;
	const sessionEvents: string[] = [];
	const requestsAtStart = mock.getRequests().length;
	const totalStart = performance.now();
	try {
		const software = loadAllSoftware
			? [
					...(await Promise.all(ALL_AGENTS.map(loadSoftware))),
					(await import("@agentos-software/ripgrep")).default,
				]
			: [await loadSoftware(agent)];
		const mounts = [
			...(workspaceDir
				? [
						{
							path: "/home/agentos/workspace",
							plugin: {
								id: "host_dir",
								config: { hostPath: workspaceDir, readOnly: false },
							},
							readOnly: false,
						},
					]
				: []),
			...(claudeConfigDir
				? [
						{
							path: "/home/agentos/.claude",
							plugin: {
								id: "host_dir",
								config: { hostPath: claudeConfigDir, readOnly: false },
							},
							readOnly: false,
						},
					]
				: []),
		];
		console.error(`    ${agent}: creating VM`);
		const vmStarted = performance.now();
		vm = await within(
			AgentOs.create({
				sidecar: { kind: "explicit", handle: sidecar },
				database: {
					type: "sqlite_file",
					path: join(databaseDir, "agentos.sqlite"),
				},
				software,
				...(mounts.length > 0 ? { mounts } : {}),
				limits: { jsRuntime: { v8HeapLimitMb: 512 } },
				loopbackExemptPorts: [Number(new URL(mockUrl).port)],
				permissions: {
					fs: "allow",
					network: "allow",
					childProcess: "allow",
					process: "allow",
					env: "allow",
				},
			}),
			`${agent} VM creation`,
			30_000,
		);
		const vmCreate = performance.now() - vmStarted;

		stage = "filesystemWarm";
		console.error(`    ${agent}: warming filesystem`);
		const filesystemStarted = performance.now();
		const filesystem = await within(
			vm.execArgv("sh", [
				"-c",
				"printf AGENTOS_FS_WARM_OK > /tmp/agentos-session-bench && cat /tmp/agentos-session-bench",
			]),
			`${agent} filesystem warmup`,
			30_000,
		);
		if (filesystem.stdout !== "AGENTOS_FS_WARM_OK") {
			throw new Error(
				`filesystem warmup returned ${JSON.stringify(filesystem.stdout)}: ${filesystem.stderr}`,
			);
		}
		const filesystemWarm = performance.now() - filesystemStarted;

		const { cwd, env } = await configureAgent(vm, agent, mockUrl);
		stage = "sessionCreate";
		console.error(`    ${agent}: creating ACP session`);
		const sessionStarted = performance.now();
		const sessionId = `bench-${agent}`;
		await within(
			vm.openSession({ sessionId, agent, cwd, env }),
			`${agent} ACP session creation`,
			60_000,
		);
		unsubscribeEvents = vm.onSessionEvent(sessionId, (event) => {
			sessionEvents.push(event.type);
			if (sessionEvents.length > 8) sessionEvents.shift();
		});
		const sessionCreate = performance.now() - sessionStarted;
		const processesAfterSessionCreate = vm.listProcesses().filter((process) => process.running).length;
		stage = "firstPrompt";
		console.error(`    ${agent}: sending first mock prompt`);
		if (agent === "pi") {
			const modelOption = (await vm.getSessionConfig({ sessionId })).options
				.find((option) => option.category === "model") as
				| { id: string; options?: Array<{ value?: string }> }
				| undefined;
			const benchmarkModel = modelOption?.options?.find((option) =>
				option.value?.startsWith("anthropic/"),
			)?.value;
			if (!benchmarkModel) {
				throw new Error(
					`Pi did not advertise an Anthropic model: ${JSON.stringify(modelOption)}`,
				);
			}
			await within(
				vm.setSessionConfigOption({
					sessionId,
					configId: modelOption.id,
					value: benchmarkModel,
				}),
				"pi benchmark model selection",
				15_000,
			);
		}

		const requestsBefore = mock.getRequests().length;
		const promptStarted = performance.now();
		const response = await within(
			vm.prompt({
				sessionId,
				content: [{ type: "text", text: PROMPT }],
			}),
			`${agent} first prompt`,
			promptTimeoutMs,
		);
		const firstPrompt = performance.now() - promptStarted;
		const text =
			response.message?.content
				.filter((block) => block.type === "text")
				.map((block) => block.text)
				.join("") ?? "";
		if (!text.includes(RESPONSE_TEXT)) {
			throw new Error(`unexpected mock response: ${JSON.stringify(text)}`);
		}
		const providerRequests = mock.getRequests().length - requestsBefore;
		if (providerRequests === 0) {
			throw new Error(`${agent} prompt did not reach LLMock`);
		}

		return {
			timings: {
				vmCreate: round(vmCreate),
				filesystemWarm: round(filesystemWarm),
				sessionCreate: round(sessionCreate),
				firstPrompt: round(firstPrompt),
				readyThroughFirstPrompt: round(performance.now() - totalStart),
			},
			agentInfo: await vm.getSessionAgentInfo({ sessionId }),
			processesAfterSessionCreate,
			providerRequests,
			responseText: text,
		};
	} catch (error) {
		const providerRequests = mock.getRequests().length - requestsAtStart;
		let piTranscript = "";
		let opencodeLog = "";
		if (agent === "pi" && vm) {
			try {
				const entries = await within(
					vm.readdirRecursive("/home/agentos/.pi/agent/sessions"),
					"Pi transcript listing",
					2_000,
				);
				const transcript = entries.findLast((entry) => entry.path.endsWith(".jsonl"));
				if (transcript) {
					piTranscript = new TextDecoder()
						.decode(await within(vm.readFile(transcript.path), "Pi transcript read", 2_000))
						.slice(-4000);
				}
			} catch {
				// Best-effort failure context only.
			}
		}
		if (agent === "opencode" && vm) {
			try {
				opencodeLog = new TextDecoder()
					.decode(
						await within(
							vm.readFile("/home/agentos/.local/share/opencode/log/opencode.log"),
							"OpenCode log read",
							2_000,
						),
					)
					.slice(-8_000);
			} catch {
				// Best-effort failure context only.
			}
		}
		const diagnostics = sessionEvents.length
			? `; providerRequests=${providerRequests}; lastEvents=${JSON.stringify(sessionEvents)}${piTranscript ? `; piTranscript=${JSON.stringify(piTranscript)}` : ""}${opencodeLog ? `; opencodeLog=${JSON.stringify(opencodeLog)}` : ""}`
			: `; providerRequests=${providerRequests}${opencodeLog ? `; opencodeLog=${JSON.stringify(opencodeLog)}` : ""}`;
		throw Object.assign(new Error(`${errorMessage(error)}${diagnostics}`), {
			stage,
		});
	} finally {
		unsubscribeEvents?.();
		if (vm) {
			await within(vm.dispose(), `${agent} VM disposal`, 10_000).catch(() => undefined);
		}
		rmSync(databaseDir, { recursive: true, force: true });
	}
}

async function main() {
	const mock = new LLMock({ port: 0, logLevel: "silent" });
	mock.addFixtures([
		{ match: { predicate: () => true }, response: { content: RESPONSE_TEXT } },
	]);
	const mockUrl = await mock.start();
	const sidecar = await AgentOs.createSidecar();
	const results: Record<
		AgentName,
		{
			samples: AttemptResult[];
			failures: Failure[];
			stats?: Record<keyof Timings, MetricStats>;
		}
	> = {} as never;

	try {
		for (const agent of agents) {
			const samples: AttemptResult[] = [];
			const failures: Failure[] = [];
			console.error(`\n=== ${agent} ===`);
			for (let attempt = 0; attempt < warmup + iterations; attempt++) {
				const isWarmup = attempt < warmup;
				console.error(
					`  starting ${isWarmup ? "warmup" : `iter ${attempt - warmup + 1}`}...`,
				);
				try {
					const sample = await runAttempt(agent, sidecar, mock, mockUrl);
					if (!isWarmup) samples.push(sample);
					console.error(
						`  ${isWarmup ? "warmup" : `iter ${attempt - warmup + 1}`}: session=${sample.timings.sessionCreate}ms prompt=${sample.timings.firstPrompt}ms`,
					);
				} catch (error) {
					const stage =
						(error as { stage?: Stage }).stage ?? "vmCreate";
					const failure = {
						attempt: isWarmup ? attempt + 1 : attempt - warmup + 1,
						warmup: isWarmup,
						stage,
						error: errorMessage(error),
					};
					failures.push(failure);
					console.error(`  failed at ${stage}: ${failure.error}`);
				}
			}

			results[agent] = { samples, failures };
			if (samples.length > 0) {
				results[agent].stats = Object.fromEntries(
					(
						[
							"vmCreate",
							"filesystemWarm",
							"sessionCreate",
							"firstPrompt",
							"readyThroughFirstPrompt",
						] as const
					).map((metric) => [
						metric,
						stats(samples.map((sample) => sample.timings[metric])),
					]),
				) as Record<keyof Timings, MetricStats>;
			}
		}
	} finally {
		await sidecar.dispose();
		await mock.stop();
	}

	console.log(
		JSON.stringify(
			{
				benchmark: "agent-acp-session-startup",
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
				measurementBoundaries: {
					sessionCreate:
						"vm.openSession: ACP adapter process spawn, initialize, and session/new",
					firstPrompt: "vm.prompt through the local LLM mock response",
				},
				results,
			},
			null,
			2,
		),
	);
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
