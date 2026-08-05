/**
 * Focused WASI/coreutils `ls` scaling benchmark.
 *
 * This prepares directory fixtures outside the timed loop and reuses one VM, so
 * the samples isolate coreutils/WASI `ls -1` cost from VM creation and fixture setup.
 */

import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { existsSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createBenchSidecar, createBenchVm, type BenchVm } from "../lib/vm.js";
import type { SidecarProcess } from "@rivet-dev/agentos-core";
import { getHardware, printTable, round, stats } from "../lib/perf-utils.js";

interface WasiSyscallMetric {
	name: string;
	elapsedMs: number;
	result?: unknown;
	fd?: number;
	iovsLen?: number;
	cookie?: number;
	bufLen?: number;
	used?: number;
	recordsReturned?: number;
	totalDirentsRead?: number;
	stoppedRecordTooLarge?: boolean;
}

interface WasiSyscallSummary {
	count: number;
	totalMs: number;
	p50Ms: number;
	maxMs: number;
}

interface WasiLsCaseResult {
	variant: string;
	lsArgs: string[];
	fileCount: number;
	iterations: number;
	warmup: number;
	serialRuns: number;
	hostLs: ReturnType<typeof stats>;
	guestTrue: ReturnType<typeof stats>;
	guestLs: ReturnType<typeof stats>;
	guestLsMinusTrueMs: number;
	guestDeltaFromEmptyMs?: number;
	hostDeltaFromEmptyMs?: number;
	guestVsHostRatio: number;
	stdoutBytes: ReturnType<typeof stats>;
	stdoutCallbackChunks: ReturnType<typeof stats>;
	stdoutCallbackBytes: ReturnType<typeof stats>;
	wasiSyscalls?: Record<string, WasiSyscallSummary>;
	fdReaddir?: {
		count: number;
		recordsReturned: number;
		totalDirentsReadMax: number;
		shortBufferStops: number;
		cookies: number[];
	};
	returnedCount: {
		host: number;
		guestFixture: number;
		guestStdout: number;
	};
	raw: {
		hostLsMs: number[];
		guestTrueMs: number[];
		guestLsMs: number[];
		stdoutBytes: number[];
		stdoutCallbackChunks: number[];
		stdoutCallbackBytes: number[];
		wasmWarmupDiagnostics?: string[];
		wasiSyscallMetrics?: WasiSyscallMetric[];
	};
}

interface LsVariant {
	name: string;
	argsBeforeDir: string[];
}

const LS_VARIANTS: Record<string, LsVariant> = {
	one: { name: "one", argsBeforeDir: ["-1"] },
	unsorted: { name: "unsorted", argsBeforeDir: ["-1", "-U"] },
	"no-color": { name: "no-color", argsBeforeDir: ["-1", "--color=never"] },
	"unsorted-no-color": {
		name: "unsorted-no-color",
		argsBeforeDir: ["-1", "-U", "--color=never"],
	},
	"fast-no-decor": {
		name: "fast-no-decor",
		argsBeforeDir: ["-f", "-1", "--color=never", "--indicator-style=none"],
	},
};

