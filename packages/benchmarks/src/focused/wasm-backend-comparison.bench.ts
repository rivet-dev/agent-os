import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, statSync, writeFileSync } from "node:fs";
import http from "node:http";
import { cpus, hostname, totalmem } from "node:os";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import {
	type ProcessMemorySnapshot,
	readProcessMemorySnapshot,
} from "../lib/memory.js";
import {
	type BenchVm,
	createBenchSidecar,
	createBenchVm,
	formatSidecarProvenance,
	resolveBenchCommandsDir,
	resolveBenchSidecarProvenance,
} from "../lib/vm.js";

type Backend = "v8" | "wasmtime";

interface Workload {
	name: string;
	command: string;
	args: (context: WorkloadContext) => string[];
	stdin?: string;
	validate: (result: CommandResult, context: WorkloadContext) => void;
}

interface WorkloadContext {
	port: number;
}

interface CommandResult {
	stdout: string;
	stderr: string;
	exitCode: number;
}

interface PhaseDiagnostic {
	backend?: string;
	sourceModuleBytes?: number | null;
	moduleCacheHit?: boolean | null;
	moduleBytes?: number | null;
	firstHostCallMs?: number | null;
	firstGuestHostCallMs?: number | null;
	firstOutputMs?: number | null;
	guestLinearMemoryBytes?: number;
	asyncStackBytes?: number;
	reservedStoreBytes?: number;
	totalMs?: number;
	phases?: Array<{ name: string; ms: number }>;
	[key: string]: unknown;
}

interface TimedMemory {
	start: ProcessMemorySnapshot;
	peak: ProcessMemorySnapshot;
	end: ProcessMemorySnapshot;
}

const PHASE_PREFIX = "__AGENTOS_WASM_PHASE_METRICS__:";
const freshProcesses = integerEnv("AGENTOS_WASM_BENCH_FRESH_PROCESSES", 5);
const samplesPerProcess = integerEnv("AGENTOS_WASM_BENCH_SAMPLES", 5);
const concurrencyLevels = listEnv(
	"AGENTOS_WASM_BENCH_CONCURRENCY",
	[1, 10, 50, 100, 200],
);
const retainedSettleMs = integerEnv(
	"AGENTOS_WASM_BENCH_RETAINED_SETTLE_MS",
	250,
);
const outputPath = resolve(
	process.env.AGENTOS_WASM_BENCH_OUTPUT ??
		join(
			dirname(fileURLToPath(import.meta.url)),
			"../../results/wasm-backend-comparison.json",
		),
);
const commandsDir = resolveBenchCommandsDir(
	process.env.AGENTOS_WASM_COMMANDS_DIR,
);
const sidecarProvenance = resolveBenchSidecarProvenance();

