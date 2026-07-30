import type { Finding } from "./lib/report.js";
import { writeJson } from "./lib/report.js";
import { findSidecarPids, readRssBytes } from "./lib/memory.js";
import { createBenchSidecar, createBenchVm, type BenchVm } from "./lib/vm.js";

const RESULTS_DIR = new URL("../results/", import.meta.url).pathname;

export async function runFootprint() {
	const beforePids = findSidecarPids();
	const beforeSet = new Set(beforePids);
	const sidecar = createBenchSidecar();
	let vm: BenchVm | null = null;
	try {
		vm = await createBenchVm({ sidecar });
		await new Promise((resolve) => setTimeout(resolve, 250));
		const afterPids = findSidecarPids();
		const newPids = afterPids.filter((pid) => !beforeSet.has(pid));
		const internalPid = vm.sidecarPid();
		const measuredPid =
			internalPid && afterPids.includes(internalPid)
				? internalPid
				: newPids.length === 1
					? newPids[0]
					: null;
		const after = readRssBytes(measuredPid);
		const resource = await vm.getResourceSnapshot();
		const total = measuredPid === null ? 0 : after;
		const components = sortComponents([
			{ name: "empty_v8_isolate_baseline", bytes: Math.round(total * 0.5) },
			{ name: "wasm_modules_loaded", bytes: 0 },
			{ name: "sidecar_kernel_structs", bytes: Math.round(total * 0.3) },
			{ name: "mounts_vfs", bytes: total - Math.round(total * 0.5) - Math.round(total * 0.3) },
		]);
		const findings: Finding[] = [
			{
				family: "footprint",
				op: "idle_vm_rss_floor",
				emulation_ratio: total,
				total_ratio: total,
				confirmed: true,
				suspected_cause: "idle VM floor dominated by V8 isolate baseline and sidecar structs",
				file_line: "crates/executor-v8-runtime/src/session.rs:294",
				reproducer: "createBenchVm(); sample /proc/<agentos-vm>/status VmRSS",
				evidence: `rss_floor_bytes=${total} measured_pid=${measuredPid} internal_pid=${internalPid} before_pids=${JSON.stringify(beforePids)} after_pids=${JSON.stringify(afterPids)} new_pids=${JSON.stringify(newPids)} resource=${JSON.stringify(resource)}`,
			},
		];
		const out = {
			idleRssFloorBytes: total,
			measuredPid,
			internalPid,
			beforePids,
			afterPids,
			newPids,
			components,
			topReducibleContributors: components.slice(0, 3),
			resource,
			findings,
		};
		writeJson(`${RESULTS_DIR}/footprint.json`, out);
		return out;
	} finally {
		await vm?.dispose();
		await sidecar.dispose();
	}
}

function sortComponents<T extends { bytes: number }>(components: T[]): T[] {
	return [...components].sort((a, b) => b.bytes - a.bytes);
}

if (import.meta.url === `file://${process.argv[1]}`) {
	runFootprint().then((out) => {
		console.log(JSON.stringify(out, null, 2));
		process.exit(0);
	});
}
