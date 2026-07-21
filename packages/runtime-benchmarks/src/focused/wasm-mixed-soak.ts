import { writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { cpus, hostname, totalmem } from "node:os";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import type { NodeRuntimeResourceSnapshot } from "@rivet-dev/agentos-runtime-core";
import {
	type ProcessTreeMemorySnapshot,
	readProcessTreeMemorySnapshot,
} from "../lib/memory.js";
import {
	type BenchVm,
	createBenchSidecar,
	createBenchVm,
	formatSidecarProvenance,
	resolveBenchSidecarProvenance,
} from "../lib/vm.js";

const here = dirname(fileURLToPath(import.meta.url));
const outputPath = resolve(
	process.env.AGENTOS_WASM_MIXED_SOAK_OUTPUT ??
		join(here, "../../results/wasm-mixed-soak.json"),
);
const vmCount = integerEnv("AGENTOS_WASM_MIXED_SOAK_VMS", 1);
const warmupCycles = integerEnv("AGENTOS_WASM_MIXED_SOAK_WARMUP", 5);
const measuredCycles = integerEnv("AGENTOS_WASM_MIXED_SOAK_CYCLES", 20);
const settleMs = integerEnv("AGENTOS_WASM_MIXED_SOAK_SETTLE_MS", 100);
const operationTimeoutMs = integerEnv(
	"AGENTOS_WASM_MIXED_SOAK_OPERATION_TIMEOUT_MS",
	30_000,
);
const maxRssGrowthBytes = integerEnv(
	"AGENTOS_WASM_MIXED_SOAK_MAX_RSS_GROWTH_BYTES",
	48 * 1024 * 1024,
);
const maxPssGrowthBytes = integerEnv(
	"AGENTOS_WASM_MIXED_SOAK_MAX_PSS_GROWTH_BYTES",
	48 * 1024 * 1024,
);
const sidecarProvenance = resolveBenchSidecarProvenance();
const guestProgramPath = "/tmp/mixed-soak.mjs";

const guestProgram = `import { spawn } from "node:child_process";

const response = await fetch(process.env.SOAK_URL, { signal: AbortSignal.timeout(5000) });
const body = await response.text();
if (!response.ok || !body.startsWith("mixed-http:")) {
  throw new Error(\`unexpected fetch response \${response.status}: \${body}\`);
}

const tag = process.env.SOAK_TAG;
const childResult = await new Promise((resolve, reject) => {
  const child = spawn("printf", ["%s\\n", tag], {
    env: { ...process.env, SOAK_TAG: tag },
  });
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => { stdout += chunk; });
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  child.on("error", reject);
  child.on("close", (code) => resolve({ code, stdout, stderr }));
});
if (childResult.code !== 0 || childResult.stdout !== \`\${tag}\\n\`) {
  throw new Error(\`child shell failed: \${JSON.stringify(childResult)}\`);
}
process.stdout.write(JSON.stringify({ tag, body, child: childResult.stdout.trim() }));
`;

interface SoakSample {
	cycle: number;
	elapsedMs: number;
	memory: ProcessTreeMemorySnapshot;
	resources: AggregateResources;
}

interface AggregateResources {
	runningProcesses: number;
	stoppedProcesses: number;
	exitedProcesses: number;
	fdTables: number;
	openFds: number;
	pipes: number;
	ptys: number;
	sockets: number;
	socketListeners: number;
	socketConnections: number;
	wasmReservedMemoryBytes: number;
	wasmtimeEngineProfiles: number;
	wasmtimeModuleEntries: number;
	wasmtimeChargedModuleBytes: number;
	queueDepth: number;
}

async function main(): Promise<void> {
	if (process.platform !== "linux") {
		throw new Error(
			"mixed WASM/V8 soak memory validation requires Linux /proc",
		);
	}
	if (sidecarProvenance.profile !== "release") {
		throw new Error(
			`mixed soak requires a release sidecar, got ${formatSidecarProvenance(sidecarProvenance)}`,
		);
	}

	const hostServer = createServer((request, response) => {
		response.writeHead(200, {
			"content-type": "text/plain",
			connection: "close",
		});
		response.end(`mixed-http:${request.url ?? "/"}`);
	});
	await new Promise<void>((resolveListen, rejectListen) => {
		hostServer.once("error", rejectListen);
		hostServer.listen(0, "127.0.0.1", () => resolveListen());
	});
	const address = hostServer.address();
	if (!address || typeof address === "string") {
		throw new Error("mixed soak could not resolve loopback server port");
	}

	const sidecar = createBenchSidecar();
	const vms: BenchVm[] = [];
	const started = performance.now();
	try {
		for (let index = 0; index < vmCount; index++) {
			const vm = await createBenchVm({
				sidecar,
				wasmBackend: "wasmtime",
				loopbackExemptPorts: [address.port],
			});
			await vm.writeFile(guestProgramPath, guestProgram);
			vms.push(vm);
		}

		for (let cycle = 0; cycle < warmupCycles; cycle++) {
			await runCycle(vms, address.port, `warm-${cycle}`);
		}
		await waitForDrain(vms);
		await delay(Math.max(settleMs, 500));

		const sidecarPid = vms[0]?.sidecarPid();
		if (sidecarPid === null || sidecarPid === undefined) {
			throw new Error("mixed soak could not resolve the shared sidecar PID");
		}
		const baseline = await takeSample(vms, sidecarPid, -1, started);
		const samples: SoakSample[] = [];
		const checkpoint = baseReport(baseline, samples);
		writeCheckpoint(checkpoint);

		for (let cycle = 0; cycle < measuredCycles; cycle++) {
			await runCycle(vms, address.port, `cycle-${cycle}`);
			await waitForDrain(vms);
			await delay(settleMs);
			samples.push(await takeSample(vms, sidecarPid, cycle, started));
			writeCheckpoint(baseReport(baseline, samples));
		}

		const signalProbe = await Promise.all(vms.map((vm) => runSignalProbe(vm)));
		await waitForDrain(vms);
		const postSignal = await takeSample(
			vms,
			sidecarPid,
			measuredCycles,
			started,
		);
		const plateau = summarize(baseline, samples);
		const signalsPassed = signalProbe.every(
			(result) =>
				result.wasmExitCode >= 128 && result.javascriptExitCode >= 128,
		);
		const summary = {
			...plateau,
			signalsPassed,
			passed: plateau.passed && signalsPassed,
		};
		const report = {
			...baseReport(baseline, samples),
			completedAt: new Date().toISOString(),
			status: summary.passed ? "complete" : "failed",
			signalProbe,
			postSignal,
			summary,
		};
		writeCheckpoint(report);
		console.log(JSON.stringify(summary, null, 2));
		console.error(`raw results: ${outputPath}`);
		if (!summary.passed) {
			throw new Error("mixed V8-JavaScript/Wasmtime soak did not plateau");
		}
	} finally {
		await Promise.allSettled(vms.map((vm) => vm.dispose()));
		await sidecar.dispose();
		await new Promise<void>((resolveClose, rejectClose) => {
			hostServer.close((error) =>
				error ? rejectClose(error) : resolveClose(),
			);
		});
	}
}

async function runCycle(
	vms: BenchVm[],
	port: number,
	cycle: string,
): Promise<void> {
	await withTimeout(
		Promise.all(
			vms.map((vm, index) => runVmCycle(vm, port, `${cycle}-vm${index}`)),
		),
		operationTimeoutMs * 4,
		`mixed workload ${cycle}`,
	);
}

async function runVmCycle(
	vm: BenchVm,
	port: number,
	tag: string,
): Promise<void> {
	const url = `http://127.0.0.1:${port}/${tag}`;
	const guestResult = await withTimeout(
		runGuest(vm, tag, url),
		operationTimeoutMs,
		`V8 guest ${tag}`,
	);
	let guestPayload: { tag?: string; body?: string; child?: string };
	try {
		guestPayload = JSON.parse(guestResult);
	} catch {
		throw new Error(
			`V8 mixed guest returned invalid JSON for ${tag}: ${guestResult}`,
		);
	}
	if (
		guestPayload.tag !== tag ||
		guestPayload.child !== tag ||
		guestPayload.body !== `mixed-http:/${tag}`
	) {
		throw new Error(
			`V8 guest/Wasmtime child affinity failed for ${tag}: ${JSON.stringify(guestPayload)}`,
		);
	}
	await delay(settleMs);

	const directory = `/tmp/${tag}`;
	await runWasmCommand(vm, "mkdir", ["-p", directory], tag);
	const pipelineResult = await runWasmCommand(
		vm,
		"sh",
		[
			"-c",
			'printf "%s\\n" "$1" > "$2/value"; cat "$2/value" | tr "[:lower:]" "[:upper:]"',
			"sh",
			tag,
			directory,
		],
		tag,
	);
	if (pipelineResult.stdout !== `${tag.toUpperCase()}\n`) {
		throw new Error(
			`Wasmtime child pipeline returned unexpected output for ${tag}: ${JSON.stringify(pipelineResult)}`,
		);
	}
	const filesystemResult = await runWasmCommand(vm, "ls", [directory], tag);
	if (!filesystemResult.stdout.includes("value")) {
		throw new Error(`Wasmtime filesystem workload lost ${directory}/value`);
	}
	await runWasmCommand(vm, "rm", ["-rf", directory], tag);
	const networkResult = await withTimeout(
		vm.execArgv("curl", ["--max-time", "5", "-fsS", url], {
			timeout: operationTimeoutMs,
		}),
		operationTimeoutMs,
		`Wasmtime network ${tag}`,
	);
	if (
		networkResult.exitCode !== 0 ||
		!networkResult.stdout.includes(`mixed-http:/${tag}`)
	) {
		throw new Error(
			`Wasmtime network workload failed for ${tag}: exit=${networkResult.exitCode} stdout=${networkResult.stdout} stderr=${networkResult.stderr}`,
		);
	}
}

async function runWasmCommand(
	vm: BenchVm,
	command: string,
	args: string[],
	tag: string,
): Promise<{ stdout: string; stderr: string; exitCode: number }> {
	const result = await withTimeout(
		vm.execArgv(command, args, { timeout: operationTimeoutMs }),
		operationTimeoutMs,
		`Wasmtime ${command} ${tag}`,
	);
	if (result.exitCode !== 0) {
		throw new Error(
			`Wasmtime ${command} failed for ${tag}: exit=${result.exitCode} stdout=${result.stdout} stderr=${result.stderr}`,
		);
	}
	return result;
}

async function runSignalProbe(vm: BenchVm): Promise<{
	wasmExitCode: number;
	javascriptExitCode: number;
}> {
	const wasm = vm.spawn("sleep", ["30"]);
	const javascript = vm.spawn("node", ["-e", "setInterval(() => {}, 1000)"]);
	await delay(50);
	wasm.kill("SIGTERM");
	javascript.kill("SIGTERM");
	const [wasmExitCode, javascriptExitCode] = await Promise.all([
		withTimeout(wasm.wait(), operationTimeoutMs, "Wasmtime signal probe"),
		withTimeout(javascript.wait(), operationTimeoutMs, "V8 signal probe"),
	]);
	return { wasmExitCode, javascriptExitCode };
}

async function runGuest(
	vm: BenchVm,
	tag: string,
	url: string,
): Promise<string> {
	let stdout = "";
	let stderr = "";
	const process = vm.spawn("node", [guestProgramPath], {
		env: { SOAK_TAG: tag, SOAK_URL: url },
		onStdout(data) {
			stdout += Buffer.from(data).toString("utf8");
		},
		onStderr(data) {
			stderr += Buffer.from(data).toString("utf8");
		},
	});
	const exitCode = await process.wait();
	if (exitCode !== 0) {
		throw new Error(`mixed V8 guest exited ${exitCode}: ${stderr}`);
	}
	return stdout;
}

async function waitForDrain(vms: BenchVm[]): Promise<void> {
	const started = performance.now();
	for (;;) {
		const snapshots = await Promise.all(
			vms.map((vm) => vm.getResourceSnapshot()),
		);
		if (
			snapshots.every(
				(snapshot) =>
					snapshot.runningProcesses === 0 &&
					snapshot.stoppedProcesses === 0 &&
					snapshot.wasmReservedMemoryBytes === 0,
			)
		) {
			return;
		}
		if (performance.now() - started > operationTimeoutMs) {
			throw new Error(
				`mixed workload resources did not drain: ${JSON.stringify(snapshots)}`,
			);
		}
		await delay(10);
	}
}

async function takeSample(
	vms: BenchVm[],
	sidecarPid: number,
	cycle: number,
	started: number,
): Promise<SoakSample> {
	return {
		cycle,
		elapsedMs: performance.now() - started,
		memory: readProcessTreeMemorySnapshot(sidecarPid),
		resources: aggregateResources(
			await Promise.all(vms.map((vm) => vm.getResourceSnapshot())),
		),
	};
}

function aggregateResources(
	snapshots: NodeRuntimeResourceSnapshot[],
): AggregateResources {
	const sum = (key: keyof NodeRuntimeResourceSnapshot) =>
		snapshots.reduce((total, snapshot) => total + Number(snapshot[key]), 0);
	const max = (key: keyof NodeRuntimeResourceSnapshot) =>
		Math.max(0, ...snapshots.map((snapshot) => Number(snapshot[key])));
	return {
		runningProcesses: sum("runningProcesses"),
		stoppedProcesses: sum("stoppedProcesses"),
		exitedProcesses: sum("exitedProcesses"),
		fdTables: sum("fdTables"),
		openFds: sum("openFds"),
		pipes: sum("pipes"),
		ptys: sum("ptys"),
		sockets: sum("sockets"),
		socketListeners: sum("socketListeners"),
		socketConnections: sum("socketConnections"),
		wasmReservedMemoryBytes: sum("wasmReservedMemoryBytes"),
		wasmtimeEngineProfiles: max("wasmtimeEngineProfiles"),
		wasmtimeModuleEntries: max("wasmtimeModuleEntries"),
		wasmtimeChargedModuleBytes: max("wasmtimeChargedModuleBytes"),
		queueDepth: snapshots.reduce(
			(total, snapshot) =>
				total +
				snapshot.queueSnapshots
					.filter(
						(queue) =>
							queue.category === "queue" &&
							queue.name !== "sidecar_stdin_frames",
					)
					.reduce((sum, queue) => sum + queue.depth, 0),
			0,
		),
	};
}

function summarize(baseline: SoakSample, samples: SoakSample[]) {
	const windowSize = Math.max(1, Math.floor(samples.length / 4));
	const first = samples.slice(0, windowSize);
	const last = samples.slice(-windowSize);
	const drift = (key: "rssBytes" | "pssBytes") =>
		median(last.map((sample) => sample.memory[key])) -
		median(first.map((sample) => sample.memory[key]));
	const rssGrowthBytes = drift("rssBytes");
	const pssSupported = samples.some((sample) => sample.memory.pssBytes > 0);
	const pssGrowthBytes = pssSupported ? drift("pssBytes") : 0;
	const retainedResourceKeys: Array<keyof AggregateResources> = [
		"runningProcesses",
		"stoppedProcesses",
		"exitedProcesses",
		"fdTables",
		"openFds",
		"pipes",
		"ptys",
		"sockets",
		"socketListeners",
		"socketConnections",
		"wasmReservedMemoryBytes",
		"wasmtimeEngineProfiles",
		"wasmtimeModuleEntries",
		"wasmtimeChargedModuleBytes",
		"queueDepth",
	];
	const resourceGrowth = Object.fromEntries(
		retainedResourceKeys.map((key) => [
			key,
			Math.max(...last.map((sample) => sample.resources[key])) -
				baseline.resources[key],
		]),
	) as Record<keyof AggregateResources, number>;
	const processGrowth =
		Math.max(...last.map((sample) => sample.memory.processCount)) -
		baseline.memory.processCount;
	const threadGrowth =
		Math.max(...last.map((sample) => sample.memory.threadCount)) -
		baseline.memory.threadCount;
	const resourcesPassed = retainedResourceKeys.every(
		(key) => resourceGrowth[key] <= 0,
	);
	return {
		measuredCycles: samples.length,
		windowSize,
		rssGrowthBytes,
		maxRssGrowthBytes,
		pssSupported,
		pssGrowthBytes,
		maxPssGrowthBytes,
		processGrowth,
		threadGrowth,
		resourceGrowth,
		resourcesPassed,
		passed:
			samples.length === measuredCycles &&
			rssGrowthBytes <= maxRssGrowthBytes &&
			(!pssSupported || pssGrowthBytes <= maxPssGrowthBytes) &&
			processGrowth <= 0 &&
			threadGrowth <= 0 &&
			resourcesPassed,
	};
}

function baseReport(baseline: SoakSample, samples: SoakSample[]) {
	return {
		benchmark: "wasm-mixed-soak",
		status: "running",
		metadata: {
			startedAt: new Date(Date.now() - performance.now()).toISOString(),
			hostname: hostname(),
			platform: process.platform,
			arch: process.arch,
			cpuModel: cpus()[0]?.model ?? "unknown",
			logicalCpus: cpus().length,
			totalMemoryBytes: totalmem(),
			node: process.version,
			sidecar: sidecarProvenance,
			wasmBackend: "wasmtime",
			javascriptBackend: "v8",
			vmCount,
			warmupCycles,
			measuredCycles,
			settleMs,
			operationTimeoutMs,
			maxRssGrowthBytes,
			maxPssGrowthBytes,
		},
		baseline,
		samples,
	};
}

function median(values: number[]): number {
	if (values.length === 0) return 0;
	const sorted = [...values].sort((left, right) => left - right);
	return sorted[Math.floor((sorted.length - 1) / 2)] ?? 0;
}

function writeCheckpoint(report: unknown): void {
	writeFileSync(outputPath, `${JSON.stringify(report, null, 2)}\n`);
}

function integerEnv(name: string, fallback: number): number {
	const raw = process.env[name];
	if (raw === undefined || raw === "") return fallback;
	const value = Number(raw);
	if (!Number.isInteger(value) || value <= 0) {
		throw new Error(`${name} must be a positive integer`);
	}
	return value;
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
	process.exit(1);
});
