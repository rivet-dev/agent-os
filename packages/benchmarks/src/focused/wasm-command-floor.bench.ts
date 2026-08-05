/**
 * WASM command floor benchmark.
 *
 * Measures direct execArgv command startup/capture cost across a module-size
 * ladder and a same-module stdout-size sweep. This keeps the remaining
 * process-floor investigation separate from shell and directory setup costs.
 */

import { existsSync, statSync } from "node:fs";
import { join } from "node:path";
import { createBenchSidecar, createBenchVm, type BenchVm } from "../lib/vm.js";
import type { SidecarProcess } from "@rivet-dev/agentos-core";
import { getHardware, printTable, round, stats } from "../lib/perf-utils.js";

interface CommandCase {
	name: string;
	command: string;
	args: (fixture: { emptyDir: string }) => string[];
	stdoutSize?: number;
	expect?: (result: { exitCode: number; stdout: string; stderr: string }) => void;
}

interface CommandCaseResult {
	name: string;
	command: string;
	args: string[];
	moduleBytes: number | null;
	iterations: number;
	serialRuns: number;
	firstRun: ReturnType<typeof stats>;
	warmRuns: ReturnType<typeof stats>;
	allRuns: ReturnType<typeof stats>;
	stdoutBytes: ReturnType<typeof stats>;
	stderrBytes: ReturnType<typeof stats>;
	stdoutCallbackChunks: ReturnType<typeof stats>;
	stderrCallbackChunks: ReturnType<typeof stats>;
	stdoutCallbackBytes: ReturnType<typeof stats>;
	stderrCallbackBytes: ReturnType<typeof stats>;
	raw: {
		samplesMs: number[];
		stdoutBytes: number[];
		stderrBytes: number[];
		stdoutCallbackChunks: number[];
		stderrCallbackChunks: number[];
		stdoutCallbackBytes: number[];
		stderrCallbackBytes: number[];
		wasmWarmupDiagnostics?: string[];
	}[];
}

function parseArgs(): {
	iterations: number;
	warmup: number;
	serialRuns: number;
	stdoutSizes: number[];
	wasmWarmupDebug: boolean;
} {
	const value = (name: string) =>
		process.argv.find((arg) => arg.startsWith(`--${name}=`))?.split("=")[1];
	const hasFlag = (name: string) => process.argv.includes(`--${name}`);
	const iterations = Number(value("iterations") ?? 3);
	const warmup = Number(value("warmup") ?? 1);
	const serialRuns = Number(value("serial-runs") ?? 3);
	const stdoutSizes = (value("stdout-sizes") ?? "0,1,65536")
		.split(",")
		.map((n) => Number(n.trim()))
		.filter((n) => Number.isFinite(n) && n >= 0)
		.map((n) => Math.trunc(n));
	if (iterations < 1 || warmup < 0 || serialRuns < 1 || stdoutSizes.length === 0) {
		throw new Error(
			"invalid args; expected --iterations>=1 --warmup>=0 --serial-runs>=1 --stdout-sizes=0,1,65536",
		);
	}
	return {
		iterations,
		warmup,
		serialRuns,
		stdoutSizes,
		wasmWarmupDebug: hasFlag("wasm-warmup-debug"),
	};
}

function nowMs(start: number): number {
	return performance.now() - start;
}

function resolveAgentosRoot(): string | null {
	const candidates = [
		process.env.AGENTOS_ROOT,
		process.cwd(),
		join(process.cwd(), "../agentos"),
	].filter((path): path is string => Boolean(path));

	for (const candidate of candidates) {
		if (existsSync(join(candidate, "software/coreutils/wasm"))) {
			return candidate;
		}
	}
	return null;
}

function commandModuleBytes(command: string, agentosRoot: string | null): number | null {
	if (!agentosRoot) {
		return null;
	}
	const modulePath = join(agentosRoot, "software/coreutils/wasm", command);
	return existsSync(modulePath) ? statSync(modulePath).size : null;
}

function collectWasmWarmupDiagnostics(
	enabled: boolean,
	stderr: string,
	diagnostics: string[],
): void {
	if (!enabled) {
		return;
	}
	for (const line of stderr.split(/\r?\n/)) {
		if (
			line.startsWith("__AGENTOS_WASM_WARMUP_METRICS__:") ||
			line.startsWith("__AGENTOS_WASM_PHASE_METRICS__:")
		) {
			diagnostics.push(line);
		}
	}
}

async function createVm(sidecar: SidecarProcess): Promise<BenchVm> {
	return createBenchVm({
		sidecar,
	});
}

