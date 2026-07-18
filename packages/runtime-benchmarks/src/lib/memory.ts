import { execFileSync, spawn } from "node:child_process";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { forceGC } from "./perf-utils.js";
import type { BenchVm } from "./vm.js";

export interface MemorySample {
	cycle: number;
	guestHeapRss: number;
	sidecarRss: number;
	runningProcesses: number;
	stoppedProcesses: number;
	exitedProcesses: number;
	openFds: number;
	sockets: number;
	pipes: number;
}

export function findSidecarPid(): number | null {
	return findSidecarPids()[0] ?? null;
}

export function findSidecarPids(): number[] {
	const pids: number[] = [];
	for (const pid of readdirSync("/proc")) {
		if (!/^\d+$/.test(pid)) continue;
		try {
			const comm = readFileSync(`/proc/${pid}/comm`, "utf8").trim();
			if (comm === "agentos-native-sidecar") {
				pids.push(Number(pid));
			}
		} catch {
			// Process exited while scanning.
		}
	}
	return pids.sort((a, b) => a - b);
}

export function readRssBytes(pid: number | null): number {
	if (pid === null) return 0;
	try {
		const status = readFileSync(`/proc/${pid}/status`, "utf8");
		const match = status.match(/^VmRSS:\s+(\d+)\s+kB/m);
		return match ? Number(match[1]) * 1024 : 0;
	} catch {
		return 0;
	}
}

export interface LaneMemory {
	memBytes: number;
	memProvenance: string;
}

export interface MeasuredValue<T> {
	value: T;
	memory?: LaneMemory;
}

export const SIDECAR_MEMORY_PROVENANCE =
	"/proc/<sidecarPid>/clear_refs=5, baseline VmRSS, post-op VmHWM, max(VmHWM - baseline, 0)";

export const NATIVE_MEMORY_PROVENANCE =
	"direct child spawn with /proc/<pid>/status VmHWM sampling minus native-baseline cpu_loop --iters 1 startup baseline, floored to one page";

export const NODE_MEMORY_PROVENANCE =
	'direct child spawn with /proc/<pid>/status VmHWM sampling minus node -e "" startup baseline, floored to one page';

export function procPeakMemorySupportReason(): string | undefined {
	if (process.platform !== "linux") {
		return "Linux /proc clear_refs/VmHWM memory measurement is unavailable on this platform";
	}
	if (!existsSync("/proc/self/status") || !existsSync("/proc/self/clear_refs")) {
		return "Linux /proc status/clear_refs memory measurement is unavailable";
	}
	return undefined;
}

export function hostPeakMemorySupportReason(): string | undefined {
	if (process.platform !== "linux") {
		return "host child /proc VmHWM memory measurement is only enabled on Linux";
	}
	if (!existsSync("/proc/self/status")) {
		return "Linux /proc status memory measurement is unavailable";
	}
	return undefined;
}

export function formatBytes(bytes: number | undefined): string {
	if (bytes === undefined) return "-";
	const units = ["B", "KiB", "MiB", "GiB"];
	let value = bytes;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit++;
	}
	const decimals = unit === 0 || value >= 10 ? 0 : 1;
	return `${value.toFixed(decimals)}${units[unit]}`;
}

export function pageSizeBytes(): number {
	try {
		const stdout = execFileSync("getconf", ["PAGESIZE"], {
			encoding: "utf8",
			stdio: ["ignore", "pipe", "ignore"],
		});
		const parsed = Number(stdout.trim());
		if (Number.isFinite(parsed) && parsed > 0) return parsed;
	} catch {
		// Fall through to the common Linux page size.
	}
	return 4096;
}

export interface TimedCommandResult {
	stdout: string;
	stderr: string;
	maxRssBytes: number;
}

export function runCommandWithMaxRss(
	command: string,
	args: string[],
	options: {
		cwd?: string;
		env?: NodeJS.ProcessEnv;
		maxBuffer?: number;
	} = {},
): Promise<TimedCommandResult> {
	const reason = hostPeakMemorySupportReason();
	if (reason) throw new Error(reason);
	return new Promise((resolve, reject) => {
		const child = spawn(command, args, {
			cwd: options.cwd,
			env: options.env,
			stdio: ["ignore", "pipe", "pipe"],
		});
		const maxBuffer = options.maxBuffer ?? 128 * 1024 * 1024;
		const stdout: Buffer[] = [];
		const stderr: Buffer[] = [];
		let stdoutBytes = 0;
		let stderrBytes = 0;
		let maxRssBytes = 0;
		let poll: NodeJS.Timeout | undefined;

		const sample = () => {
			if (child.pid === undefined) return;
			try {
				maxRssBytes = Math.max(maxRssBytes, readStatusBytes(child.pid, "VmHWM"));
			} catch {
				try {
					maxRssBytes = Math.max(maxRssBytes, readStatusBytes(child.pid, "VmRSS"));
				} catch {
					// The child may have exited between polls.
				}
			}
		};
		const collect = (chunks: Buffer[], kind: "stdout" | "stderr") => (chunk: Buffer) => {
			if (kind === "stdout") stdoutBytes += chunk.length;
			else stderrBytes += chunk.length;
			if (stdoutBytes + stderrBytes > maxBuffer) {
				child.kill("SIGKILL");
				reject(new Error(`${command} output exceeded maxBuffer ${maxBuffer}`));
				return;
			}
			chunks.push(chunk);
		};

		child.stdout.on("data", collect(stdout, "stdout"));
		child.stderr.on("data", collect(stderr, "stderr"));
		child.on("spawn", () => {
			sample();
			poll = setInterval(sample, 1);
		});
		child.on("error", (error) => {
			if (poll) clearInterval(poll);
			reject(error);
		});
		child.on("close", (code, signal) => {
			if (poll) clearInterval(poll);
			sample();
			const stdoutText = Buffer.concat(stdout).toString("utf8");
			const stderrText = Buffer.concat(stderr).toString("utf8");
			if (code !== 0) {
				reject(
					new Error(
						`${command} ${args.join(" ")} exited ${code ?? signal}\n${stdoutText}\n${stderrText}`,
					),
				);
				return;
			}
			resolve({
				stdout: stdoutText,
				stderr: stderrText,
				maxRssBytes,
			});
		});
	});
}

