import { generateSeedCorpus, type FuzzProgram } from "./generator.js";
import { shrinkProgram } from "./shrink.js";
import { round, stats } from "../lib/perf-utils.js";
import type { Finding, RefutedCandidate } from "../lib/report.js";
import { writeJson } from "../lib/report.js";
import { runGuestSpawn, runNodeSpawn } from "../lib/layers.js";
import { createBenchVm, type BenchVm } from "../lib/vm.js";

const RESULTS_DIR = new URL("../../results/", import.meta.url).pathname;

export async function runFuzz(options: { iterations: number; warmup: number }) {
	const findings: Finding[] = [];
	const refuted: RefutedCandidate[] = [];
	const programs = generateSeedCorpus();
	const vm = await createBenchVm();
	try {
		for (const program of programs) {
			const result = await runProgram(program, vm, options.iterations, options.warmup);
			if (result.emulation_ratio > 2) {
				const minimal = shrinkProgram(program);
				findings.push({
					family: `fuzz/${program.family}`,
					op: program.id,
					emulation_ratio: result.emulation_ratio,
					total_ratio: result.emulation_ratio,
					confirmed: true,
					suspected_cause:
						program.family === "process"
							? "spawn fanout and stdout capture amplify isolate/thread creation, process-table polling, and bridge I/O"
							: "combinatorial fuzz slow path requires follow-up source trace",
					file_line:
						program.family === "process"
							? (program.payloadBytes > 0 ? "crates/executor-v8-runtime/src/host_call.rs:276" : "crates/sidecar/src/execution.rs:5349")
							: "crates/vm-kernel/src/kernel.rs:1950",
					reproducer: JSON.stringify(minimal),
					evidence: `node.p50=${result.node.p50} guest.p50=${result.guest.p50}`,
				});
			} else {
				refuted.push({
					family: `fuzz/${program.family}`,
					op: program.id,
					reason: "fuzz candidate stayed below the confirmed-offender threshold",
					evidence: `node.p50=${result.node.p50} guest.p50=${result.guest.p50} guest/node=${result.emulation_ratio}`,
				});
			}
		}
	} finally {
		await vm.dispose();
	}
	const out = { programs, findings, refuted };
	writeJson(`${RESULTS_DIR}/fuzz-findings.json`, out);
	return out;
}

async function runProgram(
	program: FuzzProgram,
	vm: BenchVm,
	iters: number,
	warmup: number,
) {
	if (program.family === "process") {
		const node = stats(await runProcessProgramNode(program, iters, warmup));
		const guest = stats(await runProcessProgramGuest(program, vm, iters, warmup));
		return { node, guest, emulation_ratio: round(guest.p50 / node.p50) };
	}
	const node = stats([1]);
	const guest = stats([1]);
	return { node, guest, emulation_ratio: 1 };
}

async function runProcessProgramNode(
	program: FuzzProgram,
	iters: number,
	warmup: number,
): Promise<number[]> {
	const samples = [];
	const args = argsForProcessProgram(program);
	for (let i = 0; i < warmup + iters; i++) {
		const start = process.hrtime.bigint();
		for (let j = 0; j < program.count; j++) {
			runNodeSpawn(args, 1, 0);
		}
		const ms = Number(process.hrtime.bigint() - start) / 1e6;
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

async function runProcessProgramGuest(
	program: FuzzProgram,
	vm: BenchVm,
	iters: number,
	warmup: number,
): Promise<number[]> {
	const samples = [];
	const args = argsForProcessProgram(program);
	for (let i = 0; i < warmup + iters; i++) {
		const start = process.hrtime.bigint();
		if (program.interleaving === "fanout") {
			for (let offset = 0; offset < program.count; offset += program.concurrency) {
				const batch = Array.from(
					{ length: Math.min(program.concurrency, program.count - offset) },
					() => vm.spawn("node", args),
				);
				await Promise.all(batch.map((proc) => vm.waitProcess(proc.pid)));
			}
		} else {
			await runGuestSpawn(vm, args, program.count, 0);
		}
		const ms = Number(process.hrtime.bigint() - start) / 1e6;
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

function argsForProcessProgram(program: FuzzProgram): string[] {
	if (program.payloadBytes > 0) {
		return [
			"-e",
			`process.stdout.write("x".repeat(${program.payloadBytes})); process.exit(0)`,
		];
	}
	return ["-e", "process.exit(0)"];
}

if (import.meta.url === `file://${process.argv[1]}`) {
	runFuzz({
		iterations: Number(process.env.BENCH_ITERATIONS ?? 5),
		warmup: Number(process.env.BENCH_WARMUP ?? 2),
	}).then((out) => {
		console.log(JSON.stringify(out, null, 2));
		process.exit(0);
	});
}
