import { createBenchVm, formatSidecarProvenance } from "../lib/vm.js";
import { getHardware, printTable } from "../lib/perf-utils.js";
import {
	assertMatrixBallpark,
	benchmarkProvenance,
	buildConcurrencyRow,
	concurrencyRegressionRows,
	envNumber,
	fsWriteSmallLoopProgram,
	parseCounts,
	participantFromLoop,
	prewarmConcurrencyVm,
	runGuestJsonProgram,
	writeGuestProgram,
	type ConcurrencyRow,
} from "./concurrency-common.js";

const PROGRAM_PATH = "/tmp/focused-concurrent-processes.mjs";

async function main(): Promise<void> {
	const counts = parseCounts(process.env.BENCH_PROCESS_COUNTS);
	const durationMs = envNumber("BENCH_PROCESS_DURATION_MS", 5_000);
	const hardware = getHardware();
	const provenance = benchmarkProvenance();
	console.error("=== Concurrent Processes Focused Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(formatSidecarProvenance(provenance.sidecar));
	console.error(`Counts: ${counts.join(",")} | duration: ${durationMs}ms`);

	const vm = await createBenchVm();
	try {
		await prewarmConcurrencyVm(vm);
		await writeGuestProgram(vm, PROGRAM_PATH, fsWriteSmallLoopProgram());
		const rows: ConcurrencyRow[] = [];
		let baselineOpsPerSec: number | undefined;
		for (const n of counts) {
			const loops = await Promise.all(
				Array.from({ length: n }, (_, index) =>
					runGuestJsonProgram(vm, PROGRAM_PATH, {
						BENCH_DURATION_MS: String(durationMs),
						BENCH_PROCESS_INDEX: String(index),
					}),
				),
			);
			const participants = await Promise.all(
				loops.map((loop, index) => participantFromLoop(index, vm, loop)),
			);
			const provisional = buildConcurrencyRow(n, durationMs, participants, 1);
			if (baselineOpsPerSec === undefined) {
				baselineOpsPerSec = provisional.aggregateOpsPerSec;
			}
			const row = buildConcurrencyRow(n, durationMs, participants, baselineOpsPerSec);
			if (n === 1) {
				assertMatrixBallpark("fs_write_small", row.meanP50Ms, {
					multiplier: 5,
					fallbackCeilingMs: 5,
				});
			}
			rows.push(row);
			console.error(
				`  N=${n}: ops/s=${row.aggregateOpsPerSec} mean.p95=${row.meanP95Ms}ms scaling=${row.scaling.measuredOfIdeal}x ideal`,
			);
		}

		printTable(
			["N", "ops/s", "mean p50", "mean p95", "max p95", "vs N=1", "of ideal"],
			rows.map((row) => [
				row.n,
				row.aggregateOpsPerSec,
				`${row.meanP50Ms}ms`,
				`${row.meanP95Ms}ms`,
				`${row.maxP95Ms}ms`,
				`${row.scaling.throughputVsN1}x`,
				`${row.scaling.measuredOfIdeal}x`,
			]),
		);

		console.log(
			JSON.stringify(
				{
					benchmark: "concurrent-processes",
					...provenance,
					hardware,
					op: "fs_write_small",
					counts,
					durationMs,
					rows,
					vmResourceSnapshot: await vm.getResourceSnapshot(),
					regressionRows: concurrencyRegressionRows("concurrent-processes", rows),
				},
				null,
				2,
			),
		);
	} finally {
		await vm.dispose();
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