function parseArgs(): {
	iterations: number;
	warmup: number;
	serialRuns: number;
	fileCounts: number[];
	lsVariants: LsVariant[];
	wasmWarmupDebug: boolean;
	wasiSyscallCounters: boolean;
} {
	const value = (name: string) =>
		process.argv.find((arg) => arg.startsWith(`--${name}=`))?.split("=")[1];
	const hasFlag = (name: string) => process.argv.includes(`--${name}`);
	const iterations = Number(value("iterations") ?? 5);
	const warmup = Number(value("warmup") ?? 1);
	const serialRuns = Number(value("serial-runs") ?? 3);
	const fileCounts = (value("file-counts") ?? "0,1,32,100,1000")
		.split(",")
		.map((n) => Number(n.trim()))
		.filter((n) => Number.isFinite(n) && n >= 0)
		.map((n) => Math.trunc(n));
	const lsVariants = (value("ls-variants") ?? "one")
		.split(",")
		.map((variant) => variant.trim())
		.filter((variant) => variant.length > 0)
		.map((variant) => {
			const resolved = LS_VARIANTS[variant];
			if (!resolved) {
				throw new Error(
					`unknown ls variant ${variant}; expected one of ${Object.keys(LS_VARIANTS).join(",")}`,
				);
			}
			return resolved;
		});
	if (iterations < 1 || warmup < 0 || serialRuns < 1 || fileCounts.length === 0) {
		throw new Error(
			"invalid args; expected --iterations>=1 --warmup>=0 --serial-runs>=1 --file-counts=0,1,32,100,1000",
		);
	}
	return {
		iterations,
		warmup,
		serialRuns,
		fileCounts,
		lsVariants,
		wasmWarmupDebug: hasFlag("wasm-warmup-debug"),
		wasiSyscallCounters: hasFlag("wasi-syscall-counters"),
	};
}

function nowMs(start: number): number {
	return performance.now() - start;
}

function createHostFixture(fileCount: number): string {
	const dir = mkdtempSync(join(tmpdir(), `agentos-wasi-ls-bench-${fileCount}-`));
	for (let i = 0; i < fileCount; i++) {
		writeFileSync(join(dir, `file-${String(i).padStart(5, "0")}.txt`), "x");
	}
	return dir;
}

function countGeneratedLsEntries(stdout: string): number {
	const trimmed = stdout.trimEnd();
	if (trimmed.length === 0) return 0;
	const matches = trimmed.match(/\bfile-\d{5}\.txt\b/g);
	return matches?.length ?? 0;
}

function runHostLs(
	dir: string,
	fileCount: number,
	variant: LsVariant,
	samples: number[],
): number {
	let returnedCount = 0;
	const start = performance.now();
	const stdout = execFileSync("ls", [...variant.argsBeforeDir, dir], {
		encoding: "utf8",
	});
	const elapsed = nowMs(start);
	returnedCount = countGeneratedLsEntries(stdout);
	if (returnedCount !== fileCount) {
		throw new Error(
			`host ls ${variant.name} returned ${returnedCount}, expected ${fileCount}`,
		);
	}
	samples.push(elapsed);
	return returnedCount;
}

function collectWasmWarmupDiagnostics(
	enabled: boolean,
	stderr: string,
	diagnostics: string[],
): void {
	if (!enabled) return;
	for (const line of stderr.split(/\r?\n/)) {
		if (
			line.startsWith("__AGENTOS_WASM_WARMUP_METRICS__:") ||
			line.startsWith("__AGENTOS_WASM_PHASE_METRICS__:")
		) {
			diagnostics.push(line);
		}
	}
}

function collectWasiSyscallMetrics(
	enabled: boolean,
	stderr: string,
	metrics: WasiSyscallMetric[],
): void {
	if (!enabled) return;
	for (const line of stderr.split(/\r?\n/)) {
		if (!line.startsWith("__AGENTOS_WASI_SYSCALL_METRICS__:")) continue;
		const raw = line.slice("__AGENTOS_WASI_SYSCALL_METRICS__:".length);
		try {
			const parsed = JSON.parse(raw) as WasiSyscallMetric;
			if (typeof parsed.name === "string" && Number.isFinite(parsed.elapsedMs)) {
				metrics.push(parsed);
			}
		} catch {
			// Keep benchmark collection robust if a debug line is malformed.
		}
	}
}

function summarizeWasiSyscalls(
	metrics: WasiSyscallMetric[],
): Record<string, WasiSyscallSummary> {
	const grouped = new Map<string, number[]>();
	for (const metric of metrics) {
		if (!grouped.has(metric.name)) {
			grouped.set(metric.name, []);
		}
		grouped.get(metric.name)!.push(metric.elapsedMs);
	}
	return Object.fromEntries(
		[...grouped.entries()].map(([name, values]) => {
			const summary = stats(values);
			return [
				name,
				{
					count: values.length,
					totalMs: round(values.reduce((sum, value) => sum + value, 0)),
					p50Ms: summary.p50,
					maxMs: summary.max,
				},
			];
		}),
	);
}