const allWorkloads: Workload[] = [
	{
		name: "trivial",
		command: "true",
		args: () => [],
		validate: expectExitZero,
	},
	{
		name: "coreutils",
		command: "ls",
		args: () => ["-la", "/tmp/wasmtime-bench-tree"],
		validate: (result) => {
			expectExitZero(result);
			if (!result.stdout.includes("file-063"))
				throw new Error("ls output missing fixture");
		},
	},
	{
		name: "shell",
		command: "sh",
		args: () => ["-c", "printf 'alpha\\nbeta\\n' | /opt/agentos/bin/grep beta"],
		validate: (result) => {
			expectExitZero(result);
			if (result.stdout.trim() !== "beta")
				throw new Error("shell pipeline output mismatch");
		},
	},
	{
		name: "curl",
		command: "curl",
		args: ({ port }) => ["-fsS", `http://127.0.0.1:${port}/payload`],
		validate: (result) => {
			expectExitZero(result);
			if (result.stdout !== "wasmtime-benchmark-loopback") {
				throw new Error("curl body mismatch");
			}
		},
	},
	{
		name: "sqlite",
		command: "sqlite3",
		args: () => [
			":memory:",
			"select sum(value) from generate_series(1, 1000);",
		],
		validate: (result) => {
			expectExitZero(result);
			if (result.stdout.trim() !== "500500")
				throw new Error("sqlite result mismatch");
		},
	},
	{
		name: "vim",
		command: "vim",
		args: () => ["-u", "NONE", "-N", "-n", "-es", "-c", "q"],
		validate: expectExitZero,
	},
	{
		name: "large-module",
		command: "git",
		args: () => ["--version"],
		validate: (result) => {
			expectExitZero(result);
			if (!result.stdout.startsWith("git version"))
				throw new Error("git version mismatch");
		},
	},
	{
		name: "compute-heavy",
		command: "sha256sum",
		args: () => ["/tmp/wasmtime-bench-compute.bin"],
		validate: (result) => {
			expectExitZero(result);
			if (!/^[0-9a-f]{64}\s/u.test(result.stdout))
				throw new Error("sha256 output mismatch");
		},
	},
	{
		name: "host-call-heavy",
		command: "find",
		args: () => ["/tmp/wasmtime-bench-tree", "-type", "f", "-print"],
		validate: (result) => {
			expectExitZero(result);
			if (!result.stdout.includes("file-063"))
				throw new Error("find output missing fixture");
		},
	},
];
const workloadFilter = process.env.AGENTOS_WASM_BENCH_WORKLOADS?.split(",")
	.map((entry) => entry.trim())
	.filter(Boolean);
const workloads = workloadFilter
	? allWorkloads.filter((workload) => workloadFilter.includes(workload.name))
	: allWorkloads;
if (workloads.length === 0)
	throw new Error("workload filter selected no workloads");

const diverseConcurrency = [
	["true", []],
	["printf", ["x"]],
	["pwd", []],
	["uname", []],
	["id", []],
	["date", ["+%s"]],
	["dirname", ["/a/b"]],
	["basename", ["/a/b"]],
] as const;

async function main(): Promise<void> {
	if (sidecarProvenance.profile !== "release") {
		throw new Error(
			`Wasmtime backend comparison requires a release sidecar, got ${formatSidecarProvenance(sidecarProvenance)}`,
		);
	}
	const loopback = await listenLoopback();
	try {
		const startedAt = new Date().toISOString();
		const result: Record<string, unknown> = {
			metadata: {
				startedAt,
				hostname: hostname(),
				platform: process.platform,
				arch: process.arch,
				cpuModel: cpus()[0]?.model ?? "unknown",
				logicalCpus: cpus().length,
				totalMemoryBytes: totalmem(),
				kernel: execFileSync("uname", ["-srvm"], {
					encoding: "utf8",
				}).trim(),
				node: process.version,
				sidecar: sidecarProvenance,
				commandsDir,
				freshProcesses,
				samplesPerProcess,
				concurrencyLevels,
				memoryAllocation: "on-demand",
				memoryInitCow: true,
				pooling: false,
				aot: false,
				wizer: false,
				liveSnapshots: false,
			},
			modules: moduleInventory(),
			fresh: [],
			concurrency: [],
			paths: [],
			status: "running",
		};
		for (let processIndex = 0; processIndex < freshProcesses; processIndex++) {
			for (const backend of ["v8", "wasmtime"] as const) {
				console.error(
					`fresh process ${processIndex + 1}/${freshProcesses} ${backend}`,
				);
				(result.fresh as unknown[]).push(
					await runFreshProcess(backend, processIndex, loopback.port),
				);
				writeCheckpoint(result);
			}
		}
		for (const backend of ["v8", "wasmtime"] as const) {
			console.error(`concurrency ${backend}`);
			(result.concurrency as unknown[]).push(
				await runConcurrency(backend, loopback.port),
			);
			writeCheckpoint(result);
			console.error(`safety/control paths ${backend}`);
			(result.paths as unknown[]).push(
				await runControlPaths(backend, loopback.port),
			);
			writeCheckpoint(result);
		}
		result.summary = summarize(result);
		result.completedAt = new Date().toISOString();
		result.status = "complete";
		writeCheckpoint(result);
		console.log(JSON.stringify(result.summary, null, 2));
		console.error(`raw results: ${outputPath}`);
	} finally {
		await closeServer(loopback.server);
	}
}

