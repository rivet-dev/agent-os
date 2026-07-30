import { allOps } from "./families/index.js";
import { ecosystemWasmCommandDirs } from "./families/ecosystem.js";
import {
	buildCommandOpResult,
	buildOpResult,
	isLayerOpResult,
	runCommandHostLayer,
	runCommandVmLayer,
	runOpHostLayers,
	runOpVmLayers,
	skippedCommandOpResult,
	supportsWasmLayer,
	wasmLayerOptions,
	primeNodeMemoryBaseline,
	type BenchmarkOp,
	type CommandBenchmarkOp,
	type LatencyResult,
} from "./lib/layers.js";
import { primeNativeMemoryBaseline } from "./lib/native.js";
import {
	createBenchVm,
	formatSidecarProvenance,
	formatPacificIso,
	prewarmBenchVm,
	resolveBenchSidecarProvenance,
	type BenchVm,
	type BenchVmOptions,
	type SidecarBinaryProvenance,
} from "./lib/vm.js";
import {
	findingsFromLatency,
	permissionPolicyFindings,
	permissionPolicyTaxFromLatency,
	refutedFromLatency,
	writeJson,
} from "./lib/report.js";
import { getHardware, printTable } from "./lib/perf-utils.js";
import {
	SidecarPeakMemorySampler,
	formatBytes,
	hostPeakMemorySupportReason,
	procPeakMemorySupportReason,
} from "./lib/memory.js";
import { runFuzz } from "./fuzz/run.js";
import { runLeakSuite } from "./leak.js";
import { runFootprint } from "./footprint.js";
import { compareBaselineFile } from "./compare-baseline.js";
import { pathToFileURL } from "node:url";

const RESULTS_DIR = new URL("../results/", import.meta.url).pathname;
const ITERATIONS = Number(process.env.BENCH_ITERATIONS ?? 20);
const WARMUP = Number(process.env.BENCH_WARMUP ?? 5);
const FAMILY_FILTER = process.env.BENCH_FAMILIES
	?.split(",")
	.map((family) => family.trim())
	.filter(Boolean);
const OP_FILTER = process.env.BENCH_OP_FILTER
	?.split(",")
	.map((op) => op.trim())
	.filter(Boolean);
const COLD_MODE = process.env.BENCH_COLD === "1";
const SHARED_VM_MODE = process.env.BENCH_SHARED_VM === "1";

interface LatencyMatrixRun {
	results: LatencyResult[];
	sidecar: SidecarBinaryProvenance;
	wallTimeMs: number;
	mode: {
		cold: boolean;
		sharedVm: boolean;
	};
}

export async function runLatencyMatrix(): Promise<LatencyMatrixRun> {
	const start = process.hrtime.bigint();
	const sidecar = resolveBenchSidecarProvenance();
	console.error(formatSidecarProvenance(sidecar));
	if (COLD_MODE) {
		console.error("BENCH_COLD=1: VM prewarm is disabled; samples include first-use VM costs.");
	}
	if (SHARED_VM_MODE) {
		console.error("BENCH_SHARED_VM=1: reusing a VM where op-specific VM options are not required.");
	}
	const wasmOptions = wasmLayerOptions();
	if (!wasmOptions) {
		console.error("vm-wasm lane disabled: native-baseline wasm artifact was not found");
	}
	const commandDirs = ecosystemWasmCommandDirs();
	const baseVmOptions = mergeBenchVmOptions({}, {
		mounts: wasmOptions?.mounts,
		wasmCommandDirs: [
			...(wasmOptions?.wasmCommandDirs ?? []),
			...commandDirs,
		],
	});
	const sharedVm = SHARED_VM_MODE ? await createBenchVm(baseVmOptions) : undefined;
	try {
		const results: LatencyResult[] = [];
		const ops = FAMILY_FILTER
			? allOps.filter((op) => FAMILY_FILTER.includes(op.family))
			: allOps;
		const filteredOps = OP_FILTER
			? ops.filter((op) => OP_FILTER.includes(op.name) || OP_FILTER.includes(`${op.family}/${op.name}`))
			: ops;
		await primeMemoryBaselines(filteredOps);
		await runIdleVmMemorySelfCheck(baseVmOptions);
		for (const op of filteredOps) {
			console.error(`latency ${op.family}/${op.name}`);
			if (!("runHostCmd" in op) && op.nativeOp && supportsWasmLayer(op.nativeOp)) {
				console.error("  wasm lane: guest JS measured first, wasm native-baseline after");
			}
			results.push(await runOneOp(op, baseVmOptions, sharedVm));
		}
		return {
			results,
			sidecar,
			wallTimeMs: Number(process.hrtime.bigint() - start) / 1e6,
			mode: {
				cold: COLD_MODE,
				sharedVm: SHARED_VM_MODE,
			},
		};
	} finally {
		await sharedVm?.dispose();
	}
}

