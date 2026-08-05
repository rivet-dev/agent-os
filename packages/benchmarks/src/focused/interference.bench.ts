import { createBenchVm, formatSidecarProvenance, type BenchVm } from "../lib/vm.js";
import { getHardware, printTable, round, stats, type Stats } from "../lib/perf-utils.js";
import {
	assertMatrixBallpark,
	benchmarkProvenance,
	busyInterferenceProgram,
	envNumber,
	fsWriteSmallLoopProgram,
	participantFromLoop,
	prewarmConcurrencyVm,
	runGuestJsonProgram,
	writeGuestProgram,
	type RegressionRow,
} from "./concurrency-common.js";

const PROBE_PROGRAM_PATH = "/tmp/focused-interference-probe.mjs";
const BUSY_PROGRAM_PATH = "/tmp/focused-interference-busy.mjs";

interface ProbeCase {
	mode: "idle" | "busy";
	ops: number;
	opsPerSec: number;
	durationMs: number;
	latencyMs: Stats;
	rawSampleCount: number;
	busyOps?: number;
}

async function runProbe(vm: BenchVm, durationMs: number, mode: ProbeCase["mode"]): Promise<ProbeCase> {
	const loop = await runGuestJsonProgram(vm, PROBE_PROGRAM_PATH, {
		BENCH_DURATION_MS: String(durationMs),
		BENCH_PROCESS_INDEX: mode,
	});
	const participant = await participantFromLoop(0, vm, loop);
	return {
		mode,
		ops: participant.ops,
		opsPerSec: participant.opsPerSec,
		durationMs: participant.durationMs,
		latencyMs: participant.latencyMs,
		rawSampleCount: participant.rawSampleCount,
	};
}

async function main(): Promise<void> {
	const durationMs = envNumber("BENCH_INTERFERENCE_DURATION_MS", 5_000);
	const busyDurationMs = envNumber("BENCH_INTERFERENCE_BUSY_DURATION_MS", durationMs + 1_000);
	const hardware = getHardware();
	const provenance = benchmarkProvenance();
	console.error("=== Interference Focused Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(formatSidecarProvenance(provenance.sidecar));
	console.error(`Probe duration: ${durationMs}ms | busy duration: ${busyDurationMs}ms`);

	const probeVm = await createBenchVm();
	const busyVm = await createBenchVm();
	try {
		await Promise.all([prewarmConcurrencyVm(probeVm), prewarmConcurrencyVm(busyVm)]);
		await Promise.all([
			writeGuestProgram(probeVm, PROBE_PROGRAM_PATH, fsWriteSmallLoopProgram()),
			writeGuestProgram(busyVm, BUSY_PROGRAM_PATH, busyInterferenceProgram()),
		]);

		const idle = await runProbe(probeVm, durationMs, "idle");
		assertMatrixBallpark("fs_write_small", idle.latencyMs.p50, {
			multiplier: 5,
			fallbackCeilingMs: 5,
		});

		const busyPromise = runGuestJsonProgram(busyVm, BUSY_PROGRAM_PATH, {
			BENCH_DURATION_MS: String(busyDurationMs),
		});
		const busy = await runProbe(probeVm, durationMs, "busy");
		const busyLoop = await busyPromise;
		busy.busyOps = busyLoop.ops;

		const interferenceTax = {
			p50: round(busy.latencyMs.p50 / idle.latencyMs.p50, 2),
			p95: round(busy.latencyMs.p95 / idle.latencyMs.p95, 2),
		};
		const regressionRows: RegressionRow[] = [
			{
				rowKey: "interference.idle.fs_write_small_p50_ms",
				metric: "idleP50Ms",
				value: idle.latencyMs.p50,
				unit: "ms",
			},
			{
				rowKey: "interference.busy.fs_write_small_p50_ms",
				metric: "busyP50Ms",
				value: busy.latencyMs.p50,
				unit: "ms",
			},
			{
				rowKey: "interference.fs_write_small_p50_tax",
				metric: "interferenceTaxP50",
				value: interferenceTax.p50,
				unit: "ratio",
			},
			{
				rowKey: "interference.fs_write_small_p95_tax",
				metric: "interferenceTaxP95",
				value: interferenceTax.p95,
				unit: "ratio",
			},
		];

		printTable(
			["mode", "ops/s", "p50", "p95", "samples", "busy ops"],
			[idle, busy].map((row) => [
				row.mode,
				row.opsPerSec,
				`${row.latencyMs.p50}ms`,
				`${row.latencyMs.p95}ms`,
				row.rawSampleCount,
				row.busyOps ?? "n/a",
			]),
		);
		console.error(
			`  interferenceTax: p50=${interferenceTax.p50}x p95=${interferenceTax.p95}x`,
		);

		console.log(
			JSON.stringify(
				{
					benchmark: "interference",
					...provenance,
					hardware,
					probeOp: "fs_write_small",
					busyOp: "cpu_spin_plus_fs_write_churn",
					durationMs,
					busyDurationMs,
					idle,
					busy,
					interferenceTax,
					probeVmResourceSnapshot: await probeVm.getResourceSnapshot(),
					busyVmResourceSnapshot: await busyVm.getResourceSnapshot(),
					regressionRows,
				},
				null,
				2,
			),
		);
	} finally {
		await Promise.allSettled([probeVm.dispose(), busyVm.dispose()]);
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