function writeCheckpoint(result: Record<string, unknown>): void {
	writeFileSync(outputPath, `${JSON.stringify(result, null, 2)}\n`);
}

async function runFreshProcess(
	backend: Backend,
	processIndex: number,
	port: number,
) {
	const sidecar = createBenchSidecar();
	let vm: BenchVm | undefined;
	try {
		const vmStarted = performance.now();
		vm = await createBenchVm({ sidecar, loopbackExemptPorts: [port] });
		const activeVm = vm;
		const vmSetupMs = performance.now() - vmStarted;
		const fixtureStarted = performance.now();
		await prepareFixtures(activeVm);
		const fixtureSetupMs = performance.now() - fixtureStarted;
		const pid = requiredSidecarPid(activeVm);
		const baseline = readProcessMemorySnapshot(pid);
		const workloadResults = [];
		for (const workload of workloads) {
			console.error(`  ${backend} ${workload.name}`);
			const samples = [];
			for (
				let sampleIndex = 0;
				sampleIndex < samplesPerProcess;
				sampleIndex++
			) {
				const before = await activeVm.getResourceSnapshot();
				const measured = await measureCommandMemory(pid, () =>
					activeVm.execArgv(workload.command, workload.args({ port }), {
						wasmBackend: backend,
						stdin: workload.stdin,
						env: { AGENTOS_WASM_WARMUP_DEBUG: "1" },
					}),
				);
				let validationError: string | null = null;
				try {
					workload.validate(measured.value, { port });
				} catch (error) {
					validationError = String(error);
				}
				const phase = parsePhaseDiagnostic(measured.value.stderr);
				const expectedSourceBytes = statSync(
					join(commandsDir, workload.command),
				).size;
				if (phase?.sourceModuleBytes !== expectedSourceBytes) {
					validationError ??= `${backend} ${workload.name} executed ${String(phase?.sourceModuleBytes)} source bytes; expected ${expectedSourceBytes}`;
				}
				const after = await activeVm.getResourceSnapshot();
				samples.push({
					index: sampleIndex,
					cacheState: sampleIndex === 0 ? "fresh" : "warm",
					durationMs: measured.durationMs,
					exitCode: measured.value.exitCode,
					passed: validationError === null,
					validationError,
					stdoutBytes: Buffer.byteLength(measured.value.stdout),
					stderrBytes: Buffer.byteLength(
						stripDiagnostics(measured.value.stderr),
					),
					phase,
					memory: measured.memory,
					resourceBefore: projectResources(before),
					resourceAfter: projectResources(after),
				});
			}
			workloadResults.push({
				name: workload.name,
				command: workload.command,
				samples,
			});
		}
		const beforeDispose = await activeVm.getResourceSnapshot();
		await activeVm.dispose();
		vm = undefined;
		await delay(retainedSettleMs);
		const retained = readProcessMemorySnapshot(pid);
		return {
			backend,
			processIndex,
			vmSetupMs,
			fixtureSetupMs,
			baseline,
			beforeDispose: projectResources(beforeDispose),
			retained,
			retainedDelta: memoryDelta(retained, baseline),
			workloads: workloadResults,
		};
	} finally {
		if (vm) await vm.dispose().catch(() => undefined);
		await sidecar.dispose();
	}
}

