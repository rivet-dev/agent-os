/**
 * VM startup + serial `ls` benchmark.
 *
 * This is the standard-suite version of the investigation from Codex thread
 * 019f05eb-fc28-7e90-9620-7b5a2150cb9a: create a VM with coreutils, create a
 * directory, run `ls` repeatedly in serial, and compare each `ls` call against
 * the same host command.
 */

import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createBenchSidecar, createBenchVm, type BenchVm } from "../lib/vm.js";
import type { SidecarProcess } from "@rivet-dev/agentos-core";
import { getHardware, printTable, round, stats } from "../lib/perf-utils.js";

interface LsIteration {
	vmCreateMs: number;
	vmSetupMs: number;
	vmNoopExecSamplesMs: number[];
	vmStdoutExecSamplesMs: number[];
	vmLsSamplesMs: number[];
	wasmWarmupDiagnostics?: string[];
	endToEndMs: number;
}

interface LsCaseResult {
	fileCount: number;
	iterations: number;
	serialRuns: number;
	hostLs: ReturnType<typeof stats>;
	vmCreate: ReturnType<typeof stats>;
	vmSetup: ReturnType<typeof stats>;
	vmNoopExec: ReturnType<typeof stats>;
	vmStdoutExec: ReturnType<typeof stats>;
	vmLs: ReturnType<typeof stats>;
	endToEnd: ReturnType<typeof stats>;
	vmVsHostLsRatio: number;
	vmLsMinusNoopMs: number;
	vmLsMinusStdoutMs: number;
	lsDeltaFromEmptyMs?: number;
	raw: LsIteration[];
}

function parseArgs(): {
	iterations: number;
	warmup: number;
	serialRuns: number;
	fileCounts: number[];
	wasmWarmupDebug: boolean;
} {
	const value = (name: string) =>
		process.argv.find((arg) => arg.startsWith(`--${name}=`))?.split("=")[1];
	const hasFlag = (name: string) => process.argv.includes(`--${name}`);
	const iterations = Number(value("iterations") ?? 5);
	const warmup = Number(value("warmup") ?? 1);
	const serialRuns = Number(value("serial-runs") ?? 5);
	const fileCounts = (value("file-counts") ?? "0,100")
		.split(",")
		.map((n) => Number(n.trim()))
		.filter((n) => Number.isFinite(n) && n >= 0);
	const wasmWarmupDebug = hasFlag("wasm-warmup-debug");
	if (iterations < 1 || warmup < 0 || serialRuns < 1 || fileCounts.length === 0) {
		throw new Error(
			"invalid args; expected --iterations>=1 --warmup>=0 --serial-runs>=1 --file-counts=0,100",
		);
	}
	return { iterations, warmup, serialRuns, fileCounts, wasmWarmupDebug };
}

function nowMs(start: number): number {
	return performance.now() - start;
}

function createHostFixture(fileCount: number): string {
	const dir = mkdtempSync(join(tmpdir(), `agentos-ls-bench-${fileCount}-`));
	for (let i = 0; i < fileCount; i++) {
		writeFileSync(join(dir, `file-${String(i).padStart(4, "0")}.txt`), "x");
	}
	return dir;
}