function summarizeFdReaddir(metrics: WasiSyscallMetric[]) {
	const fdReaddir = metrics.filter((metric) => metric.name === "fd_readdir");
	if (fdReaddir.length === 0) return undefined;
	return {
		count: fdReaddir.length,
		recordsReturned: fdReaddir.reduce(
			(sum, metric) => sum + (metric.recordsReturned ?? 0),
			0,
		),
		totalDirentsReadMax: Math.max(
			...fdReaddir.map((metric) => metric.totalDirentsRead ?? 0),
		),
		shortBufferStops: fdReaddir.filter((metric) => metric.stoppedRecordTooLarge)
			.length,
		cookies: fdReaddir.map((metric) => metric.cookie ?? 0),
	};
}

function resolveAgentosRoot(): string | null {
	const candidates = [
		process.env.AGENTOS_ROOT,
		process.cwd(),
		join(process.cwd(), "../agentos"),
	].filter((path): path is string => Boolean(path));
	for (const candidate of candidates) {
		if (existsSync(join(candidate, "software/coreutils/wasm/ls"))) {
			return candidate;
		}
	}
	return null;
}

function lsModuleBytes(agentosRoot: string | null): number | null {
	if (!agentosRoot) return null;
	const modulePath = join(agentosRoot, "software/coreutils/wasm/ls");
	return existsSync(modulePath) ? statSync(modulePath).size : null;
}

async function createVm(sidecar: SidecarProcess): Promise<BenchVm> {
	return createBenchVm({
		sidecar,
	});
}

async function setupGuestFixture(vm: BenchVm, dir: string, fileCount: number): Promise<void> {
	await vm.exec(`rm -rf ${dir} && mkdir -p ${dir}`);
	for (let i = 0; i < fileCount; i++) {
		await vm.writeFile(`${dir}/file-${String(i).padStart(5, "0")}.txt`, "x");
	}
	const entries = await vm.readdir(dir);
	if (entries.length !== fileCount) {
		throw new Error(
			`guest fixture setup created ${entries.length} entries in ${dir}, expected ${fileCount}`,
		);
	}
}