async function runConcurrency(backend: Backend, port: number) {
	const sidecar = createBenchSidecar();
	let vm: BenchVm | undefined;
	try {
		vm = await createBenchVm({ sidecar, loopbackExemptPorts: [port] });
		const activeVm = vm;
		await prepareFixtures(activeVm);
		for (const [command, args] of diverseConcurrency) {
			const warm = await activeVm.execArgv(command, [...args], {
				wasmBackend: backend,
			});
			if (warm.exitCode !== 0)
				throw new Error(`${backend} concurrency warmup ${command} failed`);
		}
		await waitForRuntimeDrain(activeVm);
		const pid = requiredSidecarPid(activeVm);
		const levels = [];
		for (const level of concurrencyLevels) {
			for (const mode of ["repeated", "diverse"] as const) {
				const measured = await measureCommandMemory(pid, async () => {
					const settled = await Promise.allSettled(
						Array.from({ length: level }, (_, index) => {
							const [command, args] =
								mode === "repeated"
									? (["true", []] as const)
									: diverseConcurrency[index % diverseConcurrency.length];
							return activeVm.execArgv(command, [...args], {
								wasmBackend: backend,
							});
						}),
					);
					return settled;
				});
				const fulfilled = measured.value.filter(
					(entry): entry is PromiseFulfilledResult<CommandResult> =>
						entry.status === "fulfilled",
				);
				const failedExitCodes = fulfilled.filter(
					(entry) => entry.value.exitCode !== 0,
				);
				const successful = fulfilled.length - failedExitCodes.length;
				const rejected = measured.value
					.filter(
						(entry): entry is PromiseRejectedResult =>
							entry.status === "rejected",
					)
					.map((entry) => String(entry.reason));
				const failureExamples = [
					...new Set(
						failedExitCodes.map((entry) =>
							stripDiagnostics(entry.value.stderr).slice(0, 1_000),
						),
					),
				].slice(0, 3);
				const drainMs = await waitForRuntimeDrain(activeVm);
				levels.push({
					level,
					mode,
					durationMs: measured.durationMs,
					throughputPerSecond: (successful * 1_000) / measured.durationMs,
					fulfilled: fulfilled.length,
					successful,
					failedExitCodes: failedExitCodes.length,
					failureExamples,
					rejectedCount: rejected.length,
					rejectionExamples: [...new Set(rejected)].slice(0, 3),
					memory: measured.memory,
					drainMs,
				});
			}
		}
		return { backend, levels };
	} finally {
		if (vm) await vm.dispose().catch(() => undefined);
		await sidecar.dispose();
	}
}

async function runControlPaths(backend: Backend, port: number) {
	const allowedSidecar = createBenchSidecar();
	const deniedSidecar = createBenchSidecar();
	let allowed: BenchVm | undefined;
	let denied: BenchVm | undefined;
	try {
		allowed = await createBenchVm({
			sidecar: allowedSidecar,
			loopbackExemptPorts: [port],
		});
		await prepareFixtures(allowed);
		denied = await createBenchVm({
			sidecar: deniedSidecar,
			loopbackExemptPorts: [port],
			permissions: { network: "deny" },
		});
		const denial = await denied.execArgv(
			"curl",
			["-fsS", `http://127.0.0.1:${port}/payload`],
			{
				wasmBackend: backend,
				timeout: 5_000,
			},
		);

		const cancellationController = new AbortController();
		const cancellationStarted = performance.now();
		const cancellationPromise = allowed.execArgv(
			"sh",
			["-c", "while :; do :; done"],
			{
				wasmBackend: backend,
				signal: cancellationController.signal,
			},
		);
		setTimeout(() => cancellationController.abort(), 25);
		let cancellation: Record<string, unknown>;
		try {
			const value = await cancellationPromise;
			cancellation = { rejected: false, value };
		} catch (error) {
			cancellation = {
				rejected: true,
				name: error instanceof Error ? error.name : "unknown",
				message: String(error),
			};
		}
		cancellation.durationMs = performance.now() - cancellationStarted;

		const resourceStarted = performance.now();
		const resource = await allowed.execArgv(
			"sh",
			["-c", "while :; do :; done"],
			{
				wasmBackend: backend,
				cpuTimeLimitMs: 25,
				timeout: 5_000,
			},
		);
		return {
			backend,
			denial: {
				exitCode: denial.exitCode,
				stderr: stripDiagnostics(denial.stderr).slice(0, 1_000),
				passed: denial.exitCode !== 0,
			},
			cancellation: {
				...cancellation,
				passed: cancellation.rejected === true,
			},
			resourceLimit: {
				exitCode: resource.exitCode,
				durationMs: performance.now() - resourceStarted,
				stderr: stripDiagnostics(resource.stderr).slice(0, 1_000),
				passed: resource.exitCode !== 0,
			},
		};
	} finally {
		if (denied) await denied.dispose().catch(() => undefined);
		if (allowed) await allowed.dispose().catch(() => undefined);
		await deniedSidecar.dispose();
		await allowedSidecar.dispose();
	}
}

