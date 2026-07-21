import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { cpus, hostname, totalmem } from "node:os";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import {
	type ProcessTreeMemorySnapshot,
	readProcessTreeMemorySnapshot,
} from "../lib/memory.js";
import {
	type BenchVm,
	type BenchVmProcess,
	createBenchSidecar,
	createBenchVm,
	formatSidecarProvenance,
	resolveBenchSidecarProvenance,
} from "../lib/vm.js";

const here = dirname(fileURLToPath(import.meta.url));
const fixturePath = resolve(
	process.env.AGENTOS_WASM_THREADS_BENCH_FIXTURE ??
		join(here, "../../../../toolchain/c/build/pthread_benchmark.wasm"),
);
const fixtureDirectory = dirname(fixturePath);
const fixtureCommand = fixturePath.slice(fixtureDirectory.length + 1);
const outputPath = resolve(
	process.env.AGENTOS_WASM_THREADS_BENCH_OUTPUT ??
		join(here, "../../results/wasmtime-threads.json"),
);
const startupSamples = integerEnv("AGENTOS_WASM_THREADS_STARTUP_SAMPLES", 5);
const throughputSamples = integerEnv(
	"AGENTOS_WASM_THREADS_THROUGHPUT_SAMPLES",
	20,
);
const memorySamples = integerEnv("AGENTOS_WASM_THREADS_MEMORY_SAMPLES", 3);
const steadyStateWorkerThreads = 4;
const threadCounts = listEnv("AGENTOS_WASM_THREADS_COUNTS", [1, 2, 4, 8]);
const concurrencyLevels = listEnv(
	"AGENTOS_WASM_THREADS_CONCURRENCY",
	[1, 2, 4, 8],
);
const concurrentWorkerThreadsPerGroup = integerEnv(
	"AGENTOS_WASM_THREADS_CONCURRENT_WORKERS_PER_GROUP",
	2,
);
const operationTimeoutMs = integerEnv(
	"AGENTOS_WASM_THREADS_OPERATION_TIMEOUT_MS",
	15_000,
);
const sidecarProvenance = resolveBenchSidecarProvenance();

interface LiveWorkload {
	process: BenchVmProcess;
	exit: Promise<number>;
	readyMs: number;
	stdout(): string;
	stderr(): string;
}

