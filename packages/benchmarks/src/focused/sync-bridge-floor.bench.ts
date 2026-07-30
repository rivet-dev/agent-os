/**
 * Focused synchronous bridge floor benchmark.
 *
 * Measures a benchmark-only no-op sync bridge RPC that returns before any
 * filesystem/VFS dispatch. Use this to separate bridge round-trip,
 * serialization, and service-loop routing cost from real operation bodies.
 */

import { readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createBenchSidecar, createBenchVm, type BenchVm } from "../lib/vm.js";
import type { SidecarProcess } from "@rivet-dev/agentos-core";
import { getHardware, printTable, round, stats } from "../lib/perf-utils.js";

interface SyncRpcLatency {
	calls: number;
	totalUs: number;
	avgUs: number;
	maxUs: number;
}

interface SyncBridgePhaseMetric {
	method: string;
	stage: string;
	calls: number;
	totalUs: number;
	avgUs: number;
	maxUs: number;
}

interface BridgeFloorCase {
	callCount: number;
	payloadBytes: number;
	iterations: number;
	warmup: number;
	hostLoop: ReturnType<typeof stats>;
	guestBridge: ReturnType<typeof stats>;
	guestVsHostRatio: number;
	guestMsPerRpc: number;
	syncRpcLatency?: SyncRpcLatency | null;
	syncBridgePhases?: SyncBridgePhaseMetric[] | null;
	raw: {
		hostLoopMs: number[];
		guestBridgeMs: number[];
	};
}

function parseArgs(): {
	iterations: number;
	warmup: number;
	callCounts: number[];
	payloadBytes: number;
	syncRpcLatencyEnabled: boolean;
	bridgePhasesEnabled: boolean;
} {
	const value = (name: string) =>
		process.argv.find((arg) => arg.startsWith(`--${name}=`))?.split("=")[1];
	const iterations = Number(value("iterations") ?? 10);
	const warmup = Number(value("warmup") ?? 2);
	const callCounts = (value("call-counts") ?? "1,8,32")
		.split(",")
		.map((n) => Number(n.trim()))
		.filter((n) => Number.isFinite(n) && n >= 1);
	const payloadBytes = Number(value("payload-bytes") ?? 0);
	const syncRpcLatencyEnabled =
		process.argv.includes("--sync-rpc-latency") || value("sync-rpc-latency") === "1";
	const bridgePhasesEnabled =
		process.argv.includes("--bridge-phases") || value("bridge-phases") === "1";
	if (
		iterations < 1 ||
		warmup < 0 ||
		callCounts.length === 0 ||
		!Number.isFinite(payloadBytes) ||
		payloadBytes < 0
	) {
		throw new Error(
			"invalid args; expected --iterations>=1 --warmup>=0 --call-counts=1,8,32 --payload-bytes>=0",
		);
	}
	return { iterations, warmup, callCounts, payloadBytes, syncRpcLatencyEnabled, bridgePhasesEnabled };
}

function nowMs(start: number): number {
	return performance.now() - start;
}

function parseSyncRpcLatencyFile(path: string): SyncRpcLatency | null {
	try {
		const text = readFileSync(path, "utf8").trim();
		const match = /calls=(\d+) total_us=(\d+) avg_us=(\d+) max_us=(\d+)/.exec(text);
		if (!match) return null;
		return {
			calls: Number(match[1]),
			totalUs: Number(match[2]),
			avgUs: Number(match[3]),
			maxUs: Number(match[4]),
		};
	} catch {
		return null;
	}
}

function parseSyncBridgePhasesFile(path: string): SyncBridgePhaseMetric[] | null {
	try {
		const text = readFileSync(path, "utf8").trim();
		if (!text) return [];
		const metrics: SyncBridgePhaseMetric[] = [];
		for (const line of text.split(/\r?\n/)) {
			const match =
				/^method=(\S+) stage=(\S+) calls=(\d+) total_us=(\d+) avg_us=(\d+) max_us=(\d+)$/.exec(
					line,
				);
			if (!match) continue;
			metrics.push({
				method: match[1],
				stage: match[2],
				calls: Number(match[3]),
				totalUs: Number(match[4]),
				avgUs: Number(match[5]),
				maxUs: Number(match[6]),
			});
		}
		return metrics;
	} catch {
		return null;
	}
}

function runHostLoop(callCount: number, payloadBytes: number, iterations: number, warmup: number): number[] {
	const samples: number[] = [];
	const payload = "x".repeat(payloadBytes);
	let sink = 0;
	for (let i = 0; i < warmup + iterations; i++) {
		const start = performance.now();
		for (let j = 0; j < callCount; j++) {
			sink += payload.length;
		}
		const ms = nowMs(start);
		if (i >= warmup) samples.push(ms);
	}
	if (sink === -1) console.error("unreachable");
	return samples;
}

async function createVm(sidecar: SidecarProcess): Promise<BenchVm> {
	return createBenchVm({
		sidecar,
	});
}