async function prepareFixtures(vm: BenchVm): Promise<void> {
	await vm.mkdir("/tmp/wasmtime-bench-tree", { recursive: true });
	await Promise.all(
		Array.from({ length: 64 }, (_, index) =>
			vm.writeFile(
				`/tmp/wasmtime-bench-tree/file-${index.toString().padStart(3, "0")}`,
				`fixture-${index}\n`,
			),
		),
	);
	const compute = new Uint8Array(4 * 1024 * 1024);
	for (let index = 0; index < compute.length; index++)
		compute[index] = index & 0xff;
	await vm.writeFile("/tmp/wasmtime-bench-compute.bin", compute);
}

async function measureCommandMemory<T>(pid: number, run: () => Promise<T>) {
	const start = readProcessMemorySnapshot(pid);
	let peak = start;
	const sample = () => {
		try {
			peak = maxMemory(peak, readProcessMemorySnapshot(pid));
		} catch {
			// The sidecar exiting is reported by the command itself.
		}
	};
	const poll = setInterval(sample, 5);
	const started = performance.now();
	try {
		const value = await run();
		sample();
		const end = readProcessMemorySnapshot(pid);
		return {
			value,
			durationMs: performance.now() - started,
			memory: { start, peak: maxMemory(peak, end), end } satisfies TimedMemory,
		};
	} finally {
		clearInterval(poll);
	}
}

async function waitForRuntimeDrain(vm: BenchVm): Promise<number> {
	const started = performance.now();
	const timeoutMs = 5_000;
	for (;;) {
		const snapshot = await vm.getResourceSnapshot();
		if (
			snapshot.runningProcesses === 0 &&
			snapshot.wasmReservedMemoryBytes === 0
		) {
			// Exit delivery precedes the executor worker's final permit drop by a
			// very small interval. Require one quiet scheduler turn so the next
			// level measures its own admission capacity rather than prior teardown.
			await delay(25);
			return performance.now() - started;
		}
		if (performance.now() - started >= timeoutMs) {
			throw new Error(
				`runtime did not drain within ${timeoutMs} ms (runningProcesses=${snapshot.runningProcesses}, wasmReservedMemoryBytes=${snapshot.wasmReservedMemoryBytes})`,
			);
		}
		await delay(10);
	}
}