function runHostLs(dir: string, count: number): number[] {
	const samples: number[] = [];
	for (let i = 0; i < count; i++) {
		const start = performance.now();
		execFileSync("ls", [dir], { stdio: "ignore" });
		samples.push(nowMs(start));
	}
	return samples;
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

async function createLsVm(sidecar: SidecarProcess): Promise<BenchVm> {
	return createBenchVm({
		sidecar,
	});
}

async function setupVmFixture(vm: BenchVm, dir: string, fileCount: number): Promise<void> {
	await vm.exec(`rm -rf ${dir} && mkdir -p ${dir}`);
	for (let i = 0; i < fileCount; i++) {
		await vm.writeFile(`${dir}/file-${String(i).padStart(4, "0")}.txt`, "x");
	}
}

async function runCase(
	fileCount: number,
	iterations: number,
	warmup: number,
	serialRuns: number,
	sidecar: SidecarProcess,
	wasmWarmupDebug: boolean,
): Promise<LsCaseResult> {
	const hostDir = createHostFixture(fileCount);
	try {
		const hostLsSamples = runHostLs(hostDir, serialRuns * iterations);
		const raw: LsIteration[] = [];
		const vmCreateSamples: number[] = [];
		const vmSetupSamples: number[] = [];
		const vmNoopExecSamples: number[] = [];
		const vmStdoutExecSamples: number[] = [];
		const vmLsSamples: number[] = [];
		const endToEndSamples: number[] = [];
		const vmDir = `/tmp/ls-bench-${fileCount}`;
		const execOptions = wasmWarmupDebug
			? { env: { AGENTOS_WASM_WARMUP_DEBUG: "1" } }
			: undefined;

		for (let i = 0; i < warmup + iterations; i++) {
			const endToEndStart = performance.now();
			const createStart = performance.now();
			const vm = await createLsVm(sidecar);
			const vmCreateMs = nowMs(createStart);
			try {
				const setupStart = performance.now();
				await setupVmFixture(vm, vmDir, fileCount);
				const vmSetupMs = nowMs(setupStart);
				const wasmWarmupDiagnostics: string[] = [];
				const iterNoopSamples: number[] = [];
				for (let j = 0; j < serialRuns; j++) {
					const noopStart = performance.now();
					const result = await vm.exec("true", execOptions);
					const noopMs = nowMs(noopStart);
					if (result.exitCode !== 0) {
						throw new Error(`true exited ${result.exitCode}: ${result.stderr}`);
					}
					collectWasmWarmupDiagnostics(
						wasmWarmupDebug,
						result.stderr,
						wasmWarmupDiagnostics,
					);
					iterNoopSamples.push(noopMs);
				}
				const iterStdoutSamples: number[] = [];
				for (let j = 0; j < serialRuns; j++) {
					const stdoutStart = performance.now();
					const result = await vm.exec("printf x", execOptions);
					const stdoutMs = nowMs(stdoutStart);
					if (result.exitCode !== 0 || result.stdout !== "x") {
						throw new Error(
							`printf exited ${result.exitCode}: stdout=${JSON.stringify(result.stdout)} stderr=${result.stderr}`,
						);
					}
					collectWasmWarmupDiagnostics(
						wasmWarmupDebug,
						result.stderr,
						wasmWarmupDiagnostics,
					);
					iterStdoutSamples.push(stdoutMs);
				}
				const iterLsSamples: number[] = [];
				for (let j = 0; j < serialRuns; j++) {
					const lsStart = performance.now();
					const result = await vm.exec(`ls ${vmDir}`, execOptions);
					const lsMs = nowMs(lsStart);
					if (result.exitCode !== 0) {
						throw new Error(`ls exited ${result.exitCode}: ${result.stderr}`);
					}
					collectWasmWarmupDiagnostics(
						wasmWarmupDebug,
						result.stderr,
						wasmWarmupDiagnostics,
					);
					iterLsSamples.push(lsMs);
				}
				const endToEndMs = nowMs(endToEndStart);
				console.error(
					`  files=${fileCount} iter=${i}: create=${round(vmCreateMs)}ms setup=${round(vmSetupMs)}ms noop.p50=${round(stats(iterNoopSamples).p50)}ms stdout.p50=${round(stats(iterStdoutSamples).p50)}ms ls.p50=${round(stats(iterLsSamples).p50)}ms end=${round(endToEndMs)}ms${i < warmup ? " (warmup)" : ""}`,
				);
				if (i >= warmup) {
					raw.push({
						vmCreateMs,
						vmSetupMs,
						vmNoopExecSamplesMs: iterNoopSamples,
						vmStdoutExecSamplesMs: iterStdoutSamples,
						vmLsSamplesMs: iterLsSamples,
						...(wasmWarmupDebug ? { wasmWarmupDiagnostics } : {}),
						endToEndMs,
					});
					vmCreateSamples.push(vmCreateMs);
					vmSetupSamples.push(vmSetupMs);
					vmNoopExecSamples.push(...iterNoopSamples);
					vmStdoutExecSamples.push(...iterStdoutSamples);
					vmLsSamples.push(...iterLsSamples);
					endToEndSamples.push(endToEndMs);
				}
			} finally {
				await vm.dispose();
			}
		}

		const hostLs = stats(hostLsSamples);
		const vmLs = stats(vmLsSamples);
		const vmNoopExec = stats(vmNoopExecSamples);
		const vmStdoutExec = stats(vmStdoutExecSamples);
		return {
			fileCount,
			iterations,
			serialRuns,
			hostLs,
			vmCreate: stats(vmCreateSamples),
			vmSetup: stats(vmSetupSamples),
			vmNoopExec,
			vmStdoutExec,
			vmLs,
			endToEnd: stats(endToEndSamples),
			vmVsHostLsRatio: round(vmLs.p50 / hostLs.p50),
			vmLsMinusNoopMs: round(vmLs.p50 - vmNoopExec.p50),
			vmLsMinusStdoutMs: round(vmLs.p50 - vmStdoutExec.p50),
			raw,
		};
	} finally {
		rmSync(hostDir, { recursive: true, force: true });
	}
}

async function main(): Promise<void> {
	const { iterations, warmup, serialRuns, fileCounts, wasmWarmupDebug } =
		parseArgs();
	const hardware = getHardware();
	console.error("=== VM `ls` Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(
		`Iterations: ${iterations} (+ ${warmup} warmup), serial ls runs: ${serialRuns}, file counts: ${fileCounts.join(",")}`,
	);
	if (wasmWarmupDebug) {
		console.error("WASM warmup debug: enabled");
	}

	const sidecar = await createBenchSidecar();
	try {
		const cases: LsCaseResult[] = [];
		for (const fileCount of fileCounts) {
			cases.push(
				await runCase(
					fileCount,
					iterations,
					warmup,
					serialRuns,
					sidecar,
					wasmWarmupDebug,
				),
			);
		}
		const emptyCase = cases.find((result) => result.fileCount === 0);
		if (emptyCase) {
			for (const result of cases) {
				result.lsDeltaFromEmptyMs = round(result.vmLs.p50 - emptyCase.vmLs.p50);
			}
		}
		printTable(
			[
				"files",
				"host ls p50",
				"vm create p50",
				"vm noop p50",
				"vm stdout p50",
				"vm ls p50",
				"ls-noop",
				"ls-stdout",
				"ls-empty delta",
				"vm/host ls",
				"end-to-end p50",
			],
			cases.map((result) => [
				result.fileCount,
				`${result.hostLs.p50}ms`,
				`${result.vmCreate.p50}ms`,
				`${result.vmNoopExec.p50}ms`,
				`${result.vmStdoutExec.p50}ms`,
				`${result.vmLs.p50}ms`,
				`${result.vmLsMinusNoopMs}ms`,
				`${result.vmLsMinusStdoutMs}ms`,
				result.lsDeltaFromEmptyMs === undefined ? "n/a" : `${result.lsDeltaFromEmptyMs}ms`,
				`${result.vmVsHostLsRatio}x`,
				`${result.endToEnd.p50}ms`,
			]),
		);
		console.log(
			JSON.stringify(
				{
					benchmark: "vm-ls-serial",
					hardware,
					iterations,
					warmup,
					serialRuns,
					wasmWarmupDebug,
					cases,
				},
				null,
				2,
			),
		);
	} finally {
		await sidecar.dispose();
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
