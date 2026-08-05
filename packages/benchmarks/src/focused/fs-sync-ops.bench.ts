/**
 * Focused synchronous filesystem operation benchmark.
 *
 * The broad fuzz/perf rows intentionally execute real JS snippets, but several
 * "single op" filesystem rows include guard/setup calls in the timed loop. This
 * benchmark makes logical operations and expected sync bridge calls explicit.
 */

import {
	closeSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	openSync,
	readFileSync,
	renameSync,
	rmSync,
	statSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createBenchSidecar, createBenchVm, type BenchVm } from "../lib/vm.js";
import type { SidecarProcess } from "@rivet-dev/agentos-core";
import { getHardware, printTable, round, stats } from "../lib/perf-utils.js";

type FsSyncOp =
	| "existsSync"
	| "statSync"
	| "openClose"
	| "mkdirRmdir"
	| "smallWrite"
	| "readFileSync"
	| "renameFile";
type FsSyncFixture = "vm-shadow";

interface OperationCounts {
	existsSync: number;
	statSync: number;
	openSync: number;
	closeSync: number;
	mkdirSync: number;
	rmdirSync: number;
	rmSync: number;
	readFileSync: number;
	writeFileSync: number;
	renameSync: number;
	unlinkSync: number;
}

interface SyncRpcLatency {
	calls: number;
	totalUs: number;
	avgUs: number;
	maxUs: number;
}

interface FsSyncPhaseMetric {
	method: string;
	calls: number;
	totalUs: number;
	avgUs: number;
	maxUs: number;
}

interface FsSyncCaseResult {
	op: FsSyncOp;
	fixture: FsSyncFixture;
	pathShape: string;
	callCount: number;
	payloadBytes: number;
	iterations: number;
	warmup: number;
	operationCounts: OperationCounts;
	bridgeCallCounts: OperationCounts;
	expectedSyncRpcCalls: number;
	host: ReturnType<typeof stats>;
	guest: ReturnType<typeof stats>;
	guestVsHostRatio: number;
	hostMsPerLogicalOp: number;
	guestMsPerLogicalOp: number;
	guestMsPerExpectedSyncRpc?: number;
	derived?: {
		guestP50MinusSingleMs?: number;
		hostP50MinusSingleMs?: number;
		guestIncrementalMsPerLogicalOp?: number;
		guestIncrementalMsPerExpectedSyncRpc?: number;
	};
	syncRpcLatency?: SyncRpcLatency | null;
	fsSyncPhases?: FsSyncPhaseMetric[] | null;
	raw: {
		hostMs: number[];
		guestMs: number[];
	};
}

function parseArgs(): {
	iterations: number;
	warmup: number;
	ops: FsSyncOp[];
	callCounts: number[];
	fixtures: FsSyncFixture[];
	payloadBytes: number;
	syncRpcLatencyEnabled: boolean;
	fsSyncPhasesEnabled: boolean;
} {
	const value = (name: string) =>
		process.argv.find((arg) => arg.startsWith(`--${name}=`))?.split("=")[1];
	const iterations = Number(value("iterations") ?? 10);
	const warmup = Number(value("warmup") ?? 2);
	const ops = (value("ops") ?? "existsSync,statSync,openClose,mkdirRmdir,smallWrite,readFileSync,renameFile")
		.split(",")
		.map((op) => op.trim())
		.filter(
			(op): op is FsSyncOp =>
				op === "existsSync" ||
				op === "statSync" ||
				op === "openClose" ||
				op === "mkdirRmdir" ||
				op === "smallWrite" ||
				op === "readFileSync" ||
				op === "renameFile",
		);
	const callCounts = (value("call-counts") ?? value("repeat-counts") ?? "1,8,32")
		.split(",")
		.map((n) => Number(n.trim()))
		.filter((n) => Number.isFinite(n) && n >= 1);
	const fixtures = (value("fixtures") ?? "vm-shadow")
		.split(",")
		.map((fixture) => fixture.trim())
		.filter((fixture): fixture is FsSyncFixture => fixture === "vm-shadow");
	const payloadBytes = Number(value("payload-bytes") ?? 8);
	const syncRpcLatencyEnabled =
		process.argv.includes("--sync-rpc-latency") || value("sync-rpc-latency") === "1";
	const fsSyncPhasesEnabled =
		process.argv.includes("--fs-sync-phases") || value("fs-sync-phases") === "1";
	if (
		iterations < 1 ||
		warmup < 0 ||
		ops.length === 0 ||
		callCounts.length === 0 ||
		fixtures.length === 0 ||
		!Number.isFinite(payloadBytes) ||
		payloadBytes < 0
	) {
		throw new Error(
			"invalid args; expected --iterations>=1 --warmup>=0 --ops=existsSync,statSync,openClose,mkdirRmdir,smallWrite,readFileSync,renameFile --call-counts=1,8,32 --fixtures=vm-shadow --payload-bytes=8",
		);
	}
	return {
		iterations,
		warmup,
		ops,
		callCounts,
		fixtures,
		payloadBytes,
		syncRpcLatencyEnabled,
		fsSyncPhasesEnabled,
	};
}

