/**
 * Shared utilities for the AgentOS language execution cold-start, warm-start, and memory
 * benchmarks.
 */

import { readFileSync } from "node:fs";
import os from "node:os";
import {
	NodeRuntime,
	SidecarProcess,
	type NodeRuntimeBootTiming,
	type NodeRuntimeCreateOptions,
} from "@rivet-dev/agentos-runtime-core";
import { createInMemoryFileSystem } from "@rivet-dev/agentos-runtime-core/test-runtime";

function numList(envVar: string, fallback: number[]): number[] {
	const raw = process.env[envVar];
	if (!raw) return fallback;
	return raw
		.split(",")
		.map((s) => Number(s.trim()))
		.filter((n) => Number.isFinite(n) && n > 0);
}

export function num(envVar: string, fallback: number): number {
	const raw = process.env[envVar];
	if (raw === undefined) return fallback;
	const n = Number(raw);
	return Number.isFinite(n) && n >= 0 ? n : fallback;
}

export const BATCH_SIZES = numList("BENCH_BATCH_SIZES", [1, 10, 50, 100, 200]);
export const ITERATIONS = num("BENCH_ITERATIONS", 5);
export const WARMUP_ITERATIONS = num("BENCH_WARMUP", 1);
export const MEMORY_ITERATIONS = num("BENCH_MEMORY_ITERATIONS", 5);

export const TRIVIAL_CODE = "export const x = 1;";
export const RESIDENT_TRIVIAL_CODE =
	"globalThis.__benchValue = (globalThis.__benchValue ?? 0) + 1;";

export const MAX_CONCURRENCY = Math.max(1, os.availableParallelism() - 4);
export const MAX_LIVE_RUNTIMES = Math.max(
	1,
	num("BENCH_MAX_LIVE_RUNTIMES", Math.min(8, MAX_CONCURRENCY)),
);
export const MAX_RESIDENT_RUNNERS = Math.max(
	1,
	num("BENCH_MAX_RESIDENT_RUNNERS", 1),
);
export const EXEC_TIMEOUT_MS = Math.max(
	1,
	num("BENCH_EXEC_TIMEOUT_MS", 30_000),
);

export type BenchScenario =
	| "owned-sidecar"
	| "shared-sidecar"
	| "resident-runner";

export const SCENARIOS: BenchScenario[] = (
	process.env.BENCH_SCENARIOS?.split(",").map((s) => s.trim()) ?? [
		"owned-sidecar",
		"shared-sidecar",
		"resident-runner",
	]
).filter((s): s is BenchScenario =>
	["owned-sidecar", "shared-sidecar", "resident-runner"].includes(s),
);

export async function createBenchRuntime(
	options: Pick<NodeRuntimeCreateOptions, "sidecar" | "onBootTiming"> = {},
): Promise<NodeRuntime> {
	return NodeRuntime.create({
		...options,
		filesystem: createInMemoryFileSystem(),
	});
}

export function createBenchSidecar(): SidecarProcess {
	return SidecarProcess.spawn();
}

export function percentile(sorted: number[], p: number): number {
	if (sorted.length === 0) return Number.NaN;
	const idx = Math.ceil((p / 100) * sorted.length) - 1;
	return sorted[Math.max(0, idx)];
}

export function stats(samples: number[]) {
	const sorted = [...samples].sort((a, b) => a - b);
	const mean = samples.reduce((a, b) => a + b, 0) / samples.length;
	return {
		samples: samples.length,
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

export function formatBytes(bytes: number): string {
	if (Math.abs(bytes) < 1024) return `${bytes} B`;
	const mb = bytes / (1024 * 1024);
	return `${round(mb, 2)} MB`;
}

function readMemInfo(): Record<string, string> {
	try {
		const entries = readFileSync("/proc/meminfo", "utf8")
			.trim()
			.split("\n")
			.map((line) => {
				const [key, value] = line.split(":");
				return [key, value.trim()] as const;
			});
		return Object.fromEntries(entries);
	} catch {
		return {};
	}
}

export function getHardware() {
	const cpus = os.cpus();
	const memInfo = readMemInfo();
	return {
		cpu: cpus[0]?.model ?? "unknown",
		cores: os.availableParallelism(),
		ram: `${round(os.totalmem() / 1024 ** 3, 1)} GB`,
		memAvailable: memInfo.MemAvailable,
		swapTotal: memInfo.SwapTotal,
		swapFree: memInfo.SwapFree,
		swapCached: memInfo.SwapCached,
		node: process.version,
		os: `${os.type()} ${os.release()}`,
		arch: os.arch(),
		loadAverage: os.loadavg().map((n) => round(n, 2)),
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

export type PhaseSamples = Record<string, number[]>;

export function createBootTimingRecorder(phases: PhaseSamples) {
	return (timing: NodeRuntimeBootTiming) => {
		(phases[timing.phase] ??= []).push(timing.durationMs);
	};
}

export function mergePhaseSamples(target: PhaseSamples, source: PhaseSamples) {
	for (const [phase, samples] of Object.entries(source)) {
		(target[phase] ??= []).push(...samples);
	}
}

export function summarizePhases(phases: PhaseSamples) {
	return Object.fromEntries(
		Object.entries(phases).map(([phase, samples]) => [phase, stats(samples)]),
	);
}

export async function runLimited<T>(
	count: number,
	concurrency: number,
	fn: (index: number) => Promise<T>,
): Promise<T[]> {
	const results: T[] = new Array(count);
	let next = 0;
	const workers = Array.from(
		{ length: Math.min(count, Math.max(1, concurrency)) },
		async () => {
			for (;;) {
				const index = next++;
				if (index >= count) return;
				results[index] = await fn(index);
			}
		},
	);
	await Promise.all(workers);
	return results;
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
