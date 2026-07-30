/**
 * Cold-start latency benchmark.
 *
 * Measures time from AgentOs.create() through workload ready:
 *   --workload=sleep            Minimal VM + idle Node.js process (marketing cold-start workload)
 *   --workload=echo             Minimal VM + first exec("echo hello") completing
 *   --workload=pi-session       VM + openSession({ sessionId: "main", agent: "pi" }) completing
 *   --workload=pi-prompt-turn   VM + openSession({ sessionId: "main", agent: "pi-cli" }) + first prompt turn
 *   --workload=claude-session   VM + openSession({ sessionId: "main", agent: "claude" }) completing
 *
 * All VMs lease one shared sidecar process; a cold-run VM is created and
 * snapshotted first so measured iterations reflect warm, incremental cost.
 *
 * `pi-prompt-turn` benchmarks the native PI CLI path through
 * `openSession({ sessionId: "main", agent: "pi-cli" })`, which uses `pi-acp` to drive the real PI CLI in
 * RPC mode. The same PI headless test file documents that raw `spawn("pi", ...)`
 * is still not exposed on the sidecar PATH.
 *
 * Pass --iterations=N to override default (5). The reported p95/p99 are only
 * meaningful with enough samples (~200 for p95, ~1000 for p99); the marketing
 * run uses --iterations=2000.
 *
 * Usage:
 *   pnpm exec tsx scripts/benchmarks/coldstart.bench.ts --workload=sleep --iterations=2000
 *   pnpm exec tsx scripts/benchmarks/coldstart.bench.ts --workload=pi-session --iterations=3
 *   pnpm exec tsx scripts/benchmarks/coldstart.bench.ts --workload=claude-session --iterations=3
 */

import {
	ITERATIONS,
	type WorkloadObservation,
	WARMUP_ITERATIONS,
	WORKLOADS,
	clearBenchRootSnapshot,
	createBenchVm,
	ECHO_COMMAND,
	EXPECTED_OUTPUT,
	getHardware,
	printTable,
	round,
	setBenchRootSnapshot,
	startBenchSidecar,
	stats,
	stopBenchSidecar,
	stopLlmock,
} from "./bench-utils.js";
import { AgentOs } from "@rivet-dev/agentos-core";

const VALID_WORKLOADS = [
	"echo",
	"sleep",
	...Object.keys(WORKLOADS).filter(
		(k) => k.endsWith("-session") || k.endsWith("-turn"),
	),
];

interface Measurement {
	ms: number;
	observation?: WorkloadObservation;
}

async function measureEcho(): Promise<Measurement> {
	const t0 = performance.now();
	const vm = await createBenchVm();
	const result = await vm.exec(ECHO_COMMAND);
	const ms = performance.now() - t0;
	if (result.stdout !== EXPECTED_OUTPUT) {
		throw new Error(`Unexpected output: ${JSON.stringify(result.stdout)}`);
	}
	await vm.dispose();
	return { ms };
}

async function measureAgentSession(workloadName: string): Promise<Measurement> {
	const workload = WORKLOADS[workloadName];
	const t0 = performance.now();
	const vm = await workload.createVm();
	const observation = (await workload.start(vm)) as
		| WorkloadObservation
		| undefined;
	const ms = performance.now() - t0;
	await vm.dispose();
	return { ms, observation };
}

function parseArgs(): { workload: string; iterations: number } {
	const wArg = process.argv.find((a) => a.startsWith("--workload="));
	const iArg = process.argv.find((a) => a.startsWith("--iterations="));

	if (!wArg) {
		console.error(
			`Usage: pnpm exec tsx coldstart.bench.ts --workload=${VALID_WORKLOADS.join("|")} [--iterations=N]`,
		);
		process.exit(1);
	}
	const name = wArg.split("=")[1];
	if (!VALID_WORKLOADS.includes(name)) {
		console.error(`Unknown workload: ${name}. Use: ${VALID_WORKLOADS.join(", ")}`);
		process.exit(1);
	}

	let iterations = ITERATIONS;
	if (iArg) {
		const val = parseInt(iArg.split("=")[1], 10);
		if (!isNaN(val) && val >= 1) iterations = val;
	}

	return { workload: name, iterations };
}

/**
 * Cold run: create one VM up front, snapshot its root filesystem, and record
 * the snapshot so subsequent measured VMs boot from it instead of paying the
 * bootstrap cost on every iteration.
 */
async function coldRunSnapshot(workloadName: string): Promise<void> {
	let vm: AgentOs;
	if (workloadName === "echo") {
		vm = await createBenchVm();
		await vm.exec(ECHO_COMMAND);
	} else {
		const workload = WORKLOADS[workloadName];
		vm = await workload.createVm();
		await workload.start(vm);
	}
	try {
		setBenchRootSnapshot(await vm.snapshotRootFilesystem());
	} finally {
		await vm.dispose();
	}
}

async function main() {
	const { workload, iterations } = parseArgs();
	const measure = workload === "echo"
		? measureEcho
		: () => measureAgentSession(workload);

	const hardware = getHardware();
	console.error(`=== Cold-Start Benchmark (${workload}) ===`);
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(`Iterations: ${iterations} (+ ${WARMUP_ITERATIONS} warmup)`);

	// Create the sidecar once up front, then a single cold-run VM whose root
	// filesystem snapshot warms every subsequent measured VM.
	await startBenchSidecar();
	clearBenchRootSnapshot();
	console.error("cold run: snapshotting root filesystem...");
	await coldRunSnapshot(workload);

	const samples: number[] = [];
	let lastObservation: WorkloadObservation | undefined;

	for (let i = 0; i < WARMUP_ITERATIONS + iterations; i++) {
		const { ms, observation } = await measure();
		if (i >= WARMUP_ITERATIONS) {
			samples.push(ms);
			if (observation) {
				lastObservation = observation;
			}
		}
		console.error(
			`  iter ${i}: ${round(ms)}ms${i < WARMUP_ITERATIONS ? " (warmup)" : ""}`,
		);
	}

	const s = stats(samples);

	printTable(
		["metric", "mean", "p50", "p95", "min", "max"],
		[["cold start", `${s.mean}ms`, `${s.p50}ms`, `${s.p95}ms`, `${s.min}ms`, `${s.max}ms`]],
	);

	if (lastObservation) {
		console.error(
			`observed work: providerRequests=${lastObservation.providerRequestCount ?? 0} textEvents=${lastObservation.textEventCount ?? 0} stopReason=${lastObservation.stopReason ?? "n/a"}`,
		);
		if (lastObservation.finalText) {
			console.error(`final text: ${JSON.stringify(lastObservation.finalText)}`);
		}
	}

	console.log(
		JSON.stringify(
			{
				hardware,
				workload,
				iterations,
				coldStart: s,
				observation: lastObservation,
			},
			null,
			2,
		),
	);

	await stopBenchSidecar();
	await stopLlmock();
}

main().catch((err) => {
	console.error(err);
	process.exit(1);
});