function nowMs(start: number): number {
	return performance.now() - start;
}

function emptyCounts(): OperationCounts {
	return {
		existsSync: 0,
		statSync: 0,
		openSync: 0,
		closeSync: 0,
		mkdirSync: 0,
		rmdirSync: 0,
		rmSync: 0,
		readFileSync: 0,
		writeFileSync: 0,
		renameSync: 0,
		unlinkSync: 0,
	};
}

function countsForOperation(op: FsSyncOp, callCount: number): {
	operationCounts: OperationCounts;
	bridgeCallCounts: OperationCounts;
} {
	const operationCounts = emptyCounts();
	const bridgeCallCounts = emptyCounts();
	const add = (counts: OperationCounts, key: keyof OperationCounts, amount = callCount) => {
		counts[key] += amount;
	};
	switch (op) {
		case "existsSync":
			add(operationCounts, "existsSync");
			add(bridgeCallCounts, "existsSync");
			break;
		case "statSync":
			add(operationCounts, "statSync");
			add(bridgeCallCounts, "statSync");
			break;
		case "openClose":
			add(operationCounts, "openSync");
			add(operationCounts, "closeSync");
			add(bridgeCallCounts, "openSync");
			add(bridgeCallCounts, "closeSync");
			break;
		case "mkdirRmdir":
			add(operationCounts, "mkdirSync");
			add(operationCounts, "rmSync");
			add(bridgeCallCounts, "mkdirSync");
			add(bridgeCallCounts, "statSync");
			add(bridgeCallCounts, "rmdirSync");
			break;
		case "smallWrite":
			add(operationCounts, "writeFileSync");
			add(bridgeCallCounts, "writeFileSync");
			break;
		case "readFileSync":
			add(operationCounts, "readFileSync");
			add(bridgeCallCounts, "readFileSync");
			break;
		case "renameFile":
			add(operationCounts, "writeFileSync");
			add(operationCounts, "renameSync");
			add(operationCounts, "unlinkSync");
			add(bridgeCallCounts, "writeFileSync");
			add(bridgeCallCounts, "renameSync");
			add(bridgeCallCounts, "unlinkSync");
			break;
	}
	return { operationCounts, bridgeCallCounts };
}

function syncRpcCount(counts: OperationCounts): number {
	return Object.values(counts).reduce((sum, count) => sum + count, 0);
}

