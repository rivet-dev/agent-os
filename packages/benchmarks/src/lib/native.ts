import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
	NATIVE_MEMORY_PROVENANCE,
	hostPeakMemorySupportReason,
	pageSizeBytes,
	runCommandWithMaxRss,
	type LaneMemory,
} from "./memory.js";

const DEFAULT_NATIVE_BIN =
	join(
		fileURLToPath(new URL("../../../..", import.meta.url)),
		"target/release/agentos-benchmark-baseline",
	);
let nativeStartupMaxRssBytes: number | undefined;

export type NativeOp =
	| "spawn_exit"
	| "exec_capture"
	| "node_stdout_discard_2b"
	| "node_stdout_capture_2b"
	| "node_stdout_listener_only_2b"
	| "node_exit"
	| "node_fanout"
	| "node_reap_storm"
	| "node_stdin_roundtrip"
	| "pipe_chain"
	| "fs_stat"
	| "fs_stat_x32"
	| "fs_write"
	| "fs_read"
	| "stream_copy"
	| "fs_open_close"
	| "fs_mkdir_rmdir"
	| "fs_rename"
	| "fs_readdir"
	| "fs_fsync"
	| "dns_lookup"
	| "dns_lookup_x2"
	| "dns_concurrent"
	| "tcp_connect"
	| "tcp_echo"
	| "unix_connect"
	| "unix_echo"
	| "http_loopback_get"
	| "tcp_concurrent"
	| "tcp_throughput"
	| "tcp_tiny_writes"
	| "udp_echo"
	| "pipe_echo"
	| "pipe_throughput"
	| "pipe_backpressure"
	| "stdio_write_sync"
	| "cpu_loop"
	| "sleep_timer"
	| "yield_loop"
	| "alloc_free";

export function runNativeLayer(
	op: NativeOp,
	iters: number,
	warmup: number,
	extraArgs: string[] = [],
): number[] {
	const bin = resolveNativeBaselineBin();
	const stdout = execFileSync(
		bin,
		["--op", op, "--iters", String(iters), "--warmup", String(warmup), ...extraArgs],
		{ encoding: "utf8", maxBuffer: 128 * 1024 * 1024 },
	);
	return parseNativeSamples(stdout);
}

export interface NativeLayerMeasurement {
	samples: number[];
	memory?: LaneMemory;
}

export async function runNativeLayerMeasured(
	op: NativeOp,
	iters: number,
	warmup: number,
	extraArgs: string[] = [],
): Promise<NativeLayerMeasurement> {
	const bin = resolveNativeBaselineBin();
	const args = ["--op", op, "--iters", String(iters), "--warmup", String(warmup), ...extraArgs];
	const timed =
		hostPeakMemorySupportReason() === undefined
			? await runCommandWithMaxRss(bin, args)
			: undefined;
	const stdout =
		timed?.stdout ??
		execFileSync(bin, args, {
			encoding: "utf8",
			maxBuffer: 128 * 1024 * 1024,
		});
	const samples = parseNativeSamples(stdout);
	return {
		samples,
		...(timed
			? {
					memory: {
						memBytes: await subtractStartupBaseline(timed.maxRssBytes),
						memProvenance: NATIVE_MEMORY_PROVENANCE,
					},
				}
			: {}),
	};
}

export async function primeNativeMemoryBaseline(): Promise<number | undefined> {
	if (hostPeakMemorySupportReason()) return undefined;
	if (nativeStartupMaxRssBytes !== undefined) return nativeStartupMaxRssBytes;
	nativeStartupMaxRssBytes = (
		await runCommandWithMaxRss(resolveNativeBaselineBin(), [
			"--op",
			"cpu_loop",
			"--iters",
			"1",
			"--warmup",
			"0",
		])
	).maxRssBytes;
	return nativeStartupMaxRssBytes;
}

function resolveNativeBaselineBin(): string {
	return process.env.NATIVE_BASELINE_BIN ?? DEFAULT_NATIVE_BIN;
}

async function subtractStartupBaseline(opMaxRssBytes: number): Promise<number> {
	const baseline = (await primeNativeMemoryBaseline()) ?? 0;
	return Math.max(opMaxRssBytes - baseline, pageSizeBytes());
}

function parseNativeSamples(stdout: string): number[] {
	const parsed = JSON.parse(stdout) as {
		unit: string;
		samples: number[];
	};
	if (parsed.unit !== "ns") {
		throw new Error(`native-baseline emitted unexpected unit: ${parsed.unit}`);
	}
	return parsed.samples.map((ns) => ns / 1e6);
}

export type NativePhaseSamples = Record<string, number[]>;

export function runNativePhaseLayer(
	op: Extract<NativeOp, "node_exit" | "node_fanout">,
	iters: number,
	warmup: number,
): NativePhaseSamples {
	const bin = process.env.NATIVE_BASELINE_BIN ?? DEFAULT_NATIVE_BIN;
	const stdout = execFileSync(
		bin,
		[
			"--op",
			op,
			"--iters",
			String(iters),
			"--warmup",
			String(warmup),
			"--phases",
		],
		{ encoding: "utf8", maxBuffer: 128 * 1024 * 1024 },
	);
	const parsed = JSON.parse(stdout) as {
		unit: string;
		phases: Record<string, number[]>;
	};
	if (parsed.unit !== "ns") {
		throw new Error(`native-baseline emitted unexpected unit: ${parsed.unit}`);
	}
	return Object.fromEntries(
		Object.entries(parsed.phases).map(([phase, samples]) => [
			phase,
			samples.map((ns) => ns / 1e6),
		]),
	);
}