function stdoutLabel(bytes: number): string {
	if (bytes === 0) {
		return "0b";
	}
	if (bytes % (1024 * 1024) === 0) {
		return `${bytes / (1024 * 1024)}m`;
	}
	if (bytes % 1024 === 0) {
		return `${bytes / 1024}k`;
	}
	return `${bytes}b`;
}

function makeStdoutCases(stdoutSizes: number[]): CommandCase[] {
	return stdoutSizes.map((stdoutSize) => {
		const payload = "x".repeat(stdoutSize);
		return {
			name: `printf-${stdoutLabel(stdoutSize)}`,
			command: "printf",
			args: () => [payload],
			stdoutSize,
			expect: (result) => {
				if (result.exitCode !== 0 || result.stdout.length !== stdoutSize) {
					throw new Error(
						`printf-${stdoutLabel(stdoutSize)} mismatch: stdout=${result.stdout.length}`,
					);
				}
			},
		};
	});
}

function commandCases(stdoutSizes: number[]): CommandCase[] {
	return [
	{
		name: "true",
		command: "true",
		args: () => [],
		expect: (result) => {
			if (result.exitCode !== 0 || result.stdout !== "") {
				throw new Error(`true mismatch: ${JSON.stringify(result)}`);
			}
		},
	},
	...makeStdoutCases(stdoutSizes),
	{
		name: "pwd",
		command: "pwd",
		args: () => [],
		expect: (result) => {
			if (result.exitCode !== 0 || result.stdout.trim().length === 0) {
				throw new Error(`pwd mismatch: ${JSON.stringify(result)}`);
			}
		},
	},
	{
		name: "ls-empty",
		command: "ls",
		args: ({ emptyDir }) => [emptyDir],
		expect: (result) => {
			if (result.exitCode !== 0 || result.stdout !== "") {
				throw new Error(`ls-empty mismatch: ${JSON.stringify(result)}`);
			}
		},
	},
	{
		name: "date-version",
		command: "date",
		args: () => ["--version"],
		expect: (result) => {
			if (result.exitCode !== 0 || !result.stdout.includes("date")) {
				throw new Error(`date-version mismatch: ${JSON.stringify(result)}`);
			}
		},
	},
	];
}