async function main(): Promise<void> {
	if (process.platform !== "linux") {
		throw new Error("Wasmtime thread memory benchmarks require Linux /proc");
	}
	if (sidecarProvenance.profile !== "release") {
		throw new Error(
			`Wasmtime thread benchmarks require a release sidecar, got ${formatSidecarProvenance(sidecarProvenance)}`,
		);
	}
	if (!existsSync(fixturePath)) {
		throw new Error(
			`missing ${fixturePath}; run make -C toolchain/c pthread-benchmark-wasm`,
		);
	}
	for (const count of threadCounts) {
		if (!Number.isInteger(count) || count < 1 || count > 15) {
			throw new Error(`invalid thread count ${count}; expected 1..15`);
		}
	}
	if (concurrentWorkerThreadsPerGroup > 15) {
		throw new Error(
			`invalid concurrent worker count ${concurrentWorkerThreadsPerGroup}; expected 1..15`,
		);
	}
	const maxThreadsPerGroup =
		Math.max(
			...threadCounts,
			concurrentWorkerThreadsPerGroup,
			steadyStateWorkerThreads,
		) + 1;
	const maxConcurrentThreads =
		maxThreadsPerGroup * Math.max(...concurrencyLevels);
	const vmLimits = {
		wasm: {
			maxThreads: maxThreadsPerGroup,
			maxConcurrentThreads,
		},
	};

	const fixture = readFileSync(fixturePath);
	const result: Record<string, any> = {
		metadata: {
			startedAt: new Date().toISOString(),
			hostname: hostname(),
			platform: process.platform,
			arch: process.arch,
			cpuModel: cpus()[0]?.model ?? "unknown",
			logicalCpus: cpus().length,
			totalMemoryBytes: totalmem(),
			node: process.version,
			sidecar: sidecarProvenance,
			fixture: {
				path: fixturePath,
				sizeBytes: statSync(fixturePath).size,
				sha256: createHash("sha256").update(fixture).digest("hex"),
			},
			startupSamples,
			throughputSamples,
			memorySamples,
			threadCounts,
			concurrencyLevels,
			concurrentWorkerThreadsPerGroup,
			maxThreadsPerGroup,
			maxConcurrentThreads,
			backend: "wasmtime-threads",
			aot: false,
			pooling: false,
			wizer: false,
			liveSnapshots: false,
		},
		startup: [],
		throughput: null,
		memory: [],
		concurrency: [],
		status: "running",
	};
	writeCheckpoint(result);

	for (let index = 0; index < startupSamples; index++) {
		console.error(`thread cold start ${index + 1}/${startupSamples}`);
		const sidecar = createBenchSidecar();
		let vm: BenchVm | undefined;
		try {
			const vmStarted = performance.now();
			vm = await createBenchVm({
				sidecar,
				wasmCommandDirs: [fixtureDirectory],
				limits: vmLimits,
			});
			const vmSetupMs = performance.now() - vmStarted;
			const pid = requiredSidecarPid(vm);
			const baseline = readProcessTreeMemorySnapshot(pid);
			const workloadStarted = performance.now();
			const workload = await startWorkload(vm, steadyStateWorkerThreads, false);
			const exitCode = await withTimeout(
				workload.exit,
				operationTimeoutMs,
				"cold startup completion",
			);
			const drainMs = await waitForDrain(vm);
			const totalMs = performance.now() - workloadStarted;
			(result.startup as any[]).push({
				index,
				vmSetupMs,
				readyMs: workload.readyMs,
				totalMs,
				exitCode,
				stdout: workload.stdout(),
				stderr: workload.stderr(),
				baseline,
				retained: readProcessTreeMemorySnapshot(pid),
				drainMs,
				passed:
					exitCode === 0 &&
					workload.stdout().includes(`ready:${steadyStateWorkerThreads}`) &&
					workload.stdout().includes(`done:${steadyStateWorkerThreads}`),
			});
		} finally {
			if (vm) await vm.dispose().catch(() => undefined);
			await sidecar.dispose();
		}
		writeCheckpoint(result);
	}

	const sidecar = createBenchSidecar();
	let vm: BenchVm | undefined;
	try {
		vm = await createBenchVm({
			sidecar,
			wasmCommandDirs: [fixtureDirectory],
			limits: vmLimits,
		});
		const pid = requiredSidecarPid(vm);
		await runCompleted(vm, 1);
		await waitForDrain(vm);

		const throughputDurations: number[] = [];
		for (let index = 0; index < throughputSamples; index++) {
			const started = performance.now();
			await runCompleted(vm, steadyStateWorkerThreads);
			throughputDurations.push(performance.now() - started);
		}
		await waitForDrain(vm);
		result.throughput = {
			workerThreadsPerExecution: steadyStateWorkerThreads,
			totalGuestThreadsPerExecution: steadyStateWorkerThreads + 1,
			durationsMs: throughputDurations,
			p50Ms: percentile(throughputDurations, 0.5),
			p95Ms: percentile(throughputDurations, 0.95),
			executionsPerSecond:
				(throughputSamples * 1_000) /
				throughputDurations.reduce((total, value) => total + value, 0),
			passed: throughputDurations.length === throughputSamples,
		};

		for (const threadCount of threadCounts) {
			for (let sample = 0; sample < memorySamples; sample++) {
				console.error(
					`thread memory count=${threadCount} sample=${sample + 1}`,
				);
				const baseline = readProcessTreeMemorySnapshot(pid);
				const workload = await startWorkload(vm, threadCount, true);
				await delay(50);
				const live = readProcessTreeMemorySnapshot(pid);
				const terminationStarted = performance.now();
				workload.process.kill("SIGTERM");
				const exitCode = await withTimeout(
					workload.exit,
					operationTimeoutMs,
					`termination with ${threadCount} threads`,
				);
				const terminationMs = performance.now() - terminationStarted;
				const drainMs = await waitForDrain(vm);
				(result.memory as any[]).push({
					threadCount,
					sample,
					baseline,
					live,
					delta: memoryDelta(live, baseline),
					exitCode,
					terminationMs,
					drainMs,
					passed: exitCode >= 128 && terminationMs <= operationTimeoutMs,
				});
			}
		}

		for (const groupCount of concurrencyLevels) {
			console.error(
				`thread concurrency groups=${groupCount} workersPerGroup=${concurrentWorkerThreadsPerGroup}`,
			);
			const baseline = readProcessTreeMemorySnapshot(pid);
			const workloads = await Promise.all(
				Array.from({ length: groupCount }, () =>
					startWorkload(vm!, concurrentWorkerThreadsPerGroup, true),
				),
			);
			await delay(50);
			const live = readProcessTreeMemorySnapshot(pid);
			const terminationStarted = performance.now();
			for (const workload of workloads) workload.process.kill("SIGTERM");
			const exitCodes = await withTimeout(
				Promise.all(workloads.map((workload) => workload.exit)),
				operationTimeoutMs,
				`terminating ${groupCount} concurrent thread groups`,
			);
			const terminationMs = performance.now() - terminationStarted;
			const drainMs = await waitForDrain(vm);
			(result.concurrency as any[]).push({
				groupCount,
				workerThreadsPerGroup: concurrentWorkerThreadsPerGroup,
				totalGuestThreads: groupCount * (concurrentWorkerThreadsPerGroup + 1),
				readyMs: Math.max(...workloads.map((workload) => workload.readyMs)),
				baseline,
				live,
				delta: memoryDelta(live, baseline),
				exitCodes,
				terminationMs,
				drainMs,
				passed:
					exitCodes.every((exitCode) => exitCode >= 128) &&
					terminationMs <= operationTimeoutMs,
			});
		}
	} finally {
		if (vm) await vm.dispose().catch(() => undefined);
		await sidecar.dispose();
	}

	result.summary = summarize(result);
	result.completedAt = new Date().toISOString();
	result.status = result.summary.passed ? "complete" : "failed";
	writeCheckpoint(result);
	console.log(JSON.stringify(result.summary, null, 2));
	console.error(`raw results: ${outputPath}`);
	if (!result.summary.passed) {
		throw new Error("Wasmtime thread benchmark validation failed");
	}
}

