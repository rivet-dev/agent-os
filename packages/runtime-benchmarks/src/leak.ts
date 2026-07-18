import { sampleMemory, slope } from "./lib/memory.js";
import type { Finding } from "./lib/report.js";
import { writeJson } from "./lib/report.js";
import { createBenchVm, type BenchVm } from "./lib/vm.js";

const RESULTS_DIR = new URL("../results/", import.meta.url).pathname;

export async function runLeakSuite() {
	const streams = ["process", "socket", "fd"] as const;
	const all = [];
	const findings: Finding[] = [];
	for (const stream of streams) {
		const result = await runLeakStream(stream);
		all.push(result);
		for (const [signal, value] of Object.entries(result.slopes)) {
			if (value > 0) {
				findings.push({
					family: "leak",
					op: `${stream}/${signal}`,
					emulation_ratio: value,
					total_ratio: value,
					confirmed: true,
					suspected_cause: attribution(signal),
					file_line: fileLine(signal),
					reproducer: `BENCH_LEAK_CYCLES=${result.cycles} tsx packages/benchmarks/src/leak.ts`,
					evidence: `${signal} slope=${value}`,
				});
			}
		}
	}
	const out = { streams: all, findings };
	writeJson(`${RESULTS_DIR}/leak-process.json`, out);
	return out;
}

async function runLeakStream(stream: "process" | "socket" | "fd") {
	const cycles = Number(process.env.BENCH_LEAK_CYCLES ?? 4);
	const idleMs = Number(process.env.BENCH_LEAK_IDLE_MS ?? 61_000);
	const vm = await createBenchVm();
	try {
		const samples = [];
		for (let cycle = 0; cycle < cycles; cycle++) {
			if (stream === "process") {
				const proc = vm.spawn("node", ["-e", "process.exit(0)"]);
				await vm.waitProcess(proc.pid);
			} else if (stream === "fd") {
				await runGuestOne(
					vm,
					`const fs = await import("node:fs");
const path = "/tmp/leak-fd-${cycle}.txt";
const fd = fs.openSync(path, "w");
fs.writeSync(fd, "hi");
fs.closeSync(fd);`,
					`fd-${cycle}`,
				);
			} else {
				await runGuestOne(
					vm,
					`const net = await import("node:net");
await new Promise((resolve, reject) => {
  const server = net.createServer((socket) => socket.end("ok"));
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {
    const port = server.address().port;
    const socket = net.connect(port, "127.0.0.1");
    socket.on("error", reject);
    socket.on("data", () => {});
    socket.on("close", () => server.close(resolve));
  });
});`,
					`socket-${cycle}`,
				);
			}
			samples.push(await sampleMemory(vm, cycle));
			await new Promise((resolve) => setTimeout(resolve, 250));
		}
		if (idleMs > 0) {
			await new Promise((resolve) => setTimeout(resolve, idleMs));
			samples.push(await sampleMemory(vm, cycles));
		}
		const slopes = {
			guestHeapRss: slope(samples, "guestHeapRss"),
			sidecarRss: slope(samples, "sidecarRss"),
			runningProcesses: slope(samples, "runningProcesses"),
			stoppedProcesses: slope(samples, "stoppedProcesses"),
			exitedProcesses: slope(samples, "exitedProcesses"),
			openFds: slope(samples, "openFds"),
			sockets: slope(samples, "sockets"),
			pipes: slope(samples, "pipes"),
		};
		return { stream, cycles, idleMs, samples, slopes };
	} finally {
		await vm.dispose();
	}
}

async function runGuestOne(vm: BenchVm, source: string, name: string): Promise<void> {
	const path = `/tmp/fuzz-perf-leak-${name.replace(/[^a-z0-9_-]/gi, "_")}.mjs`;
	await vm.writeFile(path, source);
	let stderr = "";
	const proc = vm.spawn("node", [path], {
		onStderr: (data) => {
			stderr += Buffer.from(data).toString("utf8");
		},
	});
	const code = await vm.waitProcess(proc.pid);
	if (code !== 0) {
		throw new Error(`leak stream ${name} exited ${code}\n${stderr}`);
	}
}

function attribution(signal: string): string {
	if (signal.includes("Process")) return "ProcessTable/zombie retention or reap delay";
	if (signal === "sidecarRss") return "native Rust-side resource retention";
	if (signal === "guestHeapRss") return "guest isolate/bridge heap retention";
	return "kernel resource table retention";
}

function fileLine(signal: string): string {
	if (signal.includes("Process")) return "crates/kernel/src/process_table.rs:842";
	if (signal === "sidecarRss") return "crates/kernel/src/resource_accounting.rs:36";
	return "crates/kernel/src/kernel.rs:581";
}

if (import.meta.url === `file://${process.argv[1]}`) {
	runLeakSuite().then((out) => {
		console.log(JSON.stringify(out, null, 2));
		process.exit(0);
	});
}