async function runGuestBridge(
	vm: BenchVm,
	callCount: number,
	payloadBytes: number,
	iterations: number,
	warmup: number,
): Promise<number[]> {
	const scriptPath = `/tmp/sync-bridge-floor-${callCount}-${Date.now()}-${Math.random()
		.toString(16)
		.slice(2)}.mjs`;
	const source = `
const bridge = globalThis._benchNoop;
if (!bridge || typeof bridge.applySyncPromise !== "function") {
  throw new Error("_benchNoop bridge diagnostic is not available");
}
const callCount = ${JSON.stringify(callCount)};
const payload = "x".repeat(${JSON.stringify(payloadBytes)});
const iterations = Number(process.env.BENCH_ITERATIONS || 10);
const warmup = Number(process.env.BENCH_WARMUP || 2);
const samples = [];
const now = () => Number(process.hrtime.bigint()) / 1e6;
for (let i = 0; i < warmup + iterations; i++) {
  const start = now();
  for (let j = 0; j < callCount; j++) {
    bridge.applySyncPromise(void 0, [payload]);
  }
  const ms = now() - start;
  if (i >= warmup) samples.push(ms);
}
process.stdout.write(JSON.stringify({ samples }));
`;
	await vm.writeFile(scriptPath, source);
	let stdout = "";
	let stderr = "";
	const proc = vm.spawn("node", [scriptPath], {
		env: {
			BENCH_ITERATIONS: String(iterations),
			BENCH_WARMUP: String(warmup),
		},
		onStdout: (data) => {
			stdout += Buffer.from(data).toString("utf8");
		},
		onStderr: (data) => {
			stderr += Buffer.from(data).toString("utf8");
		},
	});
	const code = await vm.waitProcess(proc.pid);
	if (code !== 0) {
		throw new Error(`guest sync-bridge-floor exited ${code}\n${stderr}`);
	}
	return JSON.parse(stdout).samples;
}

async function runCase(
	callCount: number,
	payloadBytes: number,
	iterations: number,
	warmup: number,
	sidecar: SidecarProcess,
	latencyFile?: string,
	bridgePhasesFile?: string,
	hostBridgePhasesFile?: string,
): Promise<BridgeFloorCase> {
	const hostSamples = runHostLoop(callCount, payloadBytes, iterations, warmup);
	const vm = await createVm(sidecar);
	try {
		const guestSamples = await runGuestBridge(vm, callCount, payloadBytes, iterations, warmup);
		const hostLoop = stats(hostSamples);
		const guestBridge = stats(guestSamples);
		const syncBridgePhases = [
			...(bridgePhasesFile ? (parseSyncBridgePhasesFile(bridgePhasesFile) ?? []) : []),
			...(hostBridgePhasesFile ? (parseSyncBridgePhasesFile(hostBridgePhasesFile) ?? []) : []),
		];
		return {
			callCount,
			payloadBytes,
			iterations,
			warmup,
			hostLoop,
			guestBridge,
			guestVsHostRatio: round(guestBridge.p50 / hostLoop.p50),
			guestMsPerRpc: round(guestBridge.p50 / callCount),
			syncRpcLatency: latencyFile ? parseSyncRpcLatencyFile(latencyFile) : null,
			syncBridgePhases: bridgePhasesFile || hostBridgePhasesFile ? syncBridgePhases : null,
			raw: {
				hostLoopMs: hostSamples,
				guestBridgeMs: guestSamples,
			},
		};
	} finally {
		await vm.dispose();
	}
}

