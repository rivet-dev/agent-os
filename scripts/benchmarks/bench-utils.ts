import { AgentOs, type SoftwareInput } from "@rivet-dev/agent-os";
import { coreutils } from "@rivet-dev/agent-os-common";
import claude from "@rivet-dev/agent-os-claude";
import codex from "@rivet-dev/agent-os-codex-agent";
import pi from "@rivet-dev/agent-os-pi";
import { LLMock } from "@copilotkit/llmock";
import os from "node:os";
import { resolve } from "node:path";

// Benchmark parameters. Keep batch sizes minimal for fast iteration.
export const BATCH_SIZES = [1, 10];
export const ITERATIONS = 5;
export const WARMUP_ITERATIONS = 1;
export const MAX_CONCURRENCY = Math.max(1, os.availableParallelism() - 4);

export const ECHO_COMMAND = "echo hello";
export const EXPECTED_OUTPUT = "hello\n";
export const PI_BENCHMARK_PROMPT = "Reply with exactly: Hello from llmock";
export const PI_HEADLESS_BLOCKER_REFERENCE =
	"packages/core/tests/pi-headless.test.ts";
export const PI_HEADLESS_BLOCKER_REASON =
	'Standalone `spawn("pi", ...)` is not exposed on the native sidecar PATH; use `createSession("pi-cli")` to benchmark the native PI CLI RPC path tracked in packages/core/tests/pi-headless.test.ts.';
const BENCHMARK_MODULE_ACCESS_CWD = resolve(
	import.meta.dirname,
	"../../packages/core",
);

// ── Shared mock LLM server ─────────────────────────────────────────

let _llmock: LLMock | undefined;
let _llmockUrl: string | undefined;
let _llmockPort: number | undefined;

/** Start a shared llmock server (idempotent). */
export async function ensureLlmock(): Promise<{
	url: string;
	port: number;
}> {
	if (_llmock) return { url: _llmockUrl!, port: _llmockPort! };
	_llmock = new LLMock({ port: 0, logLevel: "silent" });
	_llmock.addFixtures([
		{
			match: { predicate: () => true },
			response: { content: "Hello from llmock" },
		},
	]);
	_llmockUrl = await _llmock.start();
	_llmockPort = Number(new URL(_llmockUrl).port);
	return { url: _llmockUrl, port: _llmockPort };
}

/** Stop the shared llmock server. */
export async function stopLlmock(): Promise<void> {
	if (_llmock) {
		await _llmock.stop();
		_llmock = undefined;
		_llmockUrl = undefined;
		_llmockPort = undefined;
	}
}

export function getLlmockRequestCount(): number {
	return _llmock?.getRequests().length ?? 0;
}

// ── Workload abstraction ────────────────────────────────────────────

export interface WorkloadObservation {
	promptCompleted?: boolean;
	providerRequestCount?: number;
	sessionUpdateCount?: number;
	textEventCount?: number;
	finalText?: string | null;
	stopReason?: string;
	workloadPath?: string;
	substituteReason?: string;
	blockerReference?: string;
	blockerReason?: string;
}

/** A workload describes how to create a VM and start a long-running process for memory measurement. */
export interface Workload {
	name: string;
	description: string;
	createVm: () => Promise<AgentOs>;
	/** Start a long-running process so the Worker thread stays alive. */
	start: (vm: AgentOs) => Promise<WorkloadObservation | void> | WorkloadObservation | void;
	/** Verify the expected processes are running. Throws if not. */
	verify: (vm: AgentOs) => void;
	/** Time to wait after start for the process to fully initialize. */
	settleMs: number;
}

function makeAgentSessionWorkload(opts: {
	agentId: string;
	description: string;
	software: SoftwareInput[];
	processMarker: string;
}): Workload {
	return {
		name: `${opts.agentId}-session`,
		description: opts.description,
		createVm: async () => {
			const { port } = await ensureLlmock();
			return AgentOs.create({
				software: opts.software,
				loopbackExemptPorts: [port],
			});
		},
		start: async (vm) => {
			const { url } = await ensureLlmock();
			await vm.createSession(opts.agentId, {
				env: {
					ANTHROPIC_API_KEY: "bench-key",
					ANTHROPIC_BASE_URL: url,
				},
			});
		},
		verify: (vm) => {
			const procs = vm.listProcesses();
			const running = procs.filter((p) => p.running);
			const hasAgent = running.some(
				(p) =>
					p.command === "node" &&
					p.args.some((a) => a.includes(opts.processMarker)),
			);
			if (!hasAgent) {
				throw new Error(
					`Expected running ${opts.processMarker} process, got: ${JSON.stringify(running.map((p) => ({ cmd: p.command, args: p.args })))}`,
				);
			}
		},
		settleMs: 2000,
	};
}

