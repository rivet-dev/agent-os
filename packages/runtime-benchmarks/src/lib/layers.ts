import { execFileSync, spawnSync } from "node:child_process";
import {
	copyFileSync,
	existsSync,
	mkdtempSync,
	mkdirSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import type { NativeOp } from "./native.js";
import { runNativeLayerMeasured } from "./native.js";
import {
	NODE_MEMORY_PROVENANCE,
	SidecarPeakMemorySampler,
	hostPeakMemorySupportReason,
	pageSizeBytes,
	runCommandWithMaxRss,
	type LaneMemory,
} from "./memory.js";
import { nowMs, round, stats, type Stats } from "./perf-utils.js";
import type { BenchVm, BenchVmOptions } from "./vm.js";

const DEFAULT_NATIVE_BASELINE_WASM =
	join(
		fileURLToPath(new URL("../../../..", import.meta.url)),
		"target/wasm32-wasip1/release/agentos-native-baseline.wasm",
	);
const WASM_COMMAND_NAME = "native-baseline";
const WASM_BASE_DIR = "/tmp/native-baseline-wasm";
const WASM_SUPPORTED_OPS = new Set<NativeOp>([
	"fs_stat",
	"fs_stat_x32",
	"fs_write",
	"fs_read",
	"stream_copy",
	"fs_open_close",
	"fs_mkdir_rmdir",
	"fs_rename",
	"fs_readdir",
	"fs_fsync",
	"cpu_loop",
	"sleep_timer",
	"yield_loop",
	"alloc_free",
]);
let wasmCommandDir: string | undefined;

export interface LayerSamples {
	native: number[];
	node: number[];
	guest: number[];
	wasm?: number[];
}

export interface LayerSampleSet {
	samples: number[];
	memory?: LaneMemory;
}

export type LayerStatsEntry = Stats & Partial<LaneMemory>;

export interface LayerStats {
	native?: LayerStatsEntry;
	node: LayerStatsEntry;
	guest: LayerStatsEntry;
	wasm?: LayerStatsEntry;
}

export interface BenchmarkOp {
	family: string;
	name: string;
	nativeOp?: NativeOp;
	nativeArgs?: string[];
	nativeUnsupportedReason?: string;
	wasmUnsupportedReason?: string;
	fileLine: string;
	reproducer: string;
	expectedRatio?: "control";
	setup?: string;
	runNode?: (iters: number, warmup: number) => Promise<number[]> | number[];
	runGuest?: (
		vm: BenchVm,
		iters: number,
		warmup: number,
		context?: unknown,
	) => Promise<number[]>;
	maxIterations?: number;
	maxWarmup?: number;
	prepareVm?: () => Promise<{
		options?: BenchVmOptions;
		context?: unknown;
		cleanup?: () => Promise<void> | void;
	}>;
	program?: string;
}

export interface CommandBenchmarkOp {
	family: string;
	name: string;
	fileLine: string;
	reproducer: string;
	skipReason?: string;
	maxIterations?: number;
	maxWarmup?: number;
	runHostCmd: (iters: number, warmup: number) => Promise<number[]> | number[];
	runVmCmd: (
		vm: BenchVm,
		iters: number,
		warmup: number,
	) => Promise<number[]> | number[];
}

export interface OpResult {
	family: string;
	op: string;
	fileLine: string;
	reproducer: string;
	expectedRatio?: "control";
	layers: LayerStats;
	unsupported?: {
		native?: string;
		wasm?: string;
	};
	tax: {
		emulation: number;
		total?: number;
		wasm?: number;
		mem?: number;
	};
}

export interface CommandOpResult {
	family: string;
	op: string;
	fileLine: string;
	reproducer: string;
	skipped?: true;
	skipReason?: string;
	layers: {
		hostCmd?: LayerStatsEntry;
		vmCmd?: LayerStatsEntry;
	};
	tax: {
		command?: number;
		mem?: number;
	};
}

export type LatencyResult = OpResult | CommandOpResult;

export function isCommandOpResult(result: LatencyResult): result is CommandOpResult {
	return "hostCmd" in result.layers || "skipped" in result;
}

export function isLayerOpResult(result: LatencyResult): result is OpResult {
	return !isCommandOpResult(result);
}

export function supportsWasmLayer(op: NativeOp): boolean {
	return WASM_SUPPORTED_OPS.has(op);
}

export function hasNativeBaselineWasm(): boolean {
	return Boolean(resolveNativeBaselineWasm());
}

export function opSupportsWasmLayer(op: BenchmarkOp): boolean {
	return Boolean(
		op.nativeOp &&
			!op.wasmUnsupportedReason &&
			supportsWasmLayer(op.nativeOp) &&
			hasNativeBaselineWasm(),
	);
}

export function wasmLayerOptions(): BenchVmOptions | undefined {
	const wasm = resolveNativeBaselineWasm();
	if (!wasm) return undefined;
	return {
		wasmCommandDirs: [ensureWasmCommandDir(wasm)],
	};
}

export function timedProgram(operationSource: string, setupSource?: string): string {
	return `
const iters = Number(process.env.BENCH_ITERATIONS || 20);
const warmup = Number(process.env.BENCH_WARMUP || 5);
const samples = [];
const now = () => Number(process.hrtime.bigint()) / 1e6;
const setup = ${setupSource ?? "null"};
const op = ${operationSource};
(async () => {
  if (typeof setup === "function") await setup();
  for (let i = 0; i < warmup + iters; i++) {
    const start = now();
    await op(i);
    const ms = now() - start;
    if (i >= warmup) samples.push(ms);
  }
  process.stdout.write(JSON.stringify({ samples }));
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
`;
}

export function runNodeProgram(
	source: string,
	iters: number,
	warmup: number,
): number[] {
	const dir = mkdtempSync(join(tmpdir(), "agentos-fuzz-perf-node-"));
	const file = join(dir, "bench.mjs");
	try {
		writeFileSync(file, source);
		const stdout = execFileSync("node", [file], {
			encoding: "utf8",
			env: {
				...process.env,
				BENCH_ITERATIONS: String(iters),
				BENCH_WARMUP: String(warmup),
			},
			maxBuffer: 128 * 1024 * 1024,
		});
		return JSON.parse(stdout).samples;
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

export async function runNodeProgramMeasured(
	source: string,
	iters: number,
	warmup: number,
): Promise<LayerSampleSet> {
	const dir = mkdtempSync(join(tmpdir(), "agentos-fuzz-perf-node-"));
	const file = join(dir, "bench.mjs");
	try {
		writeFileSync(file, source);
		const env = {
			...process.env,
			BENCH_ITERATIONS: String(iters),
			BENCH_WARMUP: String(warmup),
		};
		const timed =
			hostPeakMemorySupportReason() === undefined
				? await runCommandWithMaxRss("node", [file], {
						env,
						maxBuffer: 128 * 1024 * 1024,
					})
				: undefined;
		const stdout =
			timed?.stdout ??
			execFileSync("node", [file], {
				encoding: "utf8",
				env,
				maxBuffer: 128 * 1024 * 1024,
			});
		return {
			samples: JSON.parse(stdout).samples,
			...(timed
				? {
						memory: {
							memBytes: await subtractNodeStartupBaseline(timed.maxRssBytes),
							memProvenance: NODE_MEMORY_PROVENANCE,
						},
					}
				: {}),
		};
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

export async function runGuestProgram(
	vm: BenchVm,
	source: string,
	iters: number,
	warmup: number,
	name: string,
): Promise<number[]> {
	const path = `/tmp/fuzz-perf-${name.replace(/[^a-z0-9_-]/gi, "_")}.mjs`;
	await vm.writeFile(path, source);
	const result = await vm.spawnNodeCapture(path, {
		BENCH_ITERATIONS: String(iters),
		BENCH_WARMUP: String(warmup),
	});
	if (result.exitCode !== 0) {
		throw new Error(`guest program ${name} exited ${result.exitCode}\n${result.stderr}`);
	}
	return JSON.parse(result.stdout).samples;
}

export function runNodeSpawn(
	args: string[],
	iters: number,
	warmup: number,
): number[] {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		const start = process.hrtime.bigint();
		const result = spawnSync("node", args, { stdio: "ignore" });
		const ms = nowMs(start);
		if (result.status !== 0) {
			throw new Error(`node spawn exited ${result.status}`);
		}
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

export async function runGuestSpawn(
	vm: BenchVm,
	args: string[],
	iters: number,
	warmup: number,
): Promise<number[]> {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		const start = process.hrtime.bigint();
		const proc = vm.spawn("node", args);
		const code = await vm.waitProcess(proc.pid);
		const ms = nowMs(start);
		if (code !== 0) throw new Error(`guest spawn exited ${code}`);
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

function resolveNativeBaselineWasm(): string | undefined {
	const wasm = process.env.NATIVE_BASELINE_WASM ?? DEFAULT_NATIVE_BASELINE_WASM;
	if (!wasm || !existsSync(wasm)) return undefined;
	return wasm;
}

function ensureWasmCommandDir(wasmPath: string): string {
	if (wasmCommandDir) return wasmCommandDir;
	const dir = mkdtempSync(join(tmpdir(), "agentos-native-baseline-wasm-cmd-"));
	mkdirSync(dir, { recursive: true });
	copyFileSync(wasmPath, join(dir, WASM_COMMAND_NAME));
	wasmCommandDir = dir;
	return wasmCommandDir;
}

export async function runWasmLayer(
	vm: BenchVm,
	nativeOp: NativeOp,
	iters: number,
	warmup: number,
	extraArgs: string[] = [],
): Promise<number[] | undefined> {
	if (!supportsWasmLayer(nativeOp)) return undefined;
	if (!resolveNativeBaselineWasm()) return undefined;
	const result = await vm.execWasmCommand(WASM_COMMAND_NAME, [
		"--op",
		nativeOp,
		"--iters",
		String(iters),
		"--warmup",
		String(warmup),
		"--base-dir",
		WASM_BASE_DIR,
		...extraArgs,
	]);
	if (result.exitCode !== 0) {
		throw new Error(`wasm native-baseline ${nativeOp} exited ${result.exitCode}\n${result.stderr}`);
	}
	const parsed = JSON.parse(result.stdout) as {
		unit?: string;
		samples?: number[];
		unsupported?: boolean;
		op?: string;
	};
	if (parsed.unsupported) {
		throw new Error(`wasm native-baseline unexpectedly returned unsupported for ${nativeOp}`);
	}
	if (parsed.unit !== "ns" || !Array.isArray(parsed.samples)) {
		throw new Error(`wasm native-baseline emitted unexpected output: ${result.stdout}`);
	}
	return parsed.samples.map((ns) => ns / 1e6);
}

export interface OpHostSamples {
	native?: LayerSampleSet;
	node: LayerSampleSet;
}

export interface OpVmSamples {
	guest: LayerSampleSet;
	wasm?: LayerSampleSet;
}

export async function runOpHostLayers(
	op: BenchmarkOp,
	iters: number,
	warmup: number,
): Promise<OpHostSamples> {
	const native = op.nativeOp
		? await runNativeLayerMeasured(op.nativeOp, iters, warmup, op.nativeArgs)
		: undefined;
	const node = op.runNode
		? { samples: await op.runNode(iters, warmup) }
		: await runNodeProgramMeasured(
				timedProgram(op.program ?? "() => {}", op.setup),
				iters,
				warmup,
			);
	return {
		...(native ? { native } : {}),
		node,
	};
}

export async function runOpVmLayers(
	op: BenchmarkOp,
	vm: BenchVm,
	iters: number,
	warmup: number,
	context?: unknown,
): Promise<OpVmSamples> {
	const sampler = SidecarPeakMemorySampler.forVm(vm);
	const guest = await measureWithSidecarPeak(sampler, () =>
		op.runGuest
			? op.runGuest(vm, iters, warmup, context)
			: runGuestProgram(
					vm,
					timedProgram(op.program ?? "() => {}", op.setup),
					iters,
					warmup,
					`${op.family}-${op.name}`,
				),
	);
	const wasm =
		op.nativeOp && !op.wasmUnsupportedReason
			? await measureOptionalWithSidecarPeak(sampler, () =>
					runWasmLayer(vm, op.nativeOp!, iters, warmup, op.nativeArgs),
				)
			: undefined;
	return {
		guest,
		...(wasm ? { wasm } : {}),
	};
}

export function buildOpResult(
	op: BenchmarkOp,
	hostSamples: OpHostSamples,
	vmSamples: OpVmSamples,
): OpResult {
	const layers = {
		...(hostSamples.native ? { native: statsWithMemory(hostSamples.native) } : {}),
		node: statsWithMemory(hostSamples.node),
		guest: statsWithMemory(vmSamples.guest),
		...(vmSamples.wasm ? { wasm: statsWithMemory(vmSamples.wasm) } : {}),
	};
	return {
		family: op.family,
		op: op.name,
		fileLine: op.fileLine,
		reproducer: op.reproducer,
		expectedRatio: op.expectedRatio,
		layers,
		unsupported: {
			...(op.nativeUnsupportedReason ? { native: op.nativeUnsupportedReason } : {}),
			...(op.wasmUnsupportedReason ? { wasm: op.wasmUnsupportedReason } : {}),
		},
		tax: {
			emulation: round(layers.guest.p50 / layers.node.p50),
			...(layers.native ? { total: round(layers.guest.p50 / layers.native.p50) } : {}),
			...(layers.wasm && layers.native
				? { wasm: round(layers.wasm.p50 / layers.native.p50) }
				: {}),
			...(layers.guest.memBytes !== undefined && layers.native?.memBytes !== undefined
				? { mem: round(layers.guest.memBytes / layers.native.memBytes) }
				: {}),
		},
	};
}

export function skippedCommandOpResult(op: CommandBenchmarkOp): CommandOpResult {
	return {
		family: op.family,
		op: op.name,
		fileLine: op.fileLine,
		reproducer: op.reproducer,
		skipped: true,
		skipReason: op.skipReason,
		layers: {},
		tax: {},
	};
}

export async function runCommandHostLayer(
	op: CommandBenchmarkOp,
	iters: number,
	warmup: number,
): Promise<LayerStatsEntry> {
	return statsWithMemory({ samples: await op.runHostCmd(iters, warmup) });
}

export async function runCommandVmLayer(
	op: CommandBenchmarkOp,
	vm: BenchVm,
	iters: number,
	warmup: number,
): Promise<LayerStatsEntry> {
	const sampler = SidecarPeakMemorySampler.forVm(vm);
	const measured = await measureWithSidecarPeak(sampler, () =>
		op.runVmCmd(vm, iters, warmup),
	);
	return statsWithMemory(measured);
}

export function buildCommandOpResult(
	op: CommandBenchmarkOp,
	hostCmd: LayerStatsEntry,
	vmCmd: LayerStatsEntry,
): CommandOpResult {
	return {
		family: op.family,
		op: op.name,
		fileLine: op.fileLine,
		reproducer: op.reproducer,
		layers: { hostCmd, vmCmd },
		tax: {
			command: round(vmCmd.p50 / hostCmd.p50),
			...(vmCmd.memBytes !== undefined && hostCmd.memBytes !== undefined
				? { mem: round(vmCmd.memBytes / hostCmd.memBytes) }
				: {}),
		},
	};
}

let nodeStartupMaxRssBytes: number | undefined;

export async function primeNodeMemoryBaseline(): Promise<number | undefined> {
	if (hostPeakMemorySupportReason()) return undefined;
	if (nodeStartupMaxRssBytes !== undefined) return nodeStartupMaxRssBytes;
	nodeStartupMaxRssBytes = (
		await runCommandWithMaxRss("node", ["-e", ""], {
			maxBuffer: 128 * 1024 * 1024,
		})
	).maxRssBytes;
	return nodeStartupMaxRssBytes;
}

async function subtractNodeStartupBaseline(opMaxRssBytes: number): Promise<number> {
	const baseline = (await primeNodeMemoryBaseline()) ?? 0;
	return Math.max(opMaxRssBytes - baseline, pageSizeBytes());
}

async function measureWithSidecarPeak<T extends number[] | undefined>(
	sampler: SidecarPeakMemorySampler | undefined,
	fn: () => Promise<T> | T,
): Promise<LayerSampleSet> {
	if (!sampler) {
		return { samples: (await fn()) ?? [] };
	}
	const measured = await sampler.measure(fn);
	return {
		samples: measured.value ?? [],
		...(measured.memory ? { memory: measured.memory } : {}),
	};
}

async function measureOptionalWithSidecarPeak(
	sampler: SidecarPeakMemorySampler | undefined,
	fn: () => Promise<number[] | undefined>,
): Promise<LayerSampleSet | undefined> {
	if (!sampler) {
		const samples = await fn();
		return samples ? { samples } : undefined;
	}
	const measured = await sampler.measure(fn);
	if (!measured.value) return undefined;
	return {
		samples: measured.value,
		...(measured.memory ? { memory: measured.memory } : {}),
	};
}

function statsWithMemory(sampleSet: LayerSampleSet): LayerStatsEntry {
	return {
		...stats(sampleSet.samples),
		...(sampleSet.memory ? sampleSet.memory : {}),
	};
}