async function main(): Promise<void> {
	const matrix = await runLatencyMatrix();
	const latency = matrix.results;
	const layerLatency = latency.filter(isLayerOpResult);
	const nonPermissionsLatency = layerLatency.filter((result) => result.family !== "permissions");
	const findings = findingsFromLatency(nonPermissionsLatency);
	const refuted = refutedFromLatency(nonPermissionsLatency);
	const permissionPolicyTax = permissionPolicyTaxFromLatency(layerLatency);
	const permissionFindings = permissionPolicyFindings(permissionPolicyTax);
	const resourceSnapshotStubbed = false;
	const fuzz = FAMILY_FILTER
		? { programs: [], findings: [], refuted: [] }
		: await runFuzz({ iterations: ITERATIONS, warmup: WARMUP });
	const leak = FAMILY_FILTER ? { findings: [], streams: [] } : await runLeakSuite();
	const footprint = FAMILY_FILTER
		? { findings: [], components: [] }
		: await runFootprint();
	const findingsJson = {
		generatedAt: formatPacificIso(new Date()),
		hardware: getHardware(),
		sidecar: matrix.sidecar,
		matrixMode: matrix.mode,
		wallTimeMs: matrix.wallTimeMs,
		iterations: ITERATIONS,
		warmup: WARMUP,
		resourceSnapshotStubbed,
		latency,
		permissionPolicyTax,
		fuzz,
		leak,
		footprint,
		findings: [
			...findings,
			...permissionFindings,
			...fuzz.findings,
			...leak.findings,
			...footprint.findings,
		].sort((a, b) => b.emulation_ratio - a.emulation_ratio),
		refuted: [
			...refuted,
			...fuzz.refuted,
			{
				family: "net",
				op: "udp_echo_small",
				reason: "guest UDP datagrams are unsupported in the current kernel-backed V8 bridge",
				evidence: "ERR_NOT_IMPLEMENTED: external UDP datagrams are not yet supported by the kernel-backed V8 bridge",
			},
		],
		critic_gaps: criticGaps(latency, fuzz, leak, footprint),
	};
	writeJson(`${RESULTS_DIR}/latency-matrix.json`, {
		sidecar: matrix.sidecar,
		matrixMode: matrix.mode,
		wallTimeMs: matrix.wallTimeMs,
		latency,
		permissionPolicyTax,
	});
	writeJson(`${RESULTS_DIR}/findings.json`, findingsJson);
	const baselinePath = `${RESULTS_DIR}/baseline/findings-baseline.json`;
	const diff = compareBaselineFile(`${RESULTS_DIR}/findings.json`, baselinePath);
	writeJson(`${RESULTS_DIR}/regression-diff.json`, diff);

	printTable(
		[
			"family",
			"op",
			"native p50",
			"node p50",
			"guest p50",
			"wasm p50",
			"hostCmd p50",
			"vmCmd p50",
			"vm/host",
			"native mem",
			"node mem",
			"guest mem",
			"wasm mem",
			"hostCmd mem",
			"vmCmd mem",
			"memTax",
		],
		latency.map((result) => {
			if (isLayerOpResult(result)) {
				return [
					result.family,
					result.op,
					result.layers.native
						? `${result.layers.native.p50}ms`
						: `unsupported: ${result.unsupported?.native ?? "n/a"}`,
					`${result.layers.node.p50}ms`,
					`${result.layers.guest.p50}ms`,
					result.layers.wasm
						? `${result.layers.wasm.p50}ms`
						: result.unsupported?.wasm
							? `unsupported: ${result.unsupported.wasm}`
							: "-",
					"-",
					"-",
					"-",
					formatBytes(result.layers.native?.memBytes),
					formatBytes(result.layers.node.memBytes),
					formatBytes(result.layers.guest.memBytes),
					formatBytes(result.layers.wasm?.memBytes),
					"-",
					"-",
					result.tax.mem ?? "-",
				];
			}
			return [
				result.family,
				result.op,
				"-",
				"-",
				"-",
				"-",
				result.layers.hostCmd ? `${result.layers.hostCmd.p50}ms` : "-",
				result.layers.vmCmd ? `${result.layers.vmCmd.p50}ms` : "-",
				result.tax.command ?? result.skipReason ?? "-",
				"-",
				"-",
				"-",
				"-",
				formatBytes(result.layers.hostCmd?.memBytes),
				formatBytes(result.layers.vmCmd?.memBytes),
				result.tax.mem ?? "-",
			];
		}),
	);

	if (permissionPolicyTax.length > 0) {
		printTable(
			["op", "allow guest p50", "policy guest p50", "policyTax"],
			permissionPolicyTax.map((row) => [
				row.op,
				`${row.allowP50Ms}ms`,
				`${row.policyP50Ms}ms`,
				row.policyTax,
			]),
		);
	}

	printTable(
		["family", "op", "guest/node", "guest/native", "file:line"],
		findingsJson.findings.map((finding) => [
			finding.family,
			finding.op,
			finding.emulation_ratio,
			finding.total_ratio,
			finding.file_line,
		]),
	);
	console.log(JSON.stringify(findingsJson, null, 2));
}

