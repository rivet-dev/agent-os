/**
 * Memory overhead benchmark for AgentOS VMs.
 *
 * Staircase approach: warms up caches with a throwaway VM, then adds VMs
 * one at a time, measuring the incremental RSS and heap after each step.
 * The per-VM cost is the average step delta — free of one-time setup noise.
 *
 * Workloads:
 *   --workload=sleep             (default) Minimal VM with idle Node.js process
 *   --workload=pi-session        VM with PI agent session via createSession
 *   --workload=claude-session    VM with Claude agent session via createSession
 *   --workload=codex-session     VM with Codex agent session via createSession
 *
 * Pass --count=N to control how many VMs to add (default 5).
 *
 * Usage:
 *   npx tsx --expose-gc benchmarks/memory.bench.ts
 *   npx tsx --expose-gc benchmarks/memory.bench.ts --workload=pi-session --count=1
 *   npx tsx --expose-gc benchmarks/memory.bench.ts --workload=claude-session --count=1
 */

import type { AgentOs } from "@rivet-dev/agent-os-core";
import { readFileSync, readdirSync } from "node:fs";
import {
	WORKLOADS,
	type Workload,
	forceGC,
	formatBytes,
	getHardware,
	printTable,
	round,
	sleep,
	stopLlmock,
} from "./bench-utils.js";

const DEFAULT_COUNT = 5;
const PAGE_SIZE = 4096; // Linux default

interface StepSample {
	stepIndex: number;
	rssBytes: number;
	heapBytes: number;
}

interface MemoryResult {
	workload: string;
	count: number;
	/** Per-step incremental measurements. */
	steps: StepSample[];
	/** Average incremental RSS per VM (bytes). */
	avgPerVmRssBytes: number;
	/** Average incremental heap per VM (bytes). */
	avgPerVmHeapBytes: number;
	/** RSS reclaimed after disposing all VMs (bytes). */
	reclaimedRssBytes: number;
}

/**
 * Read RSS for a single PID from /proc/[pid]/statm.
 * Returns bytes. Returns 0 if the process no longer exists.
 */
function pidRssBytes(pid: number): number {
	try {
		const statm = readFileSync(`/proc/${pid}/statm`, "utf8");
		const rssPages = parseInt(statm.split(" ")[1], 10);
		return rssPages * PAGE_SIZE;
	} catch {
		return 0;
	}
}

/**
 * Sum RSS across the host process and all descendant processes.
 * The V8 isolate runs as a separate child process (Rust binary), so
 * process.memoryUsage().rss only captures the host + WASM Worker threads.
 * This reads /proc to include the full process tree.
 */
function processTreeRssBytes(): number {
	const hostPid = process.pid;
	let total = pidRssBytes(hostPid);

	// Walk /proc to find children (and their children, etc.)
	const visited = new Set<number>([hostPid]);
	const queue = [hostPid];

	while (queue.length > 0) {
		const parentPid = queue.pop()!;
		try {
			const children = readFileSync(
				`/proc/${parentPid}/task/${parentPid}/children`,
				"utf8",
			).trim();
			if (!children) continue;
			for (const token of children.split(/\s+/)) {
				const childPid = parseInt(token, 10);
				if (!isNaN(childPid) && !visited.has(childPid)) {
					visited.add(childPid);
					total += pidRssBytes(childPid);
					queue.push(childPid);
				}
			}
		} catch {
			// /proc entry may have vanished
		}
	}

	return total;
}

async function sampleMemory(): Promise<{ rss: number; heap: number }> {
	forceGC();
	forceGC();
	await sleep(100);
	return { rss: processTreeRssBytes(), heap: process.memoryUsage().heapUsed };
}

async function measure(
	workload: Workload,
	count: number,
): Promise<MemoryResult> {
	// Warmup: create and destroy one VM to pay one-time costs (module cache, JIT, etc.)
	console.error("  warming up...");
	const warmupVm = await workload.createVm();
	await workload.start(warmupVm);
	await sleep(workload.settleMs);
	await warmupVm.dispose();
	forceGC();
	forceGC();
	await sleep(200);

	// Staircase: add VMs one at a time, measure after each.
	const vms: AgentOs[] = [];
	const steps: StepSample[] = [];

	let prev = await sampleMemory();

	for (let i = 0; i < count; i++) {
		const vm = await workload.createVm();
		await workload.start(vm);
		vms.push(vm);

		await sleep(workload.settleMs);
		workload.verify(vm);
		const cur = await sampleMemory();

		const rssDelta = cur.rss - prev.rss;
		const heapDelta = cur.heap - prev.heap;
		steps.push({ stepIndex: i, rssBytes: rssDelta, heapBytes: heapDelta });

		console.error(
			`    step ${i}: rss=${formatBytes(rssDelta)} heap=${formatBytes(heapDelta)}`,
		);
		prev = cur;
	}

	// Measure reclaim.
	const beforeTeardown = await sampleMemory();
	await Promise.all(vms.map((vm) => vm.dispose()));
	const afterTeardown = await sampleMemory();
	const reclaimed = beforeTeardown.rss - afterTeardown.rss;

	const avgRss =
		steps.reduce((a, s) => a + s.rssBytes, 0) / steps.length;
	const avgHeap =
		steps.reduce((a, s) => a + s.heapBytes, 0) / steps.length;

	return {
		workload: workload.name,
		count,
		steps,
		avgPerVmRssBytes: Math.round(avgRss),
		avgPerVmHeapBytes: Math.round(avgHeap),
		reclaimedRssBytes: Math.round(reclaimed),
	};
}

function parseArgs(): { count: number; workload: Workload } {
	const countArg = process.argv.find((a) => a.startsWith("--count="));
	const workloadArg = process.argv.find((a) => a.startsWith("--workload="));

	let count = DEFAULT_COUNT;
	if (countArg) {
		const val = parseInt(countArg.split("=")[1], 10);
		if (isNaN(val) || val < 1) {
			console.error(`Invalid --count value: ${countArg}`);
			process.exit(1);
		}
		count = val;
	}

	let workload = WORKLOADS.sleep;
	if (workloadArg) {
		const name = workloadArg.split("=")[1];
		if (!WORKLOADS[name]) {
			console.error(
				`Unknown workload: ${name}. Available: ${Object.keys(WORKLOADS).join(", ")}`,
			);
			process.exit(1);
		}
		workload = WORKLOADS[name];
	}

	return { count, workload };
}

async function main() {
	if (!global.gc) {
		console.error(
			"ERROR: Run with --expose-gc flag\n" +
				"  npx tsx --expose-gc benchmarks/memory.bench.ts",
		);
		process.exit(1);
	}

	const { count, workload } = parseArgs();
	const hardware = getHardware();
	console.error(`=== Memory Overhead Benchmark ===`);
	console.error(`Workload: ${workload.name} — ${workload.description}`);
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(`Count: ${count} VMs\n`);

	const result = await measure(workload, count);

	console.error(
		`\n  avg per-VM RSS: ${formatBytes(result.avgPerVmRssBytes)} | heap: ${formatBytes(result.avgPerVmHeapBytes)}`,
	);
	console.error(`  reclaimed: ${formatBytes(result.reclaimedRssBytes)}`);

	// Summary table.
	printTable(
		["step", "RSS delta", "heap delta"],
		result.steps.map((s) => [
			s.stepIndex,
			formatBytes(s.rssBytes),
			formatBytes(s.heapBytes),
		]),
	);

	// JSON to stdout.
	console.log(JSON.stringify({ hardware, result }, null, 2));

	await stopLlmock();
}

main().catch((err) => {
	console.error(err);
	process.exit(1);
});