function pathShapeForOperation(op: FsSyncOp): string {
	switch (op) {
		case "existsSync":
		case "statSync":
		case "openClose":
			return "existing-file";
		case "mkdirRmdir":
			return "fresh-empty-dir";
		case "smallWrite":
			return "overwrite-file";
		case "readFileSync":
			return "existing-file-read";
		case "renameFile":
			return "fresh-file-rename-unlink";
	}
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

function parseFsSyncPhasesFile(path: string): FsSyncPhaseMetric[] | null {
	try {
		const text = readFileSync(path, "utf8").trim();
		if (!text) return [];
		const phases: FsSyncPhaseMetric[] = [];
		for (const line of text.split(/\r?\n/)) {
			const match =
				/^method=(\S+) calls=(\d+) total_us=(\d+) avg_us=(\d+) max_us=(\d+)$/.exec(line);
			if (!match) return null;
			phases.push({
				method: match[1],
				calls: Number(match[2]),
				totalUs: Number(match[3]),
				avgUs: Number(match[4]),
				maxUs: Number(match[5]),
			});
		}
		return phases;
	} catch {
		return null;
	}
}

function createHostFixture(payloadBytes: number): string {
	const dir = mkdtempSync(join(tmpdir(), "agentos-fs-sync-bench-"));
	writeFileSync(join(dir, "fixture.txt"), "x".repeat(payloadBytes));
	return dir;
}

function runOperation(root: string, op: FsSyncOp, sampleIndex: number, callCount: number, payload: string): void {
	const fixture = join(root, "fixture.txt");
	for (let i = 0; i < callCount; i++) {
		const suffix = `${sampleIndex}-${i}`;
		switch (op) {
			case "existsSync":
				existsSync(fixture);
				break;
			case "statSync":
				statSync(fixture);
				break;
			case "openClose": {
				const fd = openSync(fixture, "r");
				closeSync(fd);
				break;
			}
			case "mkdirRmdir": {
				const path = join(root, `dir-${suffix}`);
				mkdirSync(path);
				rmSync(path, { recursive: true });
				break;
			}
			case "smallWrite":
				writeFileSync(fixture, payload);
				break;
			case "readFileSync":
				readFileSync(fixture);
				break;
			case "renameFile": {
				const from = join(root, `rename-${suffix}.a`);
				const to = join(root, `rename-${suffix}.b`);
				writeFileSync(from, payload);
				renameSync(from, to);
				unlinkSync(to);
				break;
			}
		}
	}
}

function runHost(
	root: string,
	op: FsSyncOp,
	callCount: number,
	payloadBytes: number,
	iterations: number,
	warmup: number,
): number[] {
	const samples: number[] = [];
	const payload = "x".repeat(payloadBytes);
	for (let i = 0; i < warmup + iterations; i++) {
		const start = performance.now();
		runOperation(root, op, i, callCount, payload);
		const ms = nowMs(start);
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

async function setupGuestFixture(vm: BenchVm, root: string, payloadBytes: number): Promise<void> {
	await vm.delete(root, { recursive: true }).catch(() => {});
	await vm.mkdir(root, { recursive: true });
	await vm.writeFile(`${root}/fixture.txt`, "x".repeat(payloadBytes));
}

async function runGuest(
	vm: BenchVm,
	root: string,
	op: FsSyncOp,
	callCount: number,
	payloadBytes: number,
	iterations: number,
	warmup: number,
): Promise<number[]> {
	const scriptPath = `/tmp/fs-sync-ops-${op}-${Date.now()}-${Math.random()
		.toString(16)
		.slice(2)}.mjs`;
	const source = `
import fs from "node:fs";
const root = ${JSON.stringify(root)};
const op = ${JSON.stringify(op)};
const callCount = ${JSON.stringify(callCount)};
const payload = "x".repeat(${JSON.stringify(payloadBytes)});
const iterations = Number(process.env.BENCH_ITERATIONS || 10);
const warmup = Number(process.env.BENCH_WARMUP || 2);
const fixture = root + "/fixture.txt";
const samples = [];
const now = () => Number(process.hrtime.bigint()) / 1e6;
function runOperation(sampleIndex) {
  for (let i = 0; i < callCount; i++) {
    const suffix = sampleIndex + "-" + i;
    if (op === "existsSync") {
      fs.existsSync(fixture);
    } else if (op === "statSync") {
      fs.statSync(fixture);
    } else if (op === "openClose") {
      const fd = fs.openSync(fixture, "r");
      fs.closeSync(fd);
    } else if (op === "mkdirRmdir") {
      const path = root + "/dir-" + suffix;
      fs.mkdirSync(path);
      fs.rmSync(path, { recursive: true });
    } else if (op === "smallWrite") {
      fs.writeFileSync(fixture, payload);
    } else if (op === "readFileSync") {
      fs.readFileSync(fixture);
    } else if (op === "renameFile") {
      const from = root + "/rename-" + suffix + ".a";
      const to = root + "/rename-" + suffix + ".b";
      fs.writeFileSync(from, payload);
      fs.renameSync(from, to);
      fs.unlinkSync(to);
    }
  }
}
for (let i = 0; i < warmup + iterations; i++) {
  const start = now();
  runOperation(i);
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
		throw new Error(`guest fs-sync-ops ${op} exited ${code}\n${stderr}`);
	}
	return JSON.parse(stdout).samples;
}

async function createVm(sidecar: SidecarProcess): Promise<BenchVm> {
	return createBenchVm({
		sidecar,
	});
}

async function runCase(
	op: FsSyncOp,
	fixture: FsSyncFixture,
	callCount: number,
	payloadBytes: number,
	iterations: number,
	warmup: number,
	sidecar: SidecarProcess,
	latencyFile?: string,
	fsSyncPhasesFile?: string,
): Promise<FsSyncCaseResult> {
	const hostRoot = createHostFixture(payloadBytes);
	const vmRoot = `/tmp/fs-sync-ops-${op}-${callCount}`;
	try {
		const hostSamples = runHost(hostRoot, op, callCount, payloadBytes, iterations, warmup);
		const vm = await createVm(sidecar);
		try {
			await setupGuestFixture(vm, vmRoot, payloadBytes);
			const guestSamples = await runGuest(vm, vmRoot, op, callCount, payloadBytes, iterations, warmup);
			const hostStats = stats(hostSamples);
			const guestStats = stats(guestSamples);
			const counts = countsForOperation(op, callCount);
			const expectedSyncRpcCalls = syncRpcCount(counts.bridgeCallCounts);
			return {
				op,
				fixture,
				pathShape: pathShapeForOperation(op),
				callCount,
				payloadBytes,
				iterations,
				warmup,
				operationCounts: counts.operationCounts,
				bridgeCallCounts: counts.bridgeCallCounts,
				expectedSyncRpcCalls,
				host: hostStats,
				guest: guestStats,
				guestVsHostRatio: round(guestStats.p50 / hostStats.p50),
				hostMsPerLogicalOp: round(hostStats.p50 / callCount),
				guestMsPerLogicalOp: round(guestStats.p50 / callCount),
				guestMsPerExpectedSyncRpc:
					expectedSyncRpcCalls > 0 ? round(guestStats.p50 / expectedSyncRpcCalls) : undefined,
				syncRpcLatency: latencyFile ? parseSyncRpcLatencyFile(latencyFile) : null,
				fsSyncPhases: fsSyncPhasesFile ? parseFsSyncPhasesFile(fsSyncPhasesFile) : null,
				raw: {
					hostMs: hostSamples,
					guestMs: guestSamples,
				},
			};
		} finally {
			await vm.dispose();
		}
	} finally {
		rmSync(hostRoot, { recursive: true, force: true });
	}
}

function addDerivedFields(cases: FsSyncCaseResult[]): void {
	for (const result of cases) {
		const single = cases.find(
			(candidate) =>
				candidate.op === result.op &&
				candidate.fixture === result.fixture &&
				candidate.payloadBytes === result.payloadBytes &&
				candidate.callCount === 1,
		);
		if (!single) continue;
		const denominator = Math.max(result.callCount - 1, 1);
		const rpcDenominator = Math.max(result.expectedSyncRpcCalls - single.expectedSyncRpcCalls, 1);
		result.derived = {
			guestP50MinusSingleMs: round(result.guest.p50 - single.guest.p50),
			hostP50MinusSingleMs: round(result.host.p50 - single.host.p50),
			guestIncrementalMsPerLogicalOp:
				result.callCount > 1 ? round((result.guest.p50 - single.guest.p50) / denominator) : undefined,
			guestIncrementalMsPerExpectedSyncRpc:
				result.callCount > 1 ? round((result.guest.p50 - single.guest.p50) / rpcDenominator) : undefined,
		};
	}
}

async function runCaseWithOptionalLatency(args: {
	op: FsSyncOp;
	fixture: FsSyncFixture;
	callCount: number;
	payloadBytes: number;
	iterations: number;
	warmup: number;
	sharedSidecar: SidecarProcess | null;
	syncRpcLatencyEnabled: boolean;
	fsSyncPhasesEnabled: boolean;
}): Promise<FsSyncCaseResult> {
	const needsDedicatedSidecar = args.syncRpcLatencyEnabled || args.fsSyncPhasesEnabled;
	if (!needsDedicatedSidecar) {
		if (!args.sharedSidecar) throw new Error("shared sidecar required when tracing is disabled");
		return runCase(
			args.op,
			args.fixture,
			args.callCount,
			args.payloadBytes,
			args.iterations,
			args.warmup,
			args.sharedSidecar,
		);
	}

	const latencyFile = join(
		tmpdir(),
		`agentos-fs-sync-lat-${args.op}-${args.callCount}-${Date.now()}-${Math.random()
			.toString(16)
			.slice(2)}.txt`,
	);
	const phasesFile = join(
		tmpdir(),
		`agentos-fs-sync-phases-${args.op}-${args.callCount}-${Date.now()}-${Math.random()
			.toString(16)
			.slice(2)}.txt`,
	);
	const previousEnabled = process.env.AGENTOS_SYNCRPC_LAT;
	const previousFile = process.env.AGENTOS_SYNCRPC_LAT_FILE;
	const previousPhasesEnabled = process.env.AGENTOS_FS_SYNC_PHASES;
	const previousPhasesFile = process.env.AGENTOS_FS_SYNC_PHASES_FILE;
	if (args.syncRpcLatencyEnabled) {
		process.env.AGENTOS_SYNCRPC_LAT = "1";
		process.env.AGENTOS_SYNCRPC_LAT_FILE = latencyFile;
	}
	if (args.fsSyncPhasesEnabled) {
		process.env.AGENTOS_FS_SYNC_PHASES = "1";
		process.env.AGENTOS_FS_SYNC_PHASES_FILE = phasesFile;
	}
	const sidecar = await createBenchSidecar();
	try {
		return await runCase(
			args.op,
			args.fixture,
			args.callCount,
			args.payloadBytes,
			args.iterations,
			args.warmup,
			sidecar,
			args.syncRpcLatencyEnabled ? latencyFile : undefined,
			args.fsSyncPhasesEnabled ? phasesFile : undefined,
		);
	} finally {
		await sidecar.dispose();
		if (previousEnabled === undefined) delete process.env.AGENTOS_SYNCRPC_LAT;
		else process.env.AGENTOS_SYNCRPC_LAT = previousEnabled;
		if (previousFile === undefined) delete process.env.AGENTOS_SYNCRPC_LAT_FILE;
		else process.env.AGENTOS_SYNCRPC_LAT_FILE = previousFile;
		if (previousPhasesEnabled === undefined) delete process.env.AGENTOS_FS_SYNC_PHASES;
		else process.env.AGENTOS_FS_SYNC_PHASES = previousPhasesEnabled;
		if (previousPhasesFile === undefined) delete process.env.AGENTOS_FS_SYNC_PHASES_FILE;
		else process.env.AGENTOS_FS_SYNC_PHASES_FILE = previousPhasesFile;
		rmSync(latencyFile, { force: true });
		rmSync(phasesFile, { force: true });
	}
}

async function main(): Promise<void> {
	const {
		iterations,
		warmup,
		ops,
		callCounts,
		fixtures,
		payloadBytes,
		syncRpcLatencyEnabled,
		fsSyncPhasesEnabled,
	} = parseArgs();
	const hardware = getHardware();
	console.error("=== FS Sync Ops Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(
		`Iterations: ${iterations} (+ ${warmup} warmup), ops: ${ops.join(",")}, callCounts: ${callCounts.join(",")}, fixtures: ${fixtures.join(",")}, payloadBytes: ${payloadBytes}, syncRpcLatency: ${syncRpcLatencyEnabled}, fsSyncPhases: ${fsSyncPhasesEnabled}`,
	);

	const sharedSidecar =
		syncRpcLatencyEnabled || fsSyncPhasesEnabled ? null : await createBenchSidecar();
	try {
		const cases: FsSyncCaseResult[] = [];
		for (const fixture of fixtures) {
			for (const op of ops) {
				for (const callCount of callCounts) {
					const result = await runCaseWithOptionalLatency({
						op,
						fixture,
						callCount,
						payloadBytes,
						iterations,
						warmup,
						sharedSidecar,
						syncRpcLatencyEnabled,
						fsSyncPhasesEnabled,
					});
					cases.push(result);
					console.error(
						`  op=${op} calls=${callCount}: expectedRpc=${result.expectedSyncRpcCalls} host.p50=${result.host.p50}ms guest.p50=${result.guest.p50}ms ratio=${result.guestVsHostRatio}x guest.ms/rpc=${result.guestMsPerExpectedSyncRpc}`,
					);
				}
			}
		}
		addDerivedFields(cases);
		printTable(
			[
				"op",
				"calls",
				"expected RPCs",
				"host p50",
				"guest p50",
				"guest/host",
				"guest ms/op",
				"guest ms/rpc",
			],
			cases.map((result) => [
				result.op,
				result.callCount,
				result.expectedSyncRpcCalls,
				`${result.host.p50}ms`,
				`${result.guest.p50}ms`,
				`${result.guestVsHostRatio}x`,
				`${result.guestMsPerLogicalOp}ms`,
				result.guestMsPerExpectedSyncRpc === undefined
					? "n/a"
					: `${result.guestMsPerExpectedSyncRpc}ms`,
			]),
		);
		console.log(
			JSON.stringify(
				{
					benchmark: "fs-sync-ops",
					generatedAt: new Date().toISOString(),
					hardware,
					iterations,
					warmup,
					ops,
					callCounts,
					fixtures,
					payloadBytes,
					syncRpcLatencyEnabled,
					fsSyncPhasesEnabled,
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