async function runCase(
	vm: BenchVm,
	hostDir: string,
	guestDir: string,
	fileCount: number,
	variant: LsVariant,
	iterations: number,
	warmup: number,
	serialRuns: number,
	wasmWarmupDebug: boolean,
	wasiSyscallCounters: boolean,
): Promise<WasiLsCaseResult> {
	const hostLsMs: number[] = [];
	const guestTrueMs: number[] = [];
	const guestLsMs: number[] = [];
	const stdoutBytes: number[] = [];
	const stdoutCallbackChunks: number[] = [];
	const stdoutCallbackBytes: number[] = [];
	const wasmWarmupDiagnostics: string[] = [];
	const wasiSyscallMetrics: WasiSyscallMetric[] = [];
	let hostReturnedCount = 0;
	const guestFixtureCount = fileCount;
	let guestStdoutReturnedCount = 0;
	const env: Record<string, string> = {};
	if (wasmWarmupDebug) env.AGENTOS_WASM_WARMUP_DEBUG = "1";
	if (wasiSyscallCounters) env.AGENTOS_WASI_SYSCALL_COUNTERS = "1";
	const execOptions = Object.keys(env).length > 0 ? { env } : {};

	for (let i = 0; i < warmup + iterations; i++) {
		for (let j = 0; j < serialRuns; j++) {
			hostReturnedCount = runHostLs(hostDir, fileCount, variant, hostLsMs);

			let start = performance.now();
			const trueResult = await vm.execArgv("true", [], execOptions);
			const trueMs = nowMs(start);
			if (trueResult.exitCode !== 0) {
				throw new Error(`guest true exited ${trueResult.exitCode}: ${trueResult.stderr}`);
			}
			collectWasmWarmupDiagnostics(
				wasmWarmupDebug,
				trueResult.stderr,
				wasmWarmupDiagnostics,
			);

			let chunks = 0;
			let callbackBytes = 0;
			let callbackStdout = "";
			start = performance.now();
			const lsResult = await vm.execArgv(
				"ls",
				[...variant.argsBeforeDir, guestDir],
				{
					...execOptions,
					onStdout: (chunk) => {
						chunks += 1;
						callbackBytes += chunk.byteLength;
						callbackStdout += Buffer.from(chunk).toString("utf8");
					},
				},
			);
			const lsMs = nowMs(start);
			if (lsResult.exitCode !== 0) {
				throw new Error(`guest ls exited ${lsResult.exitCode}: ${lsResult.stderr}`);
			}
			const stdout = callbackStdout.length > 0 ? callbackStdout : lsResult.stdout;
			guestStdoutReturnedCount = countGeneratedLsEntries(stdout);
			collectWasmWarmupDiagnostics(
				wasmWarmupDebug,
				lsResult.stderr,
				wasmWarmupDiagnostics,
			);
			collectWasiSyscallMetrics(
				wasiSyscallCounters,
				lsResult.stderr,
				wasiSyscallMetrics,
			);

			if (i >= warmup) {
				guestTrueMs.push(trueMs);
				guestLsMs.push(lsMs);
				stdoutBytes.push(Buffer.byteLength(stdout));
				stdoutCallbackChunks.push(chunks);
				stdoutCallbackBytes.push(callbackBytes);
			}
		}
		if (i < warmup) {
			hostLsMs.splice(0, serialRuns);
		}
		console.error(
			`  variant=${variant.name} files=${fileCount} iter=${i}: true.p50=${round(stats(guestTrueMs.length ? guestTrueMs : [0]).p50)}ms ls.p50=${round(stats(guestLsMs.length ? guestLsMs : [0]).p50)}ms${i < warmup ? " (warmup)" : ""}`,
		);
	}

	const hostLs = stats(hostLsMs);
	const guestTrue = stats(guestTrueMs);
	const guestLs = stats(guestLsMs);
	return {
		variant: variant.name,
		lsArgs: [...variant.argsBeforeDir, "<dir>"],
		fileCount,
		iterations,
		warmup,
		serialRuns,
		hostLs,
		guestTrue,
		guestLs,
		guestLsMinusTrueMs: round(guestLs.p50 - guestTrue.p50),
		guestVsHostRatio: round(guestLs.p50 / hostLs.p50),
		stdoutBytes: stats(stdoutBytes),
		stdoutCallbackChunks: stats(stdoutCallbackChunks),
		stdoutCallbackBytes: stats(stdoutCallbackBytes),
		...(wasiSyscallCounters
			? {
					wasiSyscalls: summarizeWasiSyscalls(wasiSyscallMetrics),
					fdReaddir: summarizeFdReaddir(wasiSyscallMetrics),
				}
			: {}),
		returnedCount: {
			host: hostReturnedCount,
			guestFixture: guestFixtureCount,
			guestStdout: guestStdoutReturnedCount,
		},
		raw: {
			hostLsMs,
			guestTrueMs,
			guestLsMs,
			stdoutBytes,
			stdoutCallbackChunks,
			stdoutCallbackBytes,
			...(wasmWarmupDebug ? { wasmWarmupDiagnostics } : {}),
			...(wasiSyscallCounters ? { wasiSyscallMetrics } : {}),
		},
	};
}