async function runCase(
	vm: BenchVm,
	testCase: CommandCase,
	iterations: number,
	warmup: number,
	serialRuns: number,
	wasmWarmupDebug: boolean,
	agentosRoot: string | null,
): Promise<CommandCaseResult> {
	const emptyDir = "/tmp/wasm-command-floor-empty";
	const resolvedArgs = testCase.args({ emptyDir });
	const raw: CommandCaseResult["raw"] = [];
	const firstRunSamples: number[] = [];
	const warmRunSamples: number[] = [];
	const allRunSamples: number[] = [];
	const stdoutBytesSamples: number[] = [];
	const stderrBytesSamples: number[] = [];
	const stdoutCallbackChunksSamples: number[] = [];
	const stderrCallbackChunksSamples: number[] = [];
	const stdoutCallbackBytesSamples: number[] = [];
	const stderrCallbackBytesSamples: number[] = [];
	const execOptions = wasmWarmupDebug
		? { env: { AGENTOS_WASM_WARMUP_DEBUG: "1" } }
		: undefined;

	for (let i = 0; i < warmup + iterations; i++) {
		const samplesMs: number[] = [];
		const stdoutBytes: number[] = [];
		const stderrBytes: number[] = [];
		const stdoutCallbackChunks: number[] = [];
		const stderrCallbackChunks: number[] = [];
		const stdoutCallbackBytes: number[] = [];
		const stderrCallbackBytes: number[] = [];
		const wasmWarmupDiagnostics: string[] = [];
		for (let j = 0; j < serialRuns; j++) {
			let runStdoutCallbackChunks = 0;
			let runStderrCallbackChunks = 0;
			let runStdoutCallbackBytes = 0;
			let runStderrCallbackBytes = 0;
			const start = performance.now();
			const result = await vm.execArgv(testCase.command, resolvedArgs, {
				...(execOptions ?? {}),
				onStdout: (chunk) => {
					runStdoutCallbackChunks += 1;
					runStdoutCallbackBytes += chunk.byteLength;
				},
				onStderr: (chunk) => {
					runStderrCallbackChunks += 1;
					runStderrCallbackBytes += chunk.byteLength;
				},
			});
			const elapsed = nowMs(start);
			testCase.expect?.(result);
			collectWasmWarmupDiagnostics(
				wasmWarmupDebug,
				result.stderr,
				wasmWarmupDiagnostics,
			);
			samplesMs.push(elapsed);
			stdoutBytes.push(Buffer.byteLength(result.stdout));
			stderrBytes.push(Buffer.byteLength(result.stderr));
			stdoutCallbackChunks.push(runStdoutCallbackChunks);
			stderrCallbackChunks.push(runStderrCallbackChunks);
			stdoutCallbackBytes.push(runStdoutCallbackBytes);
			stderrCallbackBytes.push(runStderrCallbackBytes);
		}
		console.error(
			`  ${testCase.name} iter=${i}: first=${round(samplesMs[0] ?? 0)}ms warm.p50=${round(stats(samplesMs.slice(1)).p50)}ms all.p50=${round(stats(samplesMs).p50)}ms${i < warmup ? " (warmup)" : ""}`,
		);
		if (i >= warmup) {
			raw.push({
				samplesMs,
				stdoutBytes,
				stderrBytes,
				stdoutCallbackChunks,
				stderrCallbackChunks,
				stdoutCallbackBytes,
				stderrCallbackBytes,
				...(wasmWarmupDebug ? { wasmWarmupDiagnostics } : {}),
			});
			firstRunSamples.push(samplesMs[0]);
			warmRunSamples.push(...samplesMs.slice(1));
			allRunSamples.push(...samplesMs);
			stdoutBytesSamples.push(...stdoutBytes);
			stderrBytesSamples.push(...stderrBytes);
			stdoutCallbackChunksSamples.push(...stdoutCallbackChunks);
			stderrCallbackChunksSamples.push(...stderrCallbackChunks);
			stdoutCallbackBytesSamples.push(...stdoutCallbackBytes);
			stderrCallbackBytesSamples.push(...stderrCallbackBytes);
		}
	}

	return {
		name: testCase.name,
		command: testCase.command,
		args: resolvedArgs,
		moduleBytes: commandModuleBytes(testCase.command, agentosRoot),
		iterations,
		serialRuns,
		firstRun: stats(firstRunSamples),
		warmRuns: stats(warmRunSamples),
		allRuns: stats(allRunSamples),
		stdoutBytes: stats(stdoutBytesSamples),
		stderrBytes: stats(stderrBytesSamples),
		stdoutCallbackChunks: stats(stdoutCallbackChunksSamples),
		stderrCallbackChunks: stats(stderrCallbackChunksSamples),
		stdoutCallbackBytes: stats(stdoutCallbackBytesSamples),
		stderrCallbackBytes: stats(stderrCallbackBytesSamples),
		raw,
	};
}

async function main(): Promise<void> {
	const { iterations, warmup, serialRuns, stdoutSizes, wasmWarmupDebug } = parseArgs();
	const hardware = getHardware();
	const agentosRoot = resolveAgentosRoot();
	console.error("=== WASM Command Floor Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(
		`Iterations: ${iterations} (+ ${warmup} warmup), serial runs per command: ${serialRuns}`,
	);
	console.error(`stdout sizes: ${stdoutSizes.join(",")} bytes`);
	if (wasmWarmupDebug) {
		console.error("WASM warmup debug: enabled");
	}

	const sidecar = await createBenchSidecar();
	try {
		const vm = await createVm(sidecar);
		try {
			await vm.exec("rm -rf /tmp/wasm-command-floor-empty && mkdir -p /tmp/wasm-command-floor-empty");
			const cases: CommandCaseResult[] = [];
			for (const testCase of commandCases(stdoutSizes)) {
				cases.push(
					await runCase(
						vm,
						testCase,
						iterations,
						warmup,
						serialRuns,
						wasmWarmupDebug,
						agentosRoot,
					),
				);
			}
			printTable(
				[
					"case",
					"module bytes",
					"first p50",
					"warm p50",
					"all p50",
					"stdout p50",
					"stdout chunks",
					"stderr p50",
				],
				cases.map((result) => [
					result.name,
					result.moduleBytes ?? "n/a",
					`${result.firstRun.p50}ms`,
					`${result.warmRuns.p50}ms`,
					`${result.allRuns.p50}ms`,
					`${result.stdoutBytes.p50}B`,
					result.stdoutCallbackChunks.p50,
					`${result.stderrBytes.p50}B`,
				]),
			);
			console.log(
				JSON.stringify(
					{
						benchmark: "wasm-command-floor",
						hardware,
						iterations,
						warmup,
						serialRuns,
						stdoutSizes,
						wasmWarmupDebug,
						agentosRoot,
						cases,
					},
					null,
					2,
				),
			);
		} finally {
			await vm.dispose();
		}
	} finally {
		await sidecar.dispose();
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