function getTextEventPayload(
	event: unknown,
): { text?: string; type?: string } | undefined {
	if (!event || typeof event !== "object") {
		return undefined;
	}
	const params = (event as { params?: unknown }).params;
	if (!params || typeof params !== "object") {
		return undefined;
	}
	return params as { text?: string; type?: string };
}

function makeAgentPromptWorkload(opts: {
	agentId: string;
	description: string;
	software: SoftwareInput[];
	processMarker: string;
	prompt: string;
}): Workload {
	return {
		name: `${opts.agentId}-prompt-turn`,
		description: opts.description,
		createVm: async () => {
			const { port } = await ensureLlmock();
			return AgentOs.create({
				loopbackExemptPorts: [port],
				moduleAccessCwd: BENCHMARK_MODULE_ACCESS_CWD,
				software: opts.software,
			});
		},
		start: async (vm) => {
			const { url } = await ensureLlmock();
			const { sessionId } = await vm.createSession(opts.agentId, {
				env: {
					ANTHROPIC_API_KEY: "bench-key",
					ANTHROPIC_BASE_URL: url,
				},
			});

			const events: unknown[] = [];
			const unsubscribe = vm.onSessionEvent(sessionId, (event) => {
				events.push(event);
			});
			const requestCountBefore = getLlmockRequestCount();

			try {
				const response = await vm.prompt(sessionId, opts.prompt);
				if (response.error) {
					throw new Error(
						`${opts.agentId} prompt workload failed: ${response.error.message}`,
					);
				}
				const textEvents = events
					.map(getTextEventPayload)
					.filter((event) => event?.type === "text");
				const finalText = textEvents.at(-1)?.text ?? null;
				const providerRequestCount =
					getLlmockRequestCount() - requestCountBefore;

				return {
					promptCompleted: true,
					providerRequestCount,
					sessionUpdateCount: events.length,
					textEventCount: textEvents.length,
					finalText,
					stopReason: (response.result as { stopReason?: string } | undefined)
						?.stopReason,
					workloadPath:
						'createSession("pi-cli") + vm.prompt(...) via pi-acp -> PI CLI --mode rpc',
					blockerReference: PI_HEADLESS_BLOCKER_REFERENCE,
					blockerReason: PI_HEADLESS_BLOCKER_REASON,
				} satisfies WorkloadObservation;
			} finally {
				unsubscribe();
			}
		},
		verify: (vm) => {
			const procs = vm.listProcesses();
			const running = procs.filter((p) => p.running);
			const hasAgent = running.some(
				(p) =>
					p.command === "node" &&
					p.args.some((a) => a.includes(opts.processMarker)),
			);
			if (!hasAgent) {
				throw new Error(
					`Expected running ${opts.processMarker} process, got: ${JSON.stringify(running.map((p) => ({ cmd: p.command, args: p.args })))}`,
				);
			}
		},
		settleMs: 2000,
	};
}

export const WORKLOADS: Record<string, Workload> = {
	sleep: {
		name: "sleep",
		description: "Minimal VM with idle Node.js process (setTimeout keepalive)",
		createVm: () => AgentOs.create(),
		start: (vm) => {
			vm.spawn("node", ["-e", "setTimeout(() => {}, 999999999)"], {
				streamStdin: true,
			});
		},
		verify: (vm) => {
			const procs = vm.listProcesses();
			const running = procs.filter((p) => p.running);
			const hasNode = running.some((p) => p.command === "node");
			if (!hasNode) {
				throw new Error(
					`Expected running 'node' process, got: ${JSON.stringify(running.map((p) => p.command))}`,
				);
			}
		},
		settleMs: 2000,
	},
	"pi-session": makeAgentSessionWorkload({
		agentId: "pi",
		description: "VM with PI agent session via createSession",
		software: [pi],
		processMarker: "agent-os-pi",
	}),
	"pi-prompt-turn": makeAgentPromptWorkload({
		agentId: "pi-cli",
		description:
			'Native PI CLI headless benchmark path via createSession("pi-cli"), which drives the real PI CLI through pi-acp RPC mode and records a full prompt turn.',
		software: [],
		processMarker: "pi-acp",
		prompt: PI_BENCHMARK_PROMPT,
	}),
	"claude-session": makeAgentSessionWorkload({
		agentId: "claude",
		description: "VM with Claude agent session via createSession",
		software: [claude],
		processMarker: "agent-os-claude",
	}),
	"codex-session": makeAgentSessionWorkload({
		agentId: "codex",
		description: "VM with Codex agent session via createSession",
		software: [...codex],
		processMarker: "agent-os-codex-agent",
	}),
};

