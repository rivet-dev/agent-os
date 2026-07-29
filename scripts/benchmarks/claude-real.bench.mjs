/**
 * Compare raw AgentOS ACP with installed GigaCode using real Claude credentials.
 * This benchmark never starts LLMock and creates a fresh ACP process per sample.
 *
 * Usage:
 *   node scripts/benchmarks/claude-real.bench.mjs --iterations=5
 *   node scripts/benchmarks/claude-real.bench.mjs --mode=raw --keep-artifacts
 */

import { spawn } from "node:child_process";
import { existsSync, realpathSync, statSync } from "node:fs";
import {
	copyFile,
	mkdtemp,
	mkdir,
	readFile,
	readdir,
	rm,
	writeFile,
} from "node:fs/promises";
import { createServer } from "node:net";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { pathToFileURL } from "node:url";

const SENTINEL = "AGENTOS_REAL_CLAUDE_BENCH_OK";
const PROMPT = `Reply with exactly: ${SENTINEL}`;
const args = process.argv.slice(2);
const valueArg = (name, fallback) =>
	args.find((value) => value.startsWith(`--${name}=`))?.split("=")[1] ?? fallback;
const iterations = Number.parseInt(valueArg("iterations", "3"), 10);
const promptTimeoutMs = Number.parseInt(
	valueArg("prompt-timeout-ms", "120000"),
	10,
);
const installDir = resolve(
	valueArg("install-dir", join(homedir(), ".local", "share", "gigacode")),
);
const softwareDir = resolve(valueArg("software-dir", join(installDir, "software")));
const claudeConfigDir = realpathSync(
	resolve(valueArg("claude-config-dir", join(homedir(), ".claude"))),
);
const workspace = realpathSync(resolve(valueArg("workspace", process.cwd())));
const mode = valueArg("mode", "both");
const keepArtifacts = args.includes("--keep-artifacts");
const outputPath = valueArg("output", "");

if (!Number.isInteger(iterations) || iterations < 2) {
	throw new Error("--iterations must be an integer of at least 2");
}
if (!Number.isInteger(promptTimeoutMs) || promptTimeoutMs < 1) {
	throw new Error("--prompt-timeout-ms must be a positive integer");
}
if (!["raw", "gigacode", "both"].includes(mode)) {
	throw new Error('--mode must be "raw", "gigacode", or "both"');
}

const round = (value) => Math.round(value * 100) / 100;
const sleep = (ms) => new Promise((resolveWait) => setTimeout(resolveWait, ms));

