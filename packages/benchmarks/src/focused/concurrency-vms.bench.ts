import { createBenchVm, formatSidecarProvenance, type BenchVm } from "../lib/vm.js";
import { getHardware, printTable } from "../lib/perf-utils.js";
import {
	assertMatrixBallpark,
	benchmarkProvenance,
	buildConcurrencyRow,
	concurrencyRegressionRows,
	envNumber,
	parseCounts,
	participantFromLoop,
	prewarmConcurrencyVm,
	runGuestJsonProgram,
	tcpEchoSmallLoopProgram,
	writeGuestProgram,
	type ConcurrencyRow,
} from "./concurrency-common.js";

const PROGRAM_PATH = "/tmp/focused-concurrency-vms.mjs";

async function runRow(n: number, durationMs: number): Promise<ConcurrencyRow> {
	const vms: BenchVm[] = [];
	try {
		for (let i = 0; i < n; i++) {
			const vm = await createBenchVm();
			await prewarmConcurrencyVm(vm);
			await writeGuestProgram(vm, PROGRAM_PATH, tcpEchoSmallLoopProgram());
			vms.push(vm);
		}
		const loops = await Promise.all(
			vms.map((vm, index) =>
				runGuestJsonProgram(vm, PROGRAM_PATH, {
					BENCH_DURATION_MS: String(durationMs),
					BENCH_PROCESS_INDEX: String(index),
				}),
			),
		);
		const participants = await Promise.all(
			loops.map((loop, index) => participantFromLoop(index, vms[index], loop)),
		);
		return buildConcurrencyRow(n, durationMs, participants, 1);
	} finally {
		await Promise.allSettled(vms.map((vm) => vm.dispose()));
	}
}

async function main(): Promise<void> {
	const counts = parseCounts(process.env.BENCH_CONCURRENCY_COUNTS);
	const durationMs = envNumber("BENCH_CONCURRENCY_DURATION_MS", 5_000);
	const hardware = getHardware();
	const provenance = benchmarkProvenance();
	console.error("=== Concurrency VMs Focused Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(formatSidecarProvenance(provenance.sidecar));
	console.error(`Counts: ${counts.join(",")} | duration: ${durationMs}ms`);

	const rows: ConcurrencyRow[] = [];
	let baselineOpsPerSec: number | undefined;
	for (const n of counts) {
		const provisional = await runRow(n, durationMs);
		if (baselineOpsPerSec === undefined) {
			baselineOpsPerSec = provisional.aggregateOpsPerSec;
		}
		const row = buildConcurrencyRow(
			n,
			durationMs,
			provisional.participants,
			baselineOpsPerSec,
		);
		if (n === 1) {
			assertMatrixBallpark("tcp_echo_small", row.meanP50Ms, {
				multiplier: 5,
				fallbackCeilingMs: 100,
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
				benchmark: "concurrency-vms",
				...provenance,
				hardware,
				op: "tcp_echo_small",
				counts,
				durationMs,
				rows,
				regressionRows: concurrencyRegressionRows("concurrency-vms", rows),
			},
			null,
			2,
		),
	);
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