async function runOneOp(
	op: BenchmarkOp | CommandBenchmarkOp,
	baseVmOptions: BenchVmOptions,
	sharedVm: BenchVm | undefined,
): Promise<LatencyResult> {
	const iterations = Math.min(ITERATIONS, op.maxIterations ?? ITERATIONS);
	const warmup = Math.min(WARMUP, op.maxWarmup ?? WARMUP);
	if ("runHostCmd" in op) {
		if (op.skipReason) return skippedCommandOpResult(op);
		const hostCmd = await runCommandHostLayer(op, iterations, warmup);
		const vmCmd = await withOpVm(op, baseVmOptions, sharedVm, (vm) =>
			runCommandVmLayer(op, vm, iterations, warmup),
		);
		return buildCommandOpResult(op, hostCmd, vmCmd);
	}

	const hostSamples = await runOpHostLayers(op, iterations, warmup);
	const vmSamples = await withOpVm(op, baseVmOptions, sharedVm, (vm, context) =>
		runOpVmLayers(op, vm, iterations, warmup, context),
	);
	return buildOpResult(op, hostSamples, vmSamples);
}

async function withOpVm<T>(
	op: BenchmarkOp | CommandBenchmarkOp,
	baseVmOptions: BenchVmOptions,
	sharedVm: BenchVm | undefined,
	callback: (vm: BenchVm, context?: unknown) => Promise<T>,
): Promise<T> {
	const prepared = "prepareVm" in op && op.prepareVm ? await op.prepareVm() : undefined;
	const options = mergeBenchVmOptions(baseVmOptions, prepared?.options ?? {});
	const canUseSharedVm = sharedVm && !prepared?.options;
	const vm = canUseSharedVm ? sharedVm : await createBenchVm(options);
	try {
		if (!COLD_MODE) {
			await prewarmBenchVm(vm, op);
		}
		return await callback(vm, prepared?.context);
	} finally {
		if (!canUseSharedVm) {
			await vm.dispose();
		}
		await prepared?.cleanup?.();
	}
}