async function startWorkload(
	vm: BenchVm,
	threadCount: number,
	park: boolean,
): Promise<LiveWorkload> {
	let stdout = "";
	let stderr = "";
	let ready = false;
	let resolveReady!: () => void;
	let rejectReady!: (error: Error) => void;
	const readyPromise = new Promise<void>(
		(resolveReadyValue, rejectReadyValue) => {
			resolveReady = resolveReadyValue;
			rejectReady = rejectReadyValue;
		},
	);
	const started = performance.now();
	const child = vm.spawn(
		fixtureCommand,
		[String(threadCount), park ? "1" : "0"],
		{
			wasmBackend: "wasmtime-threads",
			onStdout(data) {
				stdout += new TextDecoder().decode(data);
				if (!ready && stdout.includes(`ready:${threadCount}`)) {
					ready = true;
					resolveReady();
				}
			},
			onStderr(data) {
				stderr += new TextDecoder().decode(data);
			},
		},
	);
	const exit = child.wait();
	exit.then(
		(code) => {
			if (!ready) {
				rejectReady(
					new Error(
						`pthread benchmark exited ${code} before ready; stderr=${stderr}`,
					),
				);
			}
		},
		(error) => {
			if (!ready)
				rejectReady(error instanceof Error ? error : new Error(String(error)));
		},
	);
	await withTimeout(readyPromise, operationTimeoutMs, "pthread readiness");
	return {
		process: child,
		exit,
		readyMs: performance.now() - started,
		stdout: () => stdout,
		stderr: () => stderr,
	};
}

async function runCompleted(vm: BenchVm, threadCount: number): Promise<void> {
	const workload = await startWorkload(vm, threadCount, false);
	const exitCode = await withTimeout(
		workload.exit,
		operationTimeoutMs,
		`completed ${threadCount}-thread workload`,
	);
	if (
		exitCode !== 0 ||
		!workload.stdout().includes(`ready:${threadCount}`) ||
		!workload.stdout().includes(`done:${threadCount}`)
	) {
		throw new Error(
			`${threadCount}-thread workload failed: exit=${exitCode} stdout=${workload.stdout()} stderr=${workload.stderr()}`,
		);
	}
}

async function waitForDrain(vm: BenchVm): Promise<number> {
	const started = performance.now();
	for (;;) {
		const snapshot = await vm.getResourceSnapshot();
		if (
			snapshot.runningProcesses === 0 &&
			snapshot.wasmReservedMemoryBytes === 0
		) {
			return performance.now() - started;
		}
		if (performance.now() - started >= operationTimeoutMs) {
			throw new Error(
				`thread resources did not drain: running=${snapshot.runningProcesses} wasmBytes=${snapshot.wasmReservedMemoryBytes}`,
			);
		}
		await delay(10);
	}
}