async function runCaseWithOptionalLatency(args: {
	callCount: number;
	payloadBytes: number;
	iterations: number;
	warmup: number;
	sharedSidecar: SidecarProcess | null;
	syncRpcLatencyEnabled: boolean;
	bridgePhasesEnabled: boolean;
}): Promise<BridgeFloorCase> {
	const needsDedicatedSidecar = args.syncRpcLatencyEnabled || args.bridgePhasesEnabled;
	if (!needsDedicatedSidecar) {
		if (!args.sharedSidecar) throw new Error("shared sidecar required when tracing is disabled");
		return runCase(
			args.callCount,
			args.payloadBytes,
			args.iterations,
			args.warmup,
			args.sharedSidecar,
		);
	}

	const latencyFile = join(
		tmpdir(),
		`agentos-sync-bridge-floor-lat-${args.callCount}-${Date.now()}-${Math.random()
			.toString(16)
			.slice(2)}.txt`,
	);
	const previousEnabled = process.env.AGENTOS_SYNCRPC_LAT;
	const previousFile = process.env.AGENTOS_SYNCRPC_LAT_FILE;
	const phasesFile = join(
		tmpdir(),
		`agentos-sync-bridge-floor-phases-${args.callCount}-${Date.now()}-${Math.random()
			.toString(16)
			.slice(2)}.txt`,
	);
	const previousPhasesEnabled = process.env.AGENTOS_SYNC_BRIDGE_PHASES;
	const previousPhasesFile = process.env.AGENTOS_SYNC_BRIDGE_PHASES_FILE;
	const hostPhasesFile = join(
		tmpdir(),
		`agentos-sync-bridge-floor-host-phases-${args.callCount}-${Date.now()}-${Math.random()
			.toString(16)
			.slice(2)}.txt`,
	);
	const previousHostPhasesEnabled = process.env.AGENTOS_SYNC_BRIDGE_HOST_PHASES;
	const previousHostPhasesFile = process.env.AGENTOS_SYNC_BRIDGE_HOST_PHASES_FILE;
	if (args.syncRpcLatencyEnabled) {
		process.env.AGENTOS_SYNCRPC_LAT = "1";
		process.env.AGENTOS_SYNCRPC_LAT_FILE = latencyFile;
	}
	if (args.bridgePhasesEnabled) {
		process.env.AGENTOS_SYNC_BRIDGE_PHASES = "1";
		process.env.AGENTOS_SYNC_BRIDGE_PHASES_FILE = phasesFile;
		process.env.AGENTOS_SYNC_BRIDGE_HOST_PHASES = "1";
		process.env.AGENTOS_SYNC_BRIDGE_HOST_PHASES_FILE = hostPhasesFile;
	}
	const sidecar = await createBenchSidecar();
	try {
		return await runCase(
			args.callCount,
			args.payloadBytes,
			args.iterations,
			args.warmup,
			sidecar,
			args.syncRpcLatencyEnabled ? latencyFile : undefined,
			args.bridgePhasesEnabled ? phasesFile : undefined,
			args.bridgePhasesEnabled ? hostPhasesFile : undefined,
		);
	} finally {
		await sidecar.dispose();
		if (previousEnabled === undefined) delete process.env.AGENTOS_SYNCRPC_LAT;
		else process.env.AGENTOS_SYNCRPC_LAT = previousEnabled;
		if (previousFile === undefined) delete process.env.AGENTOS_SYNCRPC_LAT_FILE;
		else process.env.AGENTOS_SYNCRPC_LAT_FILE = previousFile;
		if (previousPhasesEnabled === undefined) delete process.env.AGENTOS_SYNC_BRIDGE_PHASES;
		else process.env.AGENTOS_SYNC_BRIDGE_PHASES = previousPhasesEnabled;
		if (previousPhasesFile === undefined) delete process.env.AGENTOS_SYNC_BRIDGE_PHASES_FILE;
		else process.env.AGENTOS_SYNC_BRIDGE_PHASES_FILE = previousPhasesFile;
		if (previousHostPhasesEnabled === undefined) delete process.env.AGENTOS_SYNC_BRIDGE_HOST_PHASES;
		else process.env.AGENTOS_SYNC_BRIDGE_HOST_PHASES = previousHostPhasesEnabled;
		if (previousHostPhasesFile === undefined) delete process.env.AGENTOS_SYNC_BRIDGE_HOST_PHASES_FILE;
		else process.env.AGENTOS_SYNC_BRIDGE_HOST_PHASES_FILE = previousHostPhasesFile;
		rmSync(latencyFile, { force: true });
		rmSync(phasesFile, { force: true });
		rmSync(hostPhasesFile, { force: true });
	}
}

async function main(): Promise<void> {
	const { iterations, warmup, callCounts, payloadBytes, syncRpcLatencyEnabled, bridgePhasesEnabled } = parseArgs();
	const hardware = getHardware();
	console.error("=== Sync Bridge Floor Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(
		`Iterations: ${iterations} (+ ${warmup} warmup), callCounts: ${callCounts.join(",")}, payloadBytes: ${payloadBytes}, syncRpcLatency: ${syncRpcLatencyEnabled}, bridgePhases: ${bridgePhasesEnabled}`,
	);

	const sharedSidecar =
		syncRpcLatencyEnabled || bridgePhasesEnabled ? null : await createBenchSidecar();
	try {
		const cases: BridgeFloorCase[] = [];
		for (const callCount of callCounts) {
			const result = await runCaseWithOptionalLatency({
				callCount,
				payloadBytes,
				iterations,
				warmup,
				sharedSidecar,
				syncRpcLatencyEnabled,
				bridgePhasesEnabled,
			});
			cases.push(result);
			console.error(
				`  calls=${callCount}: hostLoop.p50=${result.hostLoop.p50}ms guestBridge.p50=${result.guestBridge.p50}ms guest.ms/rpc=${result.guestMsPerRpc}`,
			);
		}
		printTable(
			["calls", "hostLoopP50", "guestBridgeP50", "guestMsPerRpc", "syncRpcAvgUs"],
			cases.map((result) => [
				result.callCount,
				result.hostLoop.p50,
				result.guestBridge.p50,
				result.guestMsPerRpc,
				result.syncRpcLatency?.avgUs ?? "",
			]),
		);
		console.log(
			JSON.stringify(
				{
					benchmark: "sync-bridge-floor",
					generatedAt: new Date().toISOString(),
					hardware,
					iterations,
					warmup,
					callCounts,
					payloadBytes,
					syncRpcLatencyEnabled,
					bridgePhasesEnabled,
					cases,
				},
				null,
				2,
			),
		);
	} finally {
		if (sharedSidecar) await sharedSidecar.dispose();
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