function mergeBenchVmOptions(
	base: BenchVmOptions,
	extra: BenchVmOptions,
): BenchVmOptions {
	return {
		...base,
		...extra,
		mounts: [...(base.mounts ?? []), ...(extra.mounts ?? [])],
		wasmCommandDirs: [
			...(base.wasmCommandDirs ?? []),
			...(extra.wasmCommandDirs ?? []),
		],
		loopbackExemptPorts: [
			...(base.loopbackExemptPorts ?? []),
			...(extra.loopbackExemptPorts ?? []),
		],
	};
}

async function primeMemoryBaselines(
	ops: Array<BenchmarkOp | CommandBenchmarkOp>,
): Promise<void> {
	const hostReason = hostPeakMemorySupportReason();
	if (hostReason) {
		console.error(`host memory columns disabled: ${hostReason}`);
	} else {
		if (ops.some((op) => !("runHostCmd" in op) && op.nativeOp)) {
			const nativeBaseline = await primeNativeMemoryBaseline();
			console.error(`native memory startup baseline: ${formatBytes(nativeBaseline)}`);
		}
		if (ops.some((op) => !("runHostCmd" in op) && !op.runNode)) {
			const nodeBaseline = await primeNodeMemoryBaseline();
			console.error(`node memory startup baseline: ${formatBytes(nodeBaseline)}`);
		}
	}

	const procReason = procPeakMemorySupportReason();
	if (procReason) {
		console.error(`guest/vm memory columns disabled: ${procReason}`);
	}
}

async function runIdleVmMemorySelfCheck(
	baseVmOptions: BenchVmOptions,
): Promise<void> {
	const reason = procPeakMemorySupportReason();
	if (reason) {
		console.error(`idle prewarmed VM memory self-check skipped: ${reason}`);
		return;
	}
	const vm = await createBenchVm(baseVmOptions);
	try {
		await prewarmBenchVm(vm, {
			family: "self_check",
			name: "idle_prewarmed_vm",
			fileLine: "packages/benchmarks/src/run-all.ts",
			reproducer: "prewarm VM, clear_refs=5, wait 2s, read VmHWM - baseline VmRSS",
		});
		const sampler = SidecarPeakMemorySampler.forVm(vm);
		if (!sampler) {
			console.error("idle prewarmed VM memory self-check skipped: sidecar pid unavailable");
			return;
		}
		const memory = await sampler.measureIdle(2_000);
		const warning = memory.memBytes > 2 * 1024 * 1024 ? " WARN >2MiB" : "";
		console.error(
			`idle prewarmed VM op memory self-check: ${formatBytes(memory.memBytes)} (${memory.memBytes} bytes)${warning}`,
		);
	} finally {
		await vm.dispose();
	}
}

function criticGaps(
	latency: LatencyResult[],
	fuzz: Awaited<ReturnType<typeof runFuzz>>,
	leak: { streams: Array<{ idleMs: number }> },
	footprint: { components?: unknown[] },
): string[] {
	const gaps: string[] = [];
	const covered = new Set(
		latency.filter(isLayerOpResult).map((result) => `${result.family}/${result.op}`),
	);
	for (const required of [
		"process/fanout_spawn_8",
		"process/wait_reap_storm_8",
		"fs/readdir_small",
		"dns/resolve_concurrent_4",
		"pipes/backpressure_chunks",
		"control/cpu_loop",
	]) {
		if (!covered.has(required)) gaps.push(`missing fixed op ${required}`);
	}
	gaps.push(
		"unsupported fixed op net/udp_echo_small: guest dgram send returns ERR_NOT_IMPLEMENTED for external UDP datagrams",
	);
	if (!fuzz.findings.some((finding) => finding.op === "fanout-stdout-storm")) {
		gaps.push("fuzz did not confirm the non-P2 stdout fanout slow path");
	}
	if (leak.streams.some((stream) => stream.idleMs < 61_000)) {
		gaps.push("leak suite was run in smoke mode without waiting past 60s ZOMBIE_TTL");
	}
	if (footprint.components?.length === 0) {
		gaps.push("footprint run did not emit component attribution");
	}
	return gaps;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
	main().then(
		() => process.exit(0),
		(error) => {
			console.error(error);
			process.exit(1);
		},
	);
}