function summarize(result: Record<string, any>) {
	const startup = result.startup as any[];
	const memory = result.memory as any[];
	const concurrency = result.concurrency as any[];
	const oneThreadPss = median(
		memory
			.filter((row) => row.threadCount === Math.min(...threadCounts))
			.map((row) => row.delta.pssBytes),
	);
	const maxThreadPss = median(
		memory
			.filter((row) => row.threadCount === Math.max(...threadCounts))
			.map((row) => row.delta.pssBytes),
	);
	const threadSpan = Math.max(...threadCounts) - Math.min(...threadCounts);
	return {
		coldReadyP50Ms: percentile(
			startup.map((row) => row.readyMs),
			0.5,
		),
		coldReadyP95Ms: percentile(
			startup.map((row) => row.readyMs),
			0.95,
		),
		warmThroughputPerSecond: result.throughput.executionsPerSecond,
		oneThreadGroupPssDeltaBytes: oneThreadPss,
		maxThreadGroupPssDeltaBytes: maxThreadPss,
		perAdditionalThreadPssBytes:
			threadSpan > 0
				? Math.max(0, maxThreadPss - oneThreadPss) / threadSpan
				: 0,
		maxTerminationMs: Math.max(
			0,
			...memory.map((row) => row.terminationMs),
			...concurrency.map((row) => row.terminationMs),
		),
		maxConcurrentGroupsMeasured: Math.max(...concurrencyLevels),
		maxConcurrentGuestThreadsMeasured:
			Math.max(...concurrencyLevels) * (concurrentWorkerThreadsPerGroup + 1),
		passed:
			startup.every((row) => row.passed) &&
			result.throughput.passed === true &&
			memory.every((row) => row.passed) &&
			concurrency.every((row) => row.passed),
	};
}

function memoryDelta(
	end: ProcessTreeMemorySnapshot,
	start: ProcessTreeMemorySnapshot,
) {
	return {
		rssBytes: end.rssBytes - start.rssBytes,
		pssBytes: end.pssBytes - start.pssBytes,
		virtualBytes: end.virtualBytes - start.virtualBytes,
		minorFaults: end.minorFaults - start.minorFaults,
		majorFaults: end.majorFaults - start.majorFaults,
		processCount: end.processCount - start.processCount,
		threadCount: end.threadCount - start.threadCount,
	};
}

function percentile(values: number[], fraction: number): number {
	if (values.length === 0) return 0;
	const sorted = [...values].sort((left, right) => left - right);
	return sorted[
		Math.min(sorted.length - 1, Math.ceil(sorted.length * fraction) - 1)
	]!;
}

function median(values: number[]): number {
	return percentile(values, 0.5);
}

function integerEnv(name: string, fallback: number): number {
	const raw = process.env[name];
	if (raw === undefined || raw === "") return fallback;
	const parsed = Number(raw);
	if (!Number.isInteger(parsed) || parsed <= 0) {
		throw new Error(`${name} must be a positive integer`);
	}
	return parsed;
}

function listEnv(name: string, fallback: number[]): number[] {
	const raw = process.env[name];
	if (raw === undefined || raw === "") return fallback;
	const parsed = raw.split(",").map((value) => Number(value.trim()));
	if (
		parsed.length === 0 ||
		parsed.some((value) => !Number.isInteger(value) || value <= 0)
	) {
		throw new Error(
			`${name} must be a comma-separated list of positive integers`,
		);
	}
	return parsed;
}

function requiredSidecarPid(vm: BenchVm): number {
	const pid = vm.sidecarPid();
	if (pid === null) throw new Error("benchmark could not resolve sidecar PID");
	return pid;
}

function writeCheckpoint(result: Record<string, unknown>): void {
	writeFileSync(outputPath, `${JSON.stringify(result, null, 2)}\n`);
}

function delay(ms: number): Promise<void> {
	return new Promise((resolveDelay) => setTimeout(resolveDelay, ms));
}

async function withTimeout<T>(
	value: Promise<T>,
	timeoutMs: number,
	description: string,
): Promise<T> {
	let timer: NodeJS.Timeout | undefined;
	try {
		return await Promise.race([
			value,
			new Promise<never>((_, reject) => {
				timer = setTimeout(
					() => reject(new Error(`${description} exceeded ${timeoutMs}ms`)),
					timeoutMs,
				);
			}),
		]);
	} finally {
		if (timer) clearTimeout(timer);
	}
}

main().catch((error) => {
	console.error(error);
	process.exitCode = 1;
});