export class SidecarPeakMemorySampler {
	private constructor(private readonly pid: number) {}

	static forVm(vm: BenchVm): SidecarPeakMemorySampler | undefined {
		if (procPeakMemorySupportReason()) return undefined;
		const pid = vm.sidecarPid();
		return typeof pid === "number" ? new SidecarPeakMemorySampler(pid) : undefined;
	}

	async measure<T>(fn: () => Promise<T> | T): Promise<MeasuredValue<T>> {
		this.resetHighWaterMark();
		const baselineRss = this.readStatusBytes("VmRSS");
		const value = await fn();
		const highWater = this.readStatusBytes("VmHWM");
		return {
			value,
			memory: {
				memBytes: Math.max(0, highWater - baselineRss),
				memProvenance: SIDECAR_MEMORY_PROVENANCE,
			},
		};
	}

	async measureIdle(waitMs: number): Promise<LaneMemory> {
		this.resetHighWaterMark();
		const baselineRss = this.readStatusBytes("VmRSS");
		await new Promise((resolve) => setTimeout(resolve, waitMs));
		const highWater = this.readStatusBytes("VmHWM");
		return {
			memBytes: Math.max(0, highWater - baselineRss),
			memProvenance: SIDECAR_MEMORY_PROVENANCE,
		};
	}

	private resetHighWaterMark(): void {
		writeFileSync(`/proc/${this.pid}/clear_refs`, "5");
	}

	private readStatusBytes(field: "VmRSS" | "VmHWM"): number {
		return readStatusBytes(this.pid, field);
	}
}

function readStatusBytes(pid: number, field: "VmRSS" | "VmHWM"): number {
	const status = readFileSync(`/proc/${pid}/status`, "utf8");
	const match = status.match(new RegExp(`^${field}:\\s+(\\d+)\\s+kB`, "m"));
	if (!match) throw new Error(`could not read ${field} for pid ${pid}`);
	return Number(match[1]) * 1024;
}

export async function sampleMemory(vm: BenchVm, cycle: number): Promise<MemorySample> {
	forceGC();
	const resource = await vm.getResourceSnapshot();
	const guestHeapRss = await sampleGuestHeap(vm);
	return {
		cycle,
		guestHeapRss,
		sidecarRss: readRssBytes(findSidecarPid()),
		runningProcesses: resource.runningProcesses,
		stoppedProcesses: resource.stoppedProcesses,
		exitedProcesses: resource.exitedProcesses,
		openFds: resource.openFds,
		sockets: resource.sockets,
		pipes: resource.pipes,
	};
}

export function slope(samples: Array<{ cycle: number }>, key: string): number {
	const n = samples.length;
	const sx = samples.reduce((sum, sample) => sum + sample.cycle, 0);
	const sy = samples.reduce((sum, sample) => sum + Number((sample as any)[key]), 0);
	const sxy = samples.reduce(
		(sum, sample) => sum + sample.cycle * Number((sample as any)[key]),
		0,
	);
	const sx2 = samples.reduce((sum, sample) => sum + sample.cycle ** 2, 0);
	const denom = n * sx2 - sx ** 2;
	return denom === 0 ? 0 : (n * sxy - sx * sy) / denom;
}

async function sampleGuestHeap(vm: BenchVm): Promise<number> {
	const script = "/tmp/guest-memory-usage.mjs";
	await vm.writeFile(
		script,
		"process.stdout.write(String(process.memoryUsage().rss));",
	);
	let stdout = "";
	const proc = vm.spawn("node", [script], {
		onStdout: (data) => {
			stdout += Buffer.from(data).toString("utf8");
		},
	});
	const code = await vm.waitProcess(proc.pid);
	if (code !== 0) return 0;
	return Number(stdout.trim() || 0);
}