async function within(promise, label, timeoutMs) {
	let timer;
	try {
		return await Promise.race([
			promise,
			new Promise((_, reject) => {
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

function stats(values) {
	const sorted = [...values].sort((a, b) => a - b);
	const percentile = (value) =>
		sorted[Math.max(0, Math.ceil((value / 100) * sorted.length) - 1)];
	return {
		mean: round(values.reduce((sum, value) => sum + value, 0) / values.length),
		p50: round(percentile(50)),
		p95: round(percentile(95)),
		min: round(sorted[0]),
		max: round(sorted.at(-1) ?? sorted[0]),
	};
}

function summarize(samples) {
	const keys = [...new Set(samples.flatMap((sample) => Object.keys(sample.timings)))];
	return Object.fromEntries(
		keys.map((key) => [key, stats(samples.map((sample) => sample.timings[key]))]),
	);
}

async function freePort() {
	const server = createServer();
	await new Promise((resolveReady, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", resolveReady);
	});
	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("could not reserve a loopback port");
	}
	await new Promise((resolveClosed, reject) =>
		server.close((error) => (error ? reject(error) : resolveClosed())),
	);
	return address.port;
}

async function waitFor(condition, label, timeoutMs = 120_000) {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		if (await condition()) return;
		await sleep(100);
	}
	throw new Error(`${label} timed out after ${timeoutMs}ms`);
}

async function findInstalledCoreEntrypoint() {
	const pnpmDir = join(installDir, "node_modules", ".pnpm");
	const entries = await readdir(pnpmDir, { withFileTypes: true });
	const candidates = entries
		.filter(
			(entry) =>
				entry.isDirectory() && entry.name.startsWith("@rivet-dev+agentos-core@"),
		)
		.map((entry) =>
			join(
				pnpmDir,
				entry.name,
				"node_modules",
				"@rivet-dev",
				"agentos-core",
				"dist",
				"index.js",
			),
		)
		.filter(existsSync);
	if (candidates.length !== 1) {
		throw new Error(
			`expected one installed @rivet-dev/agentos-core, found ${candidates.length}`,
		);
	}
	return candidates[0];
}

async function installedSoftware() {
	const entries = await readdir(softwareDir, { withFileTypes: true });
	return entries
		.filter((entry) => entry.isFile() && entry.name.endsWith(".aospkg"))
		.map((entry) => ({ packagePath: join(softwareDir, entry.name) }))
		.sort((a, b) => a.packagePath.localeCompare(b.packagePath));
}

function hostMount(hostPath, guestPath) {
	return {
		path: guestPath,
		plugin: { id: "host_dir", config: { hostPath, readOnly: false } },
		readOnly: false,
	};
}

function credentialMounts() {
	const candidates = [
		[claudeConfigDir, "/home/agentos/.claude"],
		[join(homedir(), ".codex"), "/home/agentos/.codex"],
		[join(homedir(), ".pi"), "/home/agentos/.pi"],
		[join(homedir(), ".config", "opencode"), "/home/agentos/.config/opencode"],
		[
			join(homedir(), ".local", "share", "opencode"),
			"/home/agentos/.local/share/opencode",
		],
	];
	return candidates.flatMap(([hostPath, guestPath]) =>
		existsSync(hostPath) && statSync(hostPath).isDirectory()
			? [hostMount(hostPath, guestPath)]
			: [],
	);
}

function localInstructions() {
	return `
Gigacode is a local AgentOS VM experiment, not a security boundary.
The host project ${workspace} is mounted read-write at /workspace,
which is your working directory.

Use your normal shell and execution tools for commands; they execute inside the
local AgentOS VM, not Docker. Writes under /workspace affect the real project
immediately, subject to the daemon user's operating-system permissions. Other
host directories are not mounted into this VM.
`.trim();
}

function claudeProviderEnvironment() {
	return Object.fromEntries(
		[
			"ANTHROPIC_API_KEY",
			"ANTHROPIC_AUTH_TOKEN",
			"ANTHROPIC_BASE_URL",
			"DEBUG_SDK",
			"DEBUG_CLAUDE_AGENT_SDK",
			"CLAUDE_CODE_DEBUG_LOGS_DIR",
			"CLAUDE_CODE_DEBUG_LOG_LEVEL",
			"CLAUDE_CODE_SLOW_OPERATION_THRESHOLD_MS",
		].flatMap(
			(name) => (process.env[name] === undefined ? [] : [[name, process.env[name]]]),
		),
	);
}

function providerLoopbackPorts() {
	const baseUrl = process.env.ANTHROPIC_BASE_URL;
	if (!baseUrl) return [];
	const url = new URL(baseUrl);
	if (url.hostname !== "127.0.0.1" && url.hostname !== "localhost") return [];
	return [Number(url.port || (url.protocol === "https:" ? 443 : 80))];
}

function parseSidecarPhases(raw) {
	const groups = [];
	let current = {};
	for (const line of raw.split("\n")) {
		const match = line.match(/phase=(?:"([^"]+)"|([^\s]+)).*?elapsed_ms=(\d+)/);
		if (!match) continue;
		const phase = match[1] ?? match[2];
		current[phase] = Number(match[3]);
		if (phase !== "session_inner_done") continue;
		groups.push({
			spawnProcess: current.spawn_process,
			acpInitialize: current.acp_initialize,
			acpSessionNew:
				current.acp_session_new !== undefined && current.acp_initialize !== undefined
					? current.acp_session_new - current.acp_initialize
					: undefined,
			sessionInnerTotal: current.session_inner_done,
			postHandshake:
				current.spawn_process !== undefined && current.acp_session_new !== undefined
					? current.session_inner_done -
						current.spawn_process -
						current.acp_session_new
					: undefined,
		});
		current = {};
	}
	return groups;
}

async function benchmarkRaw(artifactDir) {
	const sidecarLog = join(artifactDir, "raw-sidecar.log");
	process.env.AGENTOS_SIDECAR_BIN = join(installDir, "native", "agentos-sidecar");
	process.env.AGENTOS_LOG_FILE = sidecarLog;
	process.env.AGENTOS_LOG_LEVEL = "info";
	const { AgentOs } = await import(
		pathToFileURL(await findInstalledCoreEntrypoint()).href
	);
	const sidecar = await AgentOs.createSidecar();
	const software = await installedSoftware();
	const samples = [];
	try {
		for (let index = 0; index < iterations; index++) {
			console.error(`raw AgentOS ACP sample ${index + 1}/${iterations}`);
			let vm;
			const vmStarted = performance.now();
			try {
				vm = await within(
					AgentOs.create({
						sidecar: { kind: "explicit", handle: sidecar },
						database: {
							type: "sqlite_file",
							path: join(artifactDir, `raw-session-${index}.sqlite`),
						},
						software,
						defaultSoftware: false,
						loopbackExemptPorts: providerLoopbackPorts(),
						limits: { jsRuntime: { v8HeapLimitMb: 512 } },
						permissions: {
							fs: "allow",
							network: "allow",
							childProcess: "allow",
							process: "allow",
							env: "allow",
						},
						mounts: [
							...credentialMounts(),
							hostMount(workspace, "/workspace"),
						],
					}),
					"raw AgentOS VM creation",
					60_000,
				);
				const vmCreate = performance.now() - vmStarted;
				await vm.execArgv("sh", [
					"-c",
					"test -d /workspace && test -f /home/agentos/.claude/.credentials.json",
				]);
				const readyStarted = performance.now();
				const sessionStarted = performance.now();
				const sessionId = `bench-claude-${index}`;
				await within(
					vm.openSession({
						sessionId,
						agent: "claude",
						cwd: "/workspace",
						additionalInstructions: localInstructions(),
						env: {
							HOME: "/home/agentos",
							PWD: "/workspace",
							CLAUDE_CONFIG_DIR: "/home/agentos/.claude",
							...claudeProviderEnvironment(),
						},
					}),
					"raw Claude ACP session creation",
					60_000,
				);
				const sessionCreate = performance.now() - sessionStarted;
				const promptStarted = performance.now();
				const prompted = await within(
					vm.prompt({
						sessionId,
						content: [{ type: "text", text: PROMPT }],
					}),
					"raw real Claude prompt",
					promptTimeoutMs,
				);
				const firstPrompt = performance.now() - promptStarted;
				const text =
					prompted.message?.content
						.filter((block) => block.type === "text")
						.map((block) => block.text)
						.join("") ?? "";
				if (!text.includes(SENTINEL)) {
					throw new Error(`unexpected Claude response: ${JSON.stringify(prompted)}`);
				}
				samples.push({
					timings: {
						vmCreate: round(vmCreate),
						sessionCreate: round(sessionCreate),
						sessionSetup: 0,
						firstPrompt: round(firstPrompt),
						readyThroughFirstPrompt: round(performance.now() - readyStarted),
					},
					agentInfo: await vm.getSessionAgentInfo({ sessionId }),
				});
			} finally {
				if (vm) await within(vm.dispose(), "raw AgentOS VM disposal", 15_000);
			}
		}
	} finally {
		await sidecar.dispose();
		delete process.env.AGENTOS_LOG_FILE;
	}
	const phases = parseSidecarPhases(await readFile(sidecarLog, "utf8"));
	for (const [index, sample] of samples.entries()) sample.sidecar = phases[index];
	return {
		samples,
		stats: summarize(samples),
		sidecarPhases: phases,
		sidecarStats: summarize(phases.map((timings) => ({ timings }))),
	};
}

async function startGigacode(artifactDir) {
	const apiPort = await freePort();
	const rivetPort = await freePort();
	const stateDir = join(artifactDir, "gigacode-state");
	const sidecarLog = join(artifactDir, "gigacode-sidecar.log");
	await mkdir(stateDir, { recursive: true });
	const modelCache = join(homedir(), ".local", "state", "gigacode", "models.json");
	if (existsSync(modelCache)) await copyFile(modelCache, join(stateDir, "models.json"));
	const output = [];
	const child = spawn(join(homedir(), ".local", "bin", "gigacode"), ["daemon"], {
		cwd: workspace,
		stdio: ["ignore", "pipe", "pipe"],
		env: {
			...process.env,
			GIGACODE_PORT: String(apiPort),
			GIGACODE_RIVET_PORT: String(rivetPort),
			GIGACODE_STATE_DIR: stateDir,
			RIVETKIT_STORAGE_PATH: join(stateDir, "engine"),
			GIGACODE_WORKSPACE: workspace,
			GIGACODE_CLAUDE_CONFIG_DIR: claudeConfigDir,
			GIGACODE_MODEL_REFRESH_DELAY_MS: "3600000",
			GIGACODE_DISABLE_OPEN_URL: "1",
			AGENTOS_LOG_FILE: sidecarLog,
			AGENTOS_LOG_LEVEL: "info",
		},
	});
	for (const stream of [child.stdout, child.stderr]) {
		stream?.on("data", (chunk) => {
			output.push(chunk.toString());
			if (output.length > 1_000) output.shift();
		});
	}
	const apiUrl = `http://127.0.0.1:${apiPort}/opencode`;
	await waitFor(async () => {
		const health = await fetch(`${apiUrl}/global/health`)
			.then((response) => response.json())
			.catch(() => undefined);
		return health?.rivetReady === true;
	}, "isolated GigaCode daemon");
	return { apiUrl, child, output, sidecarLog, stateDir };
}

async function stopGigacode(running) {
	running.child.kill("SIGTERM");
	await Promise.race([
		new Promise((resolveExit) => running.child.once("exit", resolveExit)),
		sleep(15_000),
	]);
	if (running.child.exitCode === null) running.child.kill("SIGKILL");
	await writeFile(
		join(dirname(running.sidecarLog), "gigacode-daemon.log"),
		running.output.join(""),
	);
}

async function sessionEvents(stateDir, sessionId) {
	const path = join(stateDir, "session-logs", `${sessionId}.jsonl`);
	let raw = "";
	await waitFor(async () => {
		raw = await readFile(path, "utf8").catch(() => "");
		return raw.includes('"event":"prompt.completed"');
	}, `${sessionId} session log`, 10_000);
	return raw
		.trim()
		.split("\n")
		.filter(Boolean)
		.map((line) => JSON.parse(line));
}

async function benchmarkGigacode(artifactDir) {
	const running = await startGigacode(artifactDir);
	const samples = [];
	try {
		for (let index = 0; index < iterations; index++) {
			console.error(`GigaCode ACP sample ${index + 1}/${iterations}`);
			let sessionId;
			try {
				const readyStarted = performance.now();
				const logicalStarted = performance.now();
				const createdResponse = await within(
					fetch(`${running.apiUrl}/session`, {
						method: "POST",
						headers: { "content-type": "application/json" },
						body: JSON.stringify({ title: `real Claude sample ${index + 1}` }),
					}),
					"GigaCode logical session creation",
					30_000,
				);
				const logicalSessionCreate = performance.now() - logicalStarted;
				if (!createdResponse.ok) {
					throw new Error(`session creation failed: ${await createdResponse.text()}`);
				}
				sessionId = (await createdResponse.json()).id;
				if (!sessionId) throw new Error("GigaCode returned no session ID");
				const messageStarted = performance.now();
				const promptedResponse = await within(
					fetch(`${running.apiUrl}/session/${sessionId}/message`, {
						method: "POST",
						headers: { "content-type": "application/json" },
						body: JSON.stringify({
							model: { providerID: "claude", modelID: "default" },
							parts: [{ type: "text", text: PROMPT }],
						}),
					}),
					"GigaCode real Claude prompt",
					promptTimeoutMs,
				);
				const messageRoundTrip = performance.now() - messageStarted;
				const responseBody = await promptedResponse.text();
				if (!promptedResponse.ok || !responseBody.includes(SENTINEL)) {
					throw new Error(
						`unexpected GigaCode response (${promptedResponse.status}): ${responseBody.slice(0, 4000)}`,
					);
				}
				const events = await sessionEvents(running.stateDir, sessionId);
				const event = (name) => events.findLast((record) => record.event === name);
				const created = event("agentos.session.created");
				const promptStarted = event("agentos.prompt.started");
				const promptCompleted = event("agentos.prompt.completed");
				if (!created || !promptStarted || !promptCompleted) {
					throw new Error(`missing timing events for ${sessionId}`);
				}
				samples.push({
					sessionId,
					timings: {
						logicalSessionCreate: round(logicalSessionCreate),
						sessionCreate: round(created.durationMs),
						sessionSetup: round(promptStarted.time - created.time),
						firstPrompt: round(promptCompleted.durationMs),
						messageRoundTrip: round(messageRoundTrip),
						readyThroughFirstPrompt: round(performance.now() - readyStarted),
					},
					attempt: created.attempt,
					actorSessionId: created.actorSessionId,
				});
			} finally {
				if (sessionId) {
					const deleted = await fetch(`${running.apiUrl}/session/${sessionId}`, {
						method: "DELETE",
					});
					if (!deleted.ok) {
						throw new Error(`failed to delete ${sessionId}: ${await deleted.text()}`);
					}
					await sleep(250);
				}
			}
		}
	} catch (error) {
		throw new Error(
			`${error instanceof Error ? error.message : String(error)}\nGigaCode daemon output:\n${running.output.join("").slice(-12_000)}`,
		);
	} finally {
		await stopGigacode(running);
	}
	const phases = parseSidecarPhases(await readFile(running.sidecarLog, "utf8"));
	for (const [index, sample] of samples.entries()) sample.sidecar = phases[index];
	return {
		samples,
		stats: summarize(samples),
		sidecarPhases: phases,
		sidecarStats: summarize(phases.map((timings) => ({ timings }))),
	};
}

async function main() {
	const artifactDir = await mkdtemp(join(tmpdir(), "claude-real-acp-bench-"));
	try {
		const raw = mode === "gigacode" ? undefined : await benchmarkRaw(artifactDir);
		const gigacode = mode === "raw" ? undefined : await benchmarkGigacode(artifactDir);
		const report = {
			benchmark: "real-claude-acp-session-startup",
			timestamp: new Date().toISOString(),
			iterations,
			mockServer: false,
			installedArtifacts: {
				installDir,
				softwareDir,
				sidecar: join(installDir, "native", "agentos-sidecar"),
				claudePackage: join(softwareDir, "claude-code.aospkg"),
			},
			configuration: { workspace, claudeConfigDir, prompt: PROMPT },
			measurementBoundaries: {
				sessionCreate: "ACP adapter spawn + initialize + session/new",
				sessionSetup:
					"post-creation forwarding/setup before prompt dispatch (zero in raw ACP)",
				firstPrompt: "real Claude turn after dispatch",
				spawnProcess:
					"sidecar create-session entry through adapter process spawn",
				acpInitialize: "ACP initialize request/response",
				acpSessionNew: "ACP session/new request/response",
			},
			artifactDir: keepArtifacts ? artifactDir : undefined,
			raw,
			gigacode,
		};
		const serialized = `${JSON.stringify(report, null, 2)}\n`;
		if (outputPath) {
			const absoluteOutput = resolve(outputPath);
			await mkdir(dirname(absoluteOutput), { recursive: true });
			await writeFile(absoluteOutput, serialized);
		}
		if (keepArtifacts) await writeFile(join(artifactDir, "report.json"), serialized);
		process.stdout.write(serialized);
	} finally {
		if (!keepArtifacts) await rm(artifactDir, { recursive: true, force: true });
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