// ── VM creation helpers ─────────────────────────────────────────────

/**
 * Create a fresh AgentOS VM with only coreutils (WASM shell + echo).
 * This is the minimal setup needed to run shell commands.
 */
export async function createBenchVm(): Promise<AgentOs> {
	return AgentOs.create({
		software: [coreutils],
	});
}

/**
 * Create a fresh AgentOS VM with agent software and a ready session.
 * Measures cold start from AgentOs.create() through createSession() completing
 * (ACP handshake done, agent ready to accept prompts).
 */
export async function createAgentSessionVm(
	agentId: string,
	software: SoftwareInput[],
): Promise<{
	vm: AgentOs;
	coldStartMs: number;
}> {
	const { url, port } = await ensureLlmock();
	const t0 = performance.now();
	const vm = await AgentOs.create({
		software,
		loopbackExemptPorts: [port],
	});
	await vm.createSession(agentId, {
		env: {
			ANTHROPIC_API_KEY: "bench-key",
			ANTHROPIC_BASE_URL: url,
		},
	});
	const coldStartMs = performance.now() - t0;
	return { vm, coldStartMs };
}

/** Convenience alias for PI agent session. */
export function createPiSessionVm() {
	return createAgentSessionVm("pi", [pi]);
}

// ── Stats and formatting ────────────────────────────────────────────

export function percentile(sorted: number[], p: number): number {
	const idx = Math.ceil((p / 100) * sorted.length) - 1;
	return sorted[Math.max(0, idx)];
}

export function stats(samples: number[]) {
	const sorted = [...samples].sort((a, b) => a - b);
	const mean = samples.reduce((a, b) => a + b, 0) / samples.length;
	return {
		mean: round(mean),
		p50: round(percentile(sorted, 50)),
		p95: round(percentile(sorted, 95)),
		p99: round(percentile(sorted, 99)),
		min: round(sorted[0]),
		max: round(sorted[sorted.length - 1]),
	};
}

export function round(n: number, decimals = 2): number {
	const f = 10 ** decimals;
	return Math.round(n * f) / f;
}

export function getHardware() {
	const cpus = os.cpus();
	return {
		cpu: cpus[0]?.model ?? "unknown",
		cores: os.availableParallelism(),
		ram: `${round(os.totalmem() / 1024 ** 3, 1)} GB`,
		node: process.version,
		os: `${os.type()} ${os.release()}`,
		arch: os.arch(),
	};
}

export function forceGC() {
	if (global.gc) {
		global.gc();
	} else {
		console.error("WARNING: global.gc not available. Run with --expose-gc");
	}
}

export async function sleep(ms: number): Promise<void> {
	return new Promise((r) => setTimeout(r, ms));
}

export function formatBytes(bytes: number): string {
	if (Math.abs(bytes) < 1024) return `${bytes} B`;
	const mb = bytes / (1024 * 1024);
	return `${round(mb, 2)} MB`;
}

/** Print a table to stderr for human readability. */
export function printTable(
	headers: string[],
	rows: (string | number)[][],
): void {
	const widths = headers.map((h, i) =>
		Math.max(h.length, ...rows.map((r) => String(r[i]).length)),
	);
	const sep = widths.map((w) => "-".repeat(w)).join(" | ");
	const fmt = (row: (string | number)[]) =>
		row.map((c, i) => String(c).padStart(widths[i])).join(" | ");

	console.error("");
	console.error(fmt(headers));
	console.error(sep);
	for (const row of rows) {
		console.error(fmt(row));
	}
	console.error("");
}
