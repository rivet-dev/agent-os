/**
 * Memory overhead benchmark for the AgentOS language execution runtime.
 *
 * Measures incremental host-process RSS per live runtime by booting N runtimes
 * via the public `agentos` SDK, sampling memory, then tearing them down.
 *
 * Each `NodeRuntime.create()` boots an out-of-process sidecar VM, so the bulk
 * of a runtime's memory lives in the sidecar child process, not this Node
 * process. The RSS delta measured here is the host-side bookkeeping per live
 * runtime (client state, frame buffers, IPC). The "teardown reclaimed" figure
 * shows how much of that host-side RSS `dispose()` returns.
 *
 * Usage:
 *   AGENTOS_SIDECAR_BIN=/path/to/agentos-native-sidecar \
 *     node --expose-gc --import tsx/esm memory.bench.ts
 */

import type { NodeRuntime } from "@rivet-dev/agentos-runtime-core";
import {
	BATCH_SIZES,
	createBenchRuntime,
	forceGC,
	formatBytes,
	getHardware,
	MAX_CONCURRENCY,
	MEMORY_ITERATIONS,
	printTable,
	sleep,
	TRIVIAL_CODE,
} from "./bench-utils.js";

interface MemoryEntry {
	batchSize: number;
	totalDeltaRssBytes: number;
	totalDeltaHeapBytes: number;
	perRuntimeRssBytes: number;
	perRuntimeHeapBytes: number;
	teardownReclaimedRssBytes: number;
}

async function measureBatch(batchSize: number): Promise<MemoryEntry> {
	const rssSamples: number[] = [];
	const heapSamples: number[] = [];
	const reclaimSamples: number[] = [];

	for (let iter = 0; iter < MEMORY_ITERATIONS; iter++) {
		// Baseline: multiple GC passes to flush incremental/concurrent phases.
		forceGC();
		forceGC();
		await sleep(50);
		const baseline = process.memoryUsage();

		// Create and initialize runtimes, in chunks up to MAX_CONCURRENCY.
		const runtimes: NodeRuntime[] = [];
		let remaining = batchSize;

		while (remaining > 0) {
			const chunk = Math.min(remaining, MAX_CONCURRENCY);
			const batch = await Promise.all(
				Array.from({ length: chunk }, async () => {
					const rt = await createBenchRuntime();
					await rt.exec(TRIVIAL_CODE);
					return rt;
				}),
			);
			runtimes.push(...batch);
			remaining -= chunk;
		}

		// Measure after init.
		forceGC();
		forceGC();
		await sleep(50);
		const afterInit = process.memoryUsage();

		rssSamples.push(afterInit.rss - baseline.rss);
		heapSamples.push(afterInit.heapUsed - baseline.heapUsed);

		// Teardown.
		await Promise.all(runtimes.map((rt) => rt.dispose()));
		forceGC();
		forceGC();
		await sleep(50);
		const afterTeardown = process.memoryUsage();

		reclaimSamples.push(afterInit.rss - afterTeardown.rss);
	}

	const avg = (xs: number[]) => xs.reduce((a, b) => a + b, 0) / xs.length;
	const avgRss = avg(rssSamples);
	const avgHeap = avg(heapSamples);
	const avgReclaim = avg(reclaimSamples);

	return {
		batchSize,
		totalDeltaRssBytes: Math.round(avgRss),
		totalDeltaHeapBytes: Math.round(avgHeap),
		perRuntimeRssBytes: Math.round(avgRss / batchSize),
		perRuntimeHeapBytes: Math.round(avgHeap / batchSize),
		teardownReclaimedRssBytes: Math.round(avgReclaim),
	};
}

async function main() {
	if (!global.gc) {
		console.error(
			"ERROR: Run with --expose-gc flag\n" +
				"  node --expose-gc --import tsx/esm memory.bench.ts",
		);
		process.exit(1);
	}

	const hardware = getHardware();
	console.error("=== Memory Overhead Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(`Iterations per batch: ${MEMORY_ITERATIONS}`);
	console.error(`Batch sizes: ${BATCH_SIZES.join(", ")}`);
	console.error(
		`Sidecar: ${process.env.AGENTOS_SIDECAR_BIN ?? process.env.AGENTOS_SIDECAR_BIN ?? "(resolved from @rivet-dev/agentos-runtime-sidecar)"}\n`,
	);

	const results: MemoryEntry[] = [];

	for (const batchSize of BATCH_SIZES) {
		console.error(`\n--- batch=${batchSize} ---`);
		const entry = await measureBatch(batchSize);
		results.push(entry);
		console.error(
			`  total RSS delta: ${formatBytes(entry.totalDeltaRssBytes)}`,
		);
		console.error(
			`  per-runtime RSS: ${formatBytes(entry.perRuntimeRssBytes)}`,
		);
		console.error(
			`  per-runtime heap: ${formatBytes(entry.perRuntimeHeapBytes)}`,
		);
		console.error(
			`  teardown reclaimed: ${formatBytes(entry.teardownReclaimedRssBytes)}`,
		);
	}

	printTable(
		["batch", "total RSS", "per-rt RSS", "per-rt heap", "reclaimed"],
		results.map((r) => [
			r.batchSize,
			formatBytes(r.totalDeltaRssBytes),
			formatBytes(r.perRuntimeRssBytes),
			formatBytes(r.perRuntimeHeapBytes),
			formatBytes(r.teardownReclaimedRssBytes),
		]),
	);

	console.log(JSON.stringify({ hardware, results }, null, 2));
}

main().catch((err) => {
	console.error(err);
	process.exit(1);
});