function summarize(result: Record<string, unknown>) {
	const fresh = result.fresh as Array<{
		backend: Backend;
		retainedDelta: ProcessMemorySnapshot;
		workloads: Array<{
			name: string;
			samples: Array<{
				durationMs: number;
				cacheState: "fresh" | "warm";
				passed: boolean;
			}>;
		}>;
	}>;
	const workloadRows = workloads.map((workload) => {
		const samples = (backend: Backend, cacheState?: "fresh" | "warm") =>
			fresh
				.filter((entry) => entry.backend === backend)
				.flatMap(
					(entry) =>
						entry.workloads.find(
							(candidate) => candidate.name === workload.name,
						)?.samples ?? [],
				)
				.filter(
					(sample) =>
						cacheState === undefined || sample.cacheState === cacheState,
				)
				.map((sample) => sample.durationMs);
		const v8 = samples("v8");
		const wasmtime = samples("wasmtime");
		const v8Cold = samples("v8", "fresh");
		const wasmtimeCold = samples("wasmtime", "fresh");
		const v8Warm = samples("v8", "warm");
		const wasmtimeWarm = samples("wasmtime", "warm");
		return {
			name: workload.name,
			correctness: {
				v8Failures: fresh
					.filter((entry) => entry.backend === "v8")
					.flatMap(
						(entry) =>
							entry.workloads.find(
								(candidate) => candidate.name === workload.name,
							)?.samples ?? [],
					)
					.filter((sample) => !sample.passed).length,
				wasmtimeFailures: fresh
					.filter((entry) => entry.backend === "wasmtime")
					.flatMap(
						(entry) =>
							entry.workloads.find(
								(candidate) => candidate.name === workload.name,
							)?.samples ?? [],
					)
					.filter((sample) => !sample.passed).length,
			},
			v8: stats(v8),
			wasmtime: stats(wasmtime),
			cold: {
				v8: stats(v8Cold),
				wasmtime: stats(wasmtimeCold),
				p50Ratio: ratio(quantile(wasmtimeCold, 0.5), quantile(v8Cold, 0.5)),
			},
			warm: {
				v8: stats(v8Warm),
				wasmtime: stats(wasmtimeWarm),
				p50Ratio: ratio(quantile(wasmtimeWarm, 0.5), quantile(v8Warm, 0.5)),
			},
			p50Ratio: quantile(wasmtime, 0.5) / quantile(v8, 0.5),
			p95Ratio: quantile(wasmtime, 0.95) / quantile(v8, 0.95),
		};
	});
	const geometricMeanP50Ratio = Math.exp(
		workloadRows.reduce((sum, row) => sum + Math.log(row.p50Ratio), 0) /
			workloadRows.length,
	);
	const concurrency = result.concurrency as Array<{
		backend: Backend;
		levels: Array<{
			level: number;
			mode: string;
			throughputPerSecond: number;
			failedExitCodes: number;
			rejectedCount: number;
		}>;
	}>;
	const throughputRows =
		concurrency
			.find((entry) => entry.backend === "v8")
			?.levels.map((v8) => {
				const wasmtime = concurrency
					.find((entry) => entry.backend === "wasmtime")
					?.levels.find(
						(candidate) =>
							candidate.level === v8.level && candidate.mode === v8.mode,
					);
				return {
					level: v8.level,
					mode: v8.mode,
					v8: v8.throughputPerSecond,
					wasmtime: wasmtime?.throughputPerSecond ?? 0,
					ratio: (wasmtime?.throughputPerSecond ?? 0) / v8.throughputPerSecond,
				};
			}) ?? [];
	const retainedMedian = (backend: Backend, key: "rssBytes" | "pssBytes") =>
		quantile(
			fresh
				.filter((entry) => entry.backend === backend)
				.map((entry) => entry.retainedDelta[key]),
			0.5,
		);
	const retained = {
		v8RssBytes: retainedMedian("v8", "rssBytes"),
		wasmtimeRssBytes: retainedMedian("wasmtime", "rssBytes"),
		v8PssBytes: retainedMedian("v8", "pssBytes"),
		wasmtimePssBytes: retainedMedian("wasmtime", "pssBytes"),
	};
	const retainedAllowance = (baseline: number) =>
		Math.max(baseline * 0.1, 4 * 1024 * 1024);
	const paths = result.paths as Array<{
		denial: { passed: boolean };
		cancellation: { passed: boolean };
		resourceLimit: { passed: boolean };
	}>;
	const gates = {
		correctness:
			workloadRows.every(
				(row) =>
					row.correctness.v8Failures === 0 &&
					row.correctness.wasmtimeFailures === 0,
			) &&
			paths.every(
				(entry) =>
					entry.denial.passed &&
					entry.cancellation.passed &&
					entry.resourceLimit.passed,
			) &&
			concurrency.every((entry) =>
				entry.levels
					.filter((level) => level.level <= 10)
					.every(
						(level) => level.failedExitCodes === 0 && level.rejectedCount === 0,
					),
			),
		geometricMeanP50: geometricMeanP50Ratio <= 1.1,
		individualP95: workloadRows.every((row) => row.p95Ratio <= 1.2),
		throughput: throughputRows.every((row) =>
			row.v8 === 0 ? row.wasmtime >= row.v8 : row.ratio >= 0.9,
		),
		retainedRss:
			retained.wasmtimeRssBytes <=
			retained.v8RssBytes + retainedAllowance(retained.v8RssBytes),
		retainedPss:
			retained.wasmtimePssBytes <=
			retained.v8PssBytes + retainedAllowance(retained.v8PssBytes),
	};
	const preferredBackend = Object.values(gates).every(Boolean)
		? "wasmtime"
		: "v8";
	return {
		workloads: workloadRows,
		geometricMeanP50Ratio,
		throughput: throughputRows,
		retained,
		gates,
		preferredBackend,
		omissionBehavior: preferredBackend,
		rollbackBackend: "v8",
	};
}