async function main(): Promise<void> {
	const {
		iterations,
		warmup,
		serialRuns,
		fileCounts,
		lsVariants,
		wasmWarmupDebug,
		wasiSyscallCounters,
	} =
		parseArgs();
	const hardware = getHardware();
	const agentosRoot = resolveAgentosRoot();
	const moduleBytes = lsModuleBytes(agentosRoot);

	console.error("=== WASI/coreutils `ls` Scaling Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(
		`Iterations: ${iterations} (+ ${warmup} warmup), serial runs: ${serialRuns}, file counts: ${fileCounts.join(",")}, variants: ${lsVariants.map((variant) => variant.name).join(",")}`,
	);
	if (moduleBytes !== null) {
		console.error(`ls module bytes: ${moduleBytes}`);
	}
	if (wasmWarmupDebug) {
		console.error("WASM warmup debug: enabled");
	}
	if (wasiSyscallCounters) {
		console.error("WASI syscall counters: enabled");
	}

	const sidecar = await createBenchSidecar();
	const hostFixtures: { fileCount: number; hostDir: string; guestDir: string }[] = [];
	try {
		const vm = await createVm(sidecar);
		try {
			for (const fileCount of fileCounts) {
				const hostDir = createHostFixture(fileCount);
				const guestDir = `/tmp/wasi-ls-bench-${fileCount}`;
				hostFixtures.push({ fileCount, hostDir, guestDir });
				await setupGuestFixture(vm, guestDir, fileCount);
			}

			const cases: WasiLsCaseResult[] = [];
			for (const variant of lsVariants) {
				for (const fixture of hostFixtures) {
					cases.push(
						await runCase(
							vm,
							fixture.hostDir,
							fixture.guestDir,
							fixture.fileCount,
							variant,
							iterations,
							warmup,
							serialRuns,
							wasmWarmupDebug,
							wasiSyscallCounters,
						),
					);
				}
			}

			for (const variant of lsVariants) {
				const emptyCase = cases.find(
					(result) => result.variant === variant.name && result.fileCount === 0,
				);
				if (!emptyCase) continue;
				for (const result of cases.filter(
					(result) => result.variant === variant.name,
				)) {
					result.guestDeltaFromEmptyMs = round(
						result.guestLs.p50 - emptyCase.guestLs.p50,
					);
					result.hostDeltaFromEmptyMs = round(
						result.hostLs.p50 - emptyCase.hostLs.p50,
					);
				}
			}

			printTable(
				[
					"variant",
					"files",
					"host ls p50",
					"guest true p50",
					"guest ls p50",
					"ls-true",
					"guest delta empty",
					"guest/host",
					"stdout names",
					"stdout bytes",
					"stdout chunks",
					"fd_readdir",
				],
				cases.map((result) => [
					result.variant,
					result.fileCount,
					`${result.hostLs.p50}ms`,
					`${result.guestTrue.p50}ms`,
					`${result.guestLs.p50}ms`,
					`${result.guestLsMinusTrueMs}ms`,
					result.guestDeltaFromEmptyMs === undefined
						? "n/a"
						: `${result.guestDeltaFromEmptyMs}ms`,
					`${result.guestVsHostRatio}x`,
					`${result.returnedCount.guestStdout}/${result.returnedCount.guestFixture}`,
					result.stdoutBytes.p50,
					result.stdoutCallbackChunks.p50,
					result.fdReaddir
						? `${result.fdReaddir.count} calls/${result.fdReaddir.recordsReturned} records`
						: "n/a",
				]),
			);

			console.log(
				JSON.stringify(
					{
						benchmark: "wasi-ls-scaling",
						hardware,
						iterations,
						warmup,
						serialRuns,
						fileCounts,
						lsVariants: lsVariants.map((variant) => ({
							name: variant.name,
							args: [...variant.argsBeforeDir, "<dir>"],
						})),
						moduleBytes,
						wasmWarmupDebug,
						wasiSyscallCounters,
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
		for (const fixture of hostFixtures) {
			rmSync(fixture.hostDir, { recursive: true, force: true });
		}
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