function moduleInventory() {
	return [
		...new Set([
			...workloads.map((workload) => workload.command),
			...diverseConcurrency.map(([c]) => c),
		]),
	]
		.sort()
		.map((command) => {
			const path = join(commandsDir, command);
			const bytes = readFileSync(path);
			return {
				command,
				path,
				bytes: statSync(path).size,
				sha256: createHash("sha256").update(bytes).digest("hex"),
			};
		});
}

function projectResources(
	resource: Awaited<ReturnType<BenchVm["getResourceSnapshot"]>>,
) {
	return {
		runningProcesses: resource.runningProcesses,
		openFds: resource.openFds,
		pipes: resource.pipes,
		pipeBufferedBytes: resource.pipeBufferedBytes,
		ptys: resource.ptys,
		ptyBufferedInputBytes: resource.ptyBufferedInputBytes,
		ptyBufferedOutputBytes: resource.ptyBufferedOutputBytes,
		sockets: resource.sockets,
		socketBufferedBytes: resource.socketBufferedBytes,
		socketDatagramQueueLen: resource.socketDatagramQueueLen,
		wasmReservedMemoryBytes: resource.wasmReservedMemoryBytes,
		wasmtimeEngineProfiles: resource.wasmtimeEngineProfiles,
		wasmtimeModuleEntries: resource.wasmtimeModuleEntries,
		wasmtimeModuleCacheHits: resource.wasmtimeModuleCacheHits,
		wasmtimeModuleCacheMisses: resource.wasmtimeModuleCacheMisses,
		wasmtimeModuleCacheEvictions: resource.wasmtimeModuleCacheEvictions,
		wasmtimeCompiledSourceBytes: resource.wasmtimeCompiledSourceBytes,
		wasmtimeChargedModuleBytes: resource.wasmtimeChargedModuleBytes,
		wasmtimeCompileTimeMicros: resource.wasmtimeCompileTimeMicros,
		wasmtimeProcessRetainedRssBytes: resource.wasmtimeProcessRetainedRssBytes,
		kernelBufferedBytes:
			resource.pipeBufferedBytes +
			resource.ptyBufferedInputBytes +
			resource.ptyBufferedOutputBytes +
			resource.socketBufferedBytes,
	};
}

function parsePhaseDiagnostic(stderr: string): PhaseDiagnostic | null {
	for (const line of stderr.split(/\r?\n/u).reverse()) {
		if (!line.startsWith(PHASE_PREFIX)) continue;
		try {
			return JSON.parse(line.slice(PHASE_PREFIX.length)) as PhaseDiagnostic;
		} catch {
			return null;
		}
	}
	return null;
}

function stripDiagnostics(stderr: string): string {
	return stderr
		.split(/\r?\n/u)
		.filter((line) => !line.startsWith("__AGENTOS_WASM_"))
		.join("\n")
		.trim();
}

function expectExitZero(result: CommandResult): void {
	if (result.exitCode !== 0) {
		throw new Error(
			`command exited ${result.exitCode}: ${stripDiagnostics(result.stderr)}`,
		);
	}
}

function maxMemory(
	left: ProcessMemorySnapshot,
	right: ProcessMemorySnapshot,
): ProcessMemorySnapshot {
	return {
		rssBytes: Math.max(left.rssBytes, right.rssBytes),
		peakRssBytes: Math.max(left.peakRssBytes, right.peakRssBytes),
		pssBytes: Math.max(left.pssBytes, right.pssBytes),
		virtualBytes: Math.max(left.virtualBytes, right.virtualBytes),
		minorFaults: Math.max(left.minorFaults, right.minorFaults),
		majorFaults: Math.max(left.majorFaults, right.majorFaults),
	};
}

function memoryDelta(
	after: ProcessMemorySnapshot,
	before: ProcessMemorySnapshot,
) {
	return {
		rssBytes: after.rssBytes - before.rssBytes,
		peakRssBytes: after.peakRssBytes - before.peakRssBytes,
		pssBytes: after.pssBytes - before.pssBytes,
		virtualBytes: after.virtualBytes - before.virtualBytes,
		minorFaults: after.minorFaults - before.minorFaults,
		majorFaults: after.majorFaults - before.majorFaults,
	};
}

function stats(values: number[]) {
	if (values.length === 0) {
		return { count: 0, min: null, p50: null, p95: null, max: null };
	}
	return {
		count: values.length,
		min: Math.min(...values),
		p50: quantile(values, 0.5),
		p95: quantile(values, 0.95),
		max: Math.max(...values),
	};
}

function quantile(values: number[], q: number): number {
	if (values.length === 0) return 0;
	const sorted = [...values].sort((a, b) => a - b);
	const index = (sorted.length - 1) * q;
	const lower = Math.floor(index);
	const fraction = index - lower;
	return sorted[lower] + (sorted[lower + 1] - sorted[lower] || 0) * fraction;
}

function ratio(numerator: number, denominator: number): number | null {
	return denominator === 0 ? null : numerator / denominator;
}

function requiredSidecarPid(vm: BenchVm): number {
	const pid = vm.sidecarPid();
	if (pid === null)
		throw new Error("benchmark could not resolve the sidecar pid");
	return pid;
}

function integerEnv(name: string, fallback: number): number {
	const value = process.env[name];
	if (!value) return fallback;
	const parsed = Number(value);
	if (!Number.isSafeInteger(parsed) || parsed <= 0)
		throw new Error(`${name} must be positive`);
	return parsed;
}

function listEnv(name: string, fallback: number[]): number[] {
	const value = process.env[name];
	return value
		? value.split(",").map((entry) => Number(entry.trim()))
		: fallback;
}

function delay(ms: number): Promise<void> {
	return new Promise((resolveDelay) => setTimeout(resolveDelay, ms));
}

async function listenLoopback(): Promise<{
	port: number;
	server: http.Server;
}> {
	const server = http.createServer((_request, response) => {
		response.setHeader("connection", "close");
		response.end("wasmtime-benchmark-loopback");
	});
	await new Promise<void>((resolveListen, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", resolveListen);
	});
	const address = server.address();
	if (!address || typeof address === "string")
		throw new Error("loopback listener has no port");
	return { port: address.port, server };
}

async function closeServer(server: http.Server): Promise<void> {
	await new Promise<void>((resolveClose, reject) => {
		server.close((error) => (error ? reject(error) : resolveClose()));
	});
}

void main().catch((error) => {
	console.error(error);
	process.exitCode = 1;
});
