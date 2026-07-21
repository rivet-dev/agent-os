/**
 * Differential 3-layer process-spawn benchmark (PoC for the fuzz-perf harness).
 *
 * Primary op `node_exit` = spawn a process that runs `node -e 'process.exit(0)'` and
 * wait for it to exit, measured at three layers so the ratios are honest:
 *
 *   native floor   real host node via Rust std::process   (native-baseline binary)
 *   node reference  real host node via Node child_process  (in-process, host)
 *   guest           node inside a agentos VM (a fresh V8 isolate per spawn)
 *
 *   emulation_tax = guest.p50 / node.p50   <- what agentos's emulation adds
 *   total_tax     = guest.p50 / native.p50 <- total cost over the raw host floor
 *
 * The guest layer hits the V8-isolate-per-spawn path that the surface map flagged as
 * the dominant process cost (a new isolate + 2 OS threads + snapshot deserialize per
 * spawn). We compare it against a real host node process doing the same logical op, so
 * the tax reflects emulation overhead rather than "JS startup is slow".
 *
 * For absolute-floor context we also print the native cost of a libc `sh -c 'exit 0'`
 * (op `spawn_exit`) — ~0.3 ms — to show how far the node-spawn floor already sits above
 * the cheapest possible process. The WASM-shell guest path (sh/echo) is a future
 * addition once the registry WASM binaries are built.
 *
 * Run:
 *   pnpm exec tsx scripts/benchmarks/process-spawn.bench.ts
 *   BENCH_ITERATIONS=5 BENCH_WARMUP=2 pnpm exec tsx scripts/benchmarks/process-spawn.bench.ts  # smoke
 *
 * Low-variance discipline: pin a core (taskset -c) and pass --expose-gc; one shared VM
 * is reused across all guest iters (no per-iter VM boot). Trust p50/p95/p99, not mean.
 */

import { type ChildProcess, spawn, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createBenchSidecar, createBenchVm, type BenchVm } from "../lib/vm.js";
import type { SidecarProcess } from "@rivet-dev/agentos-runtime-core";
import {
	forceGC,
	getHardware,
	printTable,
	round,
	stats,
} from "../lib/perf-utils.js";
import { runNativeLayer } from "../lib/native.js";
import { runNativePhaseLayer } from "../lib/native.js";

const ITERATIONS = Number(process.env.BENCH_ITERATIONS ?? 200);
const WARMUP = Number(process.env.BENCH_WARMUP ?? 20);
const PROCESS_LIFECYCLE_TRACE =
	process.env.BENCH_PROCESS_LIFECYCLE_TRACE === "1";

const NODE_ARGS = ["-e", "process.exit(0)"];
const FANOUT = 8;
const FANOUT_LADDER = [1, 8, 32] as const;

type PhaseName = "total" | "spawn" | "wait_reap" | "spawn_batch" | "wait_reap_batch";
type PhaseSamples = Partial<Record<PhaseName, number[]>>;

interface ProcessLifecycleTrace {
	version: "BENCH-035" | "BENCH-069";
	pid: number;
	processId: string;
	command: string;
	args: string[];
	firstWaiterSource?: string;
	lastWaiterSource?: string;
	waiterSources?: Record<string, number>;
	spawnToWaitBeginMs?: number;
	waitBlockedOnStartMs?: number;
	waitTotalMs?: number;
	trailingOutputDrainMs?: number;
	finishRoute?: string;
	start?: Record<string, number | boolean | undefined>;
	wait?: Record<string, number | boolean | string | undefined>;
	exit?: Record<string, number | undefined>;
		output?: Record<string, number | undefined>;
	}

interface SidecarExecutePhaseMetric {
	stage: string;
	calls: number;
	totalUs: number;
	avgUs: number;
	maxUs: number;
}

interface HostWriteSyncRow {
	row: "root_cwd_no_write" | "host_cwd_no_write" | "host_cwd_write_1file";
	cwd: string;
	writesPerProcess: number;
	samples: number;
	wallMs: ReturnType<typeof stats>;
	sidecarExecutePhases: SidecarExecutePhaseMetric[];
	cleanSkipCalls: number;
	hostWriteVisible?: boolean;
}

interface FanoutLadderRow {
	fanout: number;
	samples: number;
	totalMs: ReturnType<typeof stats>;
	spawnBatchMs: ReturnType<typeof stats>;
	waitReapBatchMs: ReturnType<typeof stats>;
	perProcessWaitReapP50Ms: number;
	sidecarExecutePhases: SidecarExecutePhaseMetric[];
}

interface NestedChildProcessRow {
	row: "guest_node_child_process_8";
	fanout: number;
	samples: number;
	warmup: number;
	command: string;
	nodeParentMs: ReturnType<typeof stats>;
	guestParentMs: ReturnType<typeof stats>;
	guestVsNodeP50: number;
	guestMinusNodeP50Ms: number;
	parentObserved: NestedChildProcessObservationSummary;
	expectedLifecycleProcessesPerIteration: number;
	expectedSidecarProcessExecutions: number;
	expectedPublicParentExecutions: number;
	expectedNestedChildProcessExecutions: number;
	sidecarExecutePhases: SidecarExecutePhaseMetric[];
}

interface NestedChildProcessObservation {
	expectedChildren: number;
	exitCallbacks: number;
	closeCallbacks: number;
	errorCallbacks: number;
	nonZeroExitCodes: number;
	nonZeroCloseCodes: number;
}

interface NestedChildProcessObservationSummary extends NestedChildProcessObservation {
	samples: number;
}

interface NestedChildProcessLayerResult {
	ms: ReturnType<typeof stats>;
	observed: NestedChildProcessObservationSummary;
}

function nowMs(start: bigint): number {
	return Number(process.hrtime.bigint() - start) / 1e6;
}

function waitNodeChild(child: ChildProcess): Promise<void> {
	return new Promise((resolve, reject) => {
		child.on("error", reject);
		child.on("exit", (code) =>
			code === 0 ? resolve() : reject(new Error(`node exited ${code}`)),
		);
	});
}

/** Node reference layer: spawn a real host node that exits, timed in-process. */
function runNodeLayer(): number[] {
	const samples: number[] = [];
	for (let i = 0; i < WARMUP + ITERATIONS; i++) {
		const t = process.hrtime.bigint();
		const r = spawnSync("node", NODE_ARGS, { stdio: "ignore" });
		const ms = nowMs(t);
		if (r.status !== 0) {
			throw new Error(`node layer: node exited ${r.status}`);
		}
		if (i >= WARMUP) samples.push(ms);
	}
	return samples;
}

/** Guest layer: spawn node inside one reused VM (a fresh V8 isolate per spawn). */
async function runGuestLayer(sidecar: SidecarProcess): Promise<number[]> {
	const vm = await createBenchVm({ sidecar });
	try {
		for (let i = 0; i < WARMUP; i++) {
			const p = vm.spawn("node", NODE_ARGS);
			const code = await vm.waitProcess(p.pid);
			if (code !== 0) throw new Error(`guest warmup: exit ${code}`);
		}
		const samples: number[] = [];
		for (let i = 0; i < ITERATIONS; i++) {
			const t = process.hrtime.bigint();
			const p = vm.spawn("node", NODE_ARGS);
			await vm.waitProcess(p.pid);
			samples.push(nowMs(t));
		}
		return samples;
	} finally {
		await vm.dispose();
	}
}

async function runNodeExitPhases(): Promise<PhaseSamples> {
	const samples: PhaseSamples = { total: [], spawn: [], wait_reap: [] };
	for (let i = 0; i < WARMUP + ITERATIONS; i++) {
		const totalStart = process.hrtime.bigint();
		const spawnStart = process.hrtime.bigint();
		const child = spawn("node", NODE_ARGS, { stdio: "ignore" });
		const spawnMs = nowMs(spawnStart);
		const waitStart = process.hrtime.bigint();
		await waitNodeChild(child);
		const waitMs = nowMs(waitStart);
		const totalMs = nowMs(totalStart);
		if (i >= WARMUP) {
			samples.total?.push(totalMs);
			samples.spawn?.push(spawnMs);
			samples.wait_reap?.push(waitMs);
		}
	}
	return samples;
}

async function runNodeFanoutPhases(): Promise<PhaseSamples> {
	const samples: PhaseSamples = {
		total: [],
		spawn_batch: [],
		wait_reap_batch: [],
	};
	for (let i = 0; i < WARMUP + ITERATIONS; i++) {
		const totalStart = process.hrtime.bigint();
		const spawnStart = process.hrtime.bigint();
		const children = Array.from({ length: FANOUT }, () =>
			spawn("node", NODE_ARGS, { stdio: "ignore" }),
		);
		const spawnMs = nowMs(spawnStart);
		const waitStart = process.hrtime.bigint();
		await Promise.all(children.map(waitNodeChild));
		const waitMs = nowMs(waitStart);
		const totalMs = nowMs(totalStart);
		if (i >= WARMUP) {
			samples.total?.push(totalMs);
			samples.spawn_batch?.push(spawnMs);
			samples.wait_reap_batch?.push(waitMs);
		}
	}
	return samples;
}

async function runGuestNodeExitPhases(sidecar: SidecarProcess): Promise<PhaseSamples> {
	const samples: PhaseSamples = { total: [], spawn: [], wait_reap: [] };
	const vm = await createBenchVm({ sidecar });
	try {
		for (let i = 0; i < WARMUP + ITERATIONS; i++) {
			const totalStart = process.hrtime.bigint();
			const spawnStart = process.hrtime.bigint();
			const child = vm.spawn("node", NODE_ARGS);
			const spawnMs = nowMs(spawnStart);
			const waitStart = process.hrtime.bigint();
			const code = await vm.waitProcess(child.pid);
			const waitMs = nowMs(waitStart);
			if (code !== 0) throw new Error(`guest phase node_exit: exit ${code}`);
			const totalMs = nowMs(totalStart);
			if (i >= WARMUP) {
				samples.total?.push(totalMs);
				samples.spawn?.push(spawnMs);
				samples.wait_reap?.push(waitMs);
			}
		}
		return samples;
	} finally {
		await vm.dispose();
	}
}

async function runGuestFanoutPhases(sidecar: SidecarProcess): Promise<PhaseSamples> {
	const samples: PhaseSamples = {
		total: [],
		spawn_batch: [],
		wait_reap_batch: [],
	};
	const vm = await createBenchVm({ sidecar });
	try {
		for (let i = 0; i < WARMUP + ITERATIONS; i++) {
			const totalStart = process.hrtime.bigint();
			const spawnStart = process.hrtime.bigint();
			const children = Array.from({ length: FANOUT }, () =>
				vm.spawn("node", NODE_ARGS),
			);
			const spawnMs = nowMs(spawnStart);
			const waitStart = process.hrtime.bigint();
			const codes = await Promise.all(
				children.map((child) => vm.waitProcess(child.pid)),
			);
			const waitMs = nowMs(waitStart);
			const badCode = codes.find((code) => code !== 0);
			if (badCode !== undefined) {
				throw new Error(`guest phase fanout: exit ${badCode}`);
			}
			const totalMs = nowMs(totalStart);
			if (i >= WARMUP) {
				samples.total?.push(totalMs);
				samples.spawn_batch?.push(spawnMs);
				samples.wait_reap_batch?.push(waitMs);
			}
		}
		return samples;
	} finally {
		await vm.dispose();
	}
}

async function runGuestFanoutLadderCase(
	sidecar: SidecarProcess,
	executePhasesFile: string,
	fanout: number,
): Promise<FanoutLadderRow> {
	const beforePhases = readSidecarExecutePhases(executePhasesFile);
	const vm = await createBenchVm({ sidecar });
	try {
		const totalSamples: number[] = [];
		const spawnBatchSamples: number[] = [];
		const waitReapBatchSamples: number[] = [];
		for (let i = 0; i < WARMUP + ITERATIONS; i++) {
			const totalStart = process.hrtime.bigint();
			const spawnStart = process.hrtime.bigint();
			const children = Array.from({ length: fanout }, () =>
				vm.spawn("node", NODE_ARGS),
			);
			const spawnMs = nowMs(spawnStart);
			const waitStart = process.hrtime.bigint();
			const codes = await Promise.all(
				children.map((child) => vm.waitProcess(child.pid)),
			);
			const waitMs = nowMs(waitStart);
			const badCode = codes.find((code) => code !== 0);
			if (badCode !== undefined) {
				throw new Error(`guest fanout ladder ${fanout}: exit ${badCode}`);
			}
			const totalMs = nowMs(totalStart);
			if (i >= WARMUP) {
				totalSamples.push(totalMs);
				spawnBatchSamples.push(spawnMs);
				waitReapBatchSamples.push(waitMs);
			}
		}
		const waitReapBatch = stats(waitReapBatchSamples);
		return {
			fanout,
			samples: totalSamples.length,
			totalMs: stats(totalSamples),
			spawnBatchMs: stats(spawnBatchSamples),
			waitReapBatchMs: waitReapBatch,
			perProcessWaitReapP50Ms: round(waitReapBatch.p50 / fanout),
			sidecarExecutePhases: diffSidecarExecutePhases(
				beforePhases,
				readSidecarExecutePhases(executePhasesFile),
			),
		};
	} finally {
		await vm.dispose();
	}
}

async function runGuestFanoutLadderRows(
	sidecar: SidecarProcess,
	executePhasesFile: string,
): Promise<FanoutLadderRow[]> {
	const rows: FanoutLadderRow[] = [];
	for (const fanout of FANOUT_LADDER) {
		rows.push(await runGuestFanoutLadderCase(sidecar, executePhasesFile, fanout));
	}
	return rows;
}

function nestedChildProcessScript(fanout: number): string {
	return `
const { spawn } = require("node:child_process");
const fanout = ${fanout};
let exits = 0;
let closes = 0;
let errors = 0;
let nonZeroExitCodes = 0;
let nonZeroCloseCodes = 0;
let failed = false;
let done = false;
const timeout = setTimeout(() => fail("nested child_process fanout timed out"), 5000);
function fail(message) {
  if (failed) return;
  failed = true;
  clearTimeout(timeout);
  console.error(message);
  process.exit(1);
}
function maybeDone() {
  if (!done && closes === fanout) {
    done = true;
    clearTimeout(timeout);
    console.log("__BENCH098__" + JSON.stringify({
      expectedChildren: fanout,
      exitCallbacks: exits,
      closeCallbacks: closes,
      errorCallbacks: errors,
      nonZeroExitCodes,
      nonZeroCloseCodes,
    }));
  }
}
for (let i = 0; i < fanout; i++) {
  const child = spawn("node", ["-e", "process.exit(0)"], { stdio: "ignore" });
  child.on("error", (error) => {
    errors++;
    fail(error && error.message ? error.message : String(error));
  });
  child.on("exit", (code, signal) => {
    if (code !== 0) nonZeroExitCodes++;
    exits++;
    maybeDone();
  });
  child.on("close", (code, signal) => {
    if (code !== 0) nonZeroCloseCodes++;
    closes++;
    maybeDone();
  });
}
`;
}

function parseNestedChildProcessObservations(
	stdout: string,
	fanout: number,
	context: string,
): NestedChildProcessObservation[] {
	const observations = stdout
		.split(/\r?\n/)
		.filter((line) => line.startsWith("__BENCH098__"))
		.map((line) => JSON.parse(line.slice("__BENCH098__".length)) as NestedChildProcessObservation);
	if (observations.length !== 1) {
		throw new Error(`${context}: expected one BENCH-098 marker, got ${observations.length}`);
	}
	assertNestedChildProcessObservation(observations[0], fanout, context);
	return observations;
}

function assertNestedChildProcessObservation(
	observation: NestedChildProcessObservation,
	fanout: number,
	context: string,
): void {
	if (
		observation.expectedChildren !== fanout ||
		observation.closeCallbacks !== fanout ||
		observation.errorCallbacks !== 0 ||
		observation.nonZeroExitCodes !== 0 ||
		observation.nonZeroCloseCodes !== 0 ||
		observation.exitCallbacks < 0 ||
		observation.exitCallbacks > fanout
	) {
		throw new Error(
			`${context}: invalid nested child_process marker ${JSON.stringify(observation)}`,
		);
	}
}

function summarizeNestedChildProcessObservations(
	observations: NestedChildProcessObservation[],
): NestedChildProcessObservationSummary {
	return {
		samples: observations.length,
		expectedChildren: observations[0]?.expectedChildren ?? 0,
		exitCallbacks: observations.reduce(
			(sum, observation) => sum + observation.exitCallbacks,
			0,
		),
		closeCallbacks: observations.reduce(
			(sum, observation) => sum + observation.closeCallbacks,
			0,
		),
		errorCallbacks: observations.reduce((sum, observation) => sum + observation.errorCallbacks, 0),
		nonZeroExitCodes: observations.reduce(
			(sum, observation) => sum + observation.nonZeroExitCodes,
			0,
		),
		nonZeroCloseCodes: observations.reduce(
			(sum, observation) => sum + observation.nonZeroCloseCodes,
			0,
		),
	};
}

function runNodeNestedChildProcessLayer(
	script: string,
	fanout: number,
): NestedChildProcessLayerResult {
	const samples: number[] = [];
	const observations: NestedChildProcessObservation[] = [];
	for (let i = 0; i < WARMUP + ITERATIONS; i++) {
		const t = process.hrtime.bigint();
		const r = spawnSync("node", ["-e", script], {
			encoding: "utf8",
			stdio: ["ignore", "pipe", "pipe"],
		});
		const ms = nowMs(t);
		if (r.status !== 0) {
			throw new Error(
				`node nested child_process: node exited ${r.status}: ${r.stderr}`,
			);
		}
		const marker = parseNestedChildProcessObservations(
			r.stdout,
			fanout,
			`node nested child_process iteration ${i}`,
		)[0];
		if (i >= WARMUP) {
			observations.push(marker);
			samples.push(ms);
		}
	}
	return {
		ms: stats(samples),
		observed: summarizeNestedChildProcessObservations(observations),
	};
}

async function runGuestNestedChildProcessLayer(
	sidecar: SidecarProcess,
	script: string,
	fanout: number,
): Promise<NestedChildProcessLayerResult> {
	const vm = await createBenchVm({ sidecar });
	try {
		const samples: number[] = [];
		const observations: NestedChildProcessObservation[] = [];
		for (let i = 0; i < WARMUP + ITERATIONS; i++) {
			const stdoutChunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const t = process.hrtime.bigint();
			const proc = vm.spawn("node", ["-e", script], {
				onStdout: (chunk) => stdoutChunks.push(chunk),
				onStderr: (chunk) => stderrChunks.push(chunk),
			});
			const code = await vm.waitProcess(proc.pid);
			const ms = nowMs(t);
			const stdout = Buffer.concat(stdoutChunks.map((chunk) => Buffer.from(chunk))).toString(
				"utf8",
			);
			const stderr = Buffer.concat(stderrChunks.map((chunk) => Buffer.from(chunk))).toString(
				"utf8",
			);
			if (code !== 0) {
				throw new Error(`guest nested child_process: parent exited ${code}: ${stderr}`);
			}
			const marker = parseNestedChildProcessObservations(
				stdout,
				fanout,
				`guest nested child_process iteration ${i}`,
			)[0];
			if (i >= WARMUP) {
				observations.push(marker);
				samples.push(ms);
			}
		}
		return {
			ms: stats(samples),
			observed: summarizeNestedChildProcessObservations(observations),
		};
	} finally {
		await vm.dispose();
	}
}

function assertNestedChildProcessPhaseCoverage(
	row: NestedChildProcessRow,
): void {
	if (
		row.parentObserved.closeCallbacks !== row.samples * row.fanout ||
		row.parentObserved.errorCallbacks !== 0 ||
		row.parentObserved.nonZeroCloseCodes !== 0 ||
		row.parentObserved.nonZeroExitCodes !== 0
	) {
		throw new Error(
			`nested child_process row invalid aggregate marker ${JSON.stringify(
				row.parentObserved,
			)}`,
		);
	}
	const requiredPublicStages = [
		"kernel_spawn_process",
		"process_register_and_lifecycle",
		"process_exit_cleanup",
		"process_exit_cleanup_wait_and_reap",
		"execute_response_to_exit_event_queued",
	];
	for (const stage of requiredPublicStages) {
		const calls = phaseCalls(row, stage);
		if (calls < row.expectedPublicParentExecutions) {
			throw new Error(
				`nested child_process row expected at least ${row.expectedPublicParentExecutions} parent ${stage} calls, got ${calls}`,
			);
		}
	}
	const requiredChildStages = [
		"child_process_spawn_total",
		"child_process_spawn_and_start_execution",
		"child_process_register",
		"child_process_exit_cleanup",
	];
	for (const stage of requiredChildStages) {
		const calls = phaseCalls(row, stage);
		if (calls < row.expectedNestedChildProcessExecutions) {
			throw new Error(
				`nested child_process row expected at least ${row.expectedNestedChildProcessExecutions} nested ${stage} calls, got ${calls}`,
			);
		}
	}
}

async function runNestedChildProcessRows(
	sidecar: SidecarProcess,
	executePhasesFile: string,
): Promise<NestedChildProcessRow[]> {
	const fanout = FANOUT;
	const script = nestedChildProcessScript(fanout);
	const nodeParent = runNodeNestedChildProcessLayer(script, fanout);
	const beforePhases = readSidecarExecutePhases(executePhasesFile);
	const guestParent = await runGuestNestedChildProcessLayer(sidecar, script, fanout);
	const totalIterations = WARMUP + ITERATIONS;
	const row = {
		row: "guest_node_child_process_8" as const,
		fanout,
		samples: ITERATIONS,
		warmup: WARMUP,
		command: "node parent -> child_process.spawn(node -e process.exit(0)) x8",
		nodeParentMs: nodeParent.ms,
		guestParentMs: guestParent.ms,
		guestVsNodeP50: round(guestParent.ms.p50 / nodeParent.ms.p50),
		guestMinusNodeP50Ms: round(guestParent.ms.p50 - nodeParent.ms.p50),
		parentObserved: guestParent.observed,
		expectedLifecycleProcessesPerIteration: fanout + 1,
		expectedSidecarProcessExecutions: totalIterations * (fanout + 1),
		expectedPublicParentExecutions: totalIterations,
		expectedNestedChildProcessExecutions: totalIterations * fanout,
		sidecarExecutePhases: diffSidecarExecutePhases(
			beforePhases,
			readSidecarExecutePhases(executePhasesFile),
		),
	};
	assertNestedChildProcessPhaseCoverage(row);
	return [row];
}

async function runGuestHostWriteSyncCase(
	sidecar: SidecarProcess,
	executePhasesFile: string,
	row: HostWriteSyncRow["row"],
	options: {
		cwd: string;
		args: string[];
		mountHostDir?: string;
		writesPerProcess: number;
		verifyHostFile?: string;
	},
): Promise<HostWriteSyncRow> {
	const beforePhases = readSidecarExecutePhases(executePhasesFile);
	const vm = await createBenchVm({
		sidecar,
		mounts: options.mountHostDir
			? [
					{
						guestPath: options.cwd,
						hostPath: options.mountHostDir,
						readOnly: false,
					},
				]
			: undefined,
	});
	try {
		const samples: number[] = [];
		for (let i = 0; i < WARMUP + ITERATIONS; i++) {
			const t = process.hrtime.bigint();
			const proc = vm.spawn("node", options.args, { cwd: options.cwd });
			const code = await vm.waitProcess(proc.pid);
			const ms = nowMs(t);
			if (code !== 0) throw new Error(`guest host-write-sync ${row}: exit ${code}`);
			if (i >= WARMUP) samples.push(ms);
		}
		const afterPhases = readSidecarExecutePhases(executePhasesFile);
		let hostWriteVisible: boolean | undefined;
		if (options.verifyHostFile) {
			try {
				hostWriteVisible = readFileSync(options.verifyHostFile, "utf8").length > 0;
			} catch {
				hostWriteVisible = false;
			}
		}
		const sidecarExecutePhases = diffSidecarExecutePhases(beforePhases, afterPhases);
		const cleanSkipCalls =
			sidecarExecutePhases.find(
				(metric) => metric.stage === "process_exit_cleanup_sync_host_writes_clean_skip",
			)?.calls ?? 0;
		return {
			row,
			cwd: options.cwd,
			writesPerProcess: options.writesPerProcess,
			samples: samples.length,
			wallMs: stats(samples),
			sidecarExecutePhases,
			cleanSkipCalls,
			hostWriteVisible,
		};
	} finally {
		await vm.dispose();
	}
}

async function runGuestHostWriteSyncRows(
	sidecar: SidecarProcess,
	executePhasesFile: string,
): Promise<HostWriteSyncRow[]> {
	const hostRoot = mkdtempSync(join(tmpdir(), "agentos-host-write-sync-"));
	const mountedNoWrite = join(hostRoot, "mounted-no-write");
	const mountedWrite = join(hostRoot, "mounted-write");
	mkdirSync(mountedNoWrite);
	mkdirSync(mountedWrite);
	writeFileSync(join(hostRoot, ".keep"), "");
	writeFileSync(join(mountedNoWrite, ".seed"), "", { flag: "wx" });
	writeFileSync(join(mountedWrite, ".seed"), "", { flag: "wx" });
	const writeTarget = join(mountedWrite, "bench-write.txt");
	try {
		return [
			await runGuestHostWriteSyncCase(sidecar, executePhasesFile, "root_cwd_no_write", {
				cwd: "/",
				args: NODE_ARGS,
				writesPerProcess: 0,
			}),
			await runGuestHostWriteSyncCase(
				sidecar,
				executePhasesFile,
				"host_cwd_no_write",
				{
					cwd: "/hostsync",
					args: NODE_ARGS,
					mountHostDir: mountedNoWrite,
					writesPerProcess: 0,
				},
			),
			await runGuestHostWriteSyncCase(
				sidecar,
				executePhasesFile,
				"host_cwd_write_1file",
				{
					cwd: "/hostsync",
					args: [
						"-e",
						"require('node:fs').writeFileSync('bench-write.txt', 'x'); process.exit(0)",
					],
					mountHostDir: mountedWrite,
					writesPerProcess: 1,
					verifyHostFile: writeTarget,
				},
			),
		];
	} finally {
		rmSync(hostRoot, { recursive: true, force: true });
	}
}

function assertHostWriteSyncRows(rows: HostWriteSyncRow[]): void {
	for (const row of rows) {
		if (row.writesPerProcess === 0 && row.cleanSkipCalls === 0) {
			throw new Error(`expected clean host-write sync skip for ${row.row}`);
		}
		if (row.writesPerProcess > 0 && row.cleanSkipCalls > 0) {
			throw new Error(`dirty host-write row unexpectedly skipped sync: ${row.row}`);
		}
		if (row.writesPerProcess > 0 && row.hostWriteVisible !== true) {
			throw new Error(`dirty host-write row did not leave visible host write: ${row.row}`);
		}
	}
}

function sampleStats(samples: number[] | undefined) {
	if (!samples || samples.length === 0) {
		throw new Error("missing phase samples");
	}
	return stats(samples);
}

function readLifecycleTraces(path: string, skip: number): ProcessLifecycleTrace[] {
	try {
		const text = readFileSync(path, "utf8").trim();
		if (!text) return [];
		return text
			.split(/\r?\n/)
			.slice(skip)
			.map((line) => JSON.parse(line) as ProcessLifecycleTrace);
	} catch {
		return [];
	}
}

function resetLifecycleTraceFile(path: string): void {
	writeFileSync(path, "");
}

function readSidecarExecutePhases(path: string): SidecarExecutePhaseMetric[] {
	try {
		const text = readFileSync(path, "utf8").trim();
		if (!text) return [];
		const metrics: SidecarExecutePhaseMetric[] = [];
		for (const line of text.split(/\r?\n/)) {
			const match =
				/^stage=(\S+) calls=(\d+) total_us=(\d+) avg_us=(\d+) max_us=(\d+)$/.exec(
					line,
				);
			if (!match) continue;
			metrics.push({
				stage: match[1],
				calls: Number(match[2]),
				totalUs: Number(match[3]),
				avgUs: Number(match[4]),
				maxUs: Number(match[5]),
			});
		}
		return metrics;
	} catch {
		return [];
	}
}

function diffSidecarExecutePhases(
	before: SidecarExecutePhaseMetric[],
	after: SidecarExecutePhaseMetric[],
): SidecarExecutePhaseMetric[] {
	const beforeByStage = new Map(before.map((metric) => [metric.stage, metric]));
	const deltas: SidecarExecutePhaseMetric[] = [];
	for (const metric of after) {
		const previous = beforeByStage.get(metric.stage);
		const calls = metric.calls - (previous?.calls ?? 0);
		const totalUs = metric.totalUs - (previous?.totalUs ?? 0);
		if (calls <= 0 && totalUs <= 0) continue;
		deltas.push({
			stage: metric.stage,
			calls,
			totalUs,
			avgUs: calls > 0 ? Math.round(totalUs / calls) : 0,
			maxUs: metric.maxUs,
		});
	}
	return deltas;
}

function phaseAvgUs(
	row: { sidecarExecutePhases: SidecarExecutePhaseMetric[] },
	stage: string,
): number | "" {
	return row.sidecarExecutePhases.find((metric) => metric.stage === stage)?.avgUs ?? "";
}

function phaseCalls(
	row: { sidecarExecutePhases: SidecarExecutePhaseMetric[] },
	stage: string,
): number {
	return row.sidecarExecutePhases.find((metric) => metric.stage === stage)?.calls ?? 0;
}

function countBy(values: string[]): Record<string, number> {
	const counts: Record<string, number> = {};
	for (const value of values) {
		counts[value] = (counts[value] ?? 0) + 1;
	}
	return counts;
}

function sumCounts(
	records: Array<Record<string, number> | undefined>,
): Record<string, number> {
	const counts: Record<string, number> = {};
	for (const record of records) {
		if (!record) continue;
		for (const [key, value] of Object.entries(record)) {
			counts[key] = (counts[key] ?? 0) + value;
		}
	}
	return counts;
}

function metricStats(
	traces: ProcessLifecycleTrace[],
	getValue: (trace: ProcessLifecycleTrace) => unknown,
) {
	const samples = traces
		.map(getValue)
		.filter((value): value is number => typeof value === "number" && Number.isFinite(value));
	return samples.length === 0 ? null : stats(samples);
}

function diffMetric(
	traces: ProcessLifecycleTrace[],
	getEnd: (trace: ProcessLifecycleTrace) => unknown,
	getStart: (trace: ProcessLifecycleTrace) => unknown,
) {
	return metricStats(traces, (trace) => {
		const end = getEnd(trace);
		const start = getStart(trace);
		return typeof end === "number" && typeof start === "number"
			? end - start
			: undefined;
	});
}

function makeLifecycleRow(
	row: "node_exit_1" | "fanout_8",
	concurrency: number,
	traces: ProcessLifecycleTrace[],
) {
	return {
		row,
		phase: row === "node_exit_1" ? "wait_reap" : "wait_reap_batch",
		concurrency,
		command: "node -e process.exit(0)",
		samples: traces.length,
			resolutionCounts: countBy(
				traces.map((trace) =>
					String(trace.wait?.resolutionRoute ?? trace.finishRoute ?? "unknown"),
				),
			),
			firstWaiterSourceCounts: countBy(
				traces.map((trace) => trace.firstWaiterSource ?? "unknown"),
			),
			waiterSourceCounts: sumCounts(traces.map((trace) => trace.waiterSources)),
			subphases: {
			ts_spawn_to_wait_begin: metricStats(traces, (trace) => trace.spawnToWaitBeginMs),
			ts_wait_blocked_on_start: metricStats(traces, (trace) => trace.waitBlockedOnStartMs),
			ts_wait_total: metricStats(traces, (trace) => trace.waitTotalMs),
			ts_trailing_output_drain: metricStats(traces, (trace) => trace.trailingOutputDrainMs),
			ts_start_total: metricStats(traces, (trace) => trace.start?.totalMs),
			ts_execute_rpc: metricStats(traces, (trace) => trace.start?.executeMs),
			ts_signal_refresh: metricStats(traces, (trace) => trace.start?.signalRefreshMs),
			ts_mount_wait: metricStats(traces, (trace) => trace.start?.mountWaitMs),
			ts_exit_event_received: metricStats(
				traces,
				(trace) => trace.exit?.eventReceivedMs,
			),
			ts_exit_finish_begin: metricStats(
				traces,
				(trace) => trace.exit?.finishBeginMs,
			),
			ts_exit_event_to_finish: diffMetric(
				traces,
				(trace) => trace.exit?.finishBeginMs,
				(trace) => trace.exit?.eventReceivedMs,
			),
			ts_finish_snapshot_update: metricStats(
				traces,
				(trace) => trace.exit?.snapshotUpdateMs,
			),
			ts_finish_resolve_wait: metricStats(
				traces,
				(trace) => trace.exit?.resolveWaitMs,
			),
			ts_finish_total: metricStats(traces, (trace) => trace.exit?.finishTotalMs),
			ts_wait_promise_observed: metricStats(
				traces,
				(trace) => trace.exit?.waitPromiseObservedMs,
			),
			ts_finish_to_wait_observed: diffMetric(
				traces,
				(trace) => trace.exit?.waitPromiseObservedMs,
				(trace) => trace.exit?.finishBeginMs,
			),
			ts_wait_observed_to_return: diffMetric(
				traces,
				(trace) => trace.exit?.waitReturnMs,
				(trace) => trace.exit?.waitPromiseObservedMs,
			),
			ts_wait_return: metricStats(traces, (trace) => trace.exit?.waitReturnMs),
				ts_release_drain: metricStats(traces, (trace) => trace.exit?.releaseDrainMs),
				ts_release_total: metricStats(traces, (trace) => trace.exit?.releaseTotalMs),
				ts_output_events: metricStats(traces, (trace) => trace.output?.events),
				ts_output_signal_refresh: metricStats(
					traces,
					(trace) => trace.output?.signalRefreshMs,
				),
				ts_output_listener_dispatch: metricStats(
					traces,
					(trace) => trace.output?.listenerDispatchMs,
				),
				ts_snapshot_poll_sleep: metricStats(
					traces,
					(trace) => trace.wait?.snapshotPollSleepMs,
				),
			ts_snapshot_refresh: metricStats(traces, (trace) => trace.wait?.snapshotRefreshMs),
			ts_poll_iterations: metricStats(traces, (trace) => trace.wait?.pollIterations),
			ts_wait_unattributed_gap: metricStats(traces, (trace) => {
				const waitTotal = trace.waitTotalMs;
				if (typeof waitTotal !== "number") return undefined;
				const blocked =
					typeof trace.waitBlockedOnStartMs === "number"
						? trace.waitBlockedOnStartMs
						: 0;
				const pollSleep =
					typeof trace.wait?.snapshotPollSleepMs === "number"
						? trace.wait.snapshotPollSleepMs
						: 0;
				const snapshotRefresh =
					typeof trace.wait?.snapshotRefreshMs === "number"
						? trace.wait.snapshotRefreshMs
						: 0;
				return waitTotal - blocked - pollSleep - snapshotRefresh;
			}),
		},
	};
}

function makePhaseRow(
	row: "node_exit_1" | "fanout_8",
	phase: PhaseName,
	concurrency: number,
	nativeSamples: PhaseSamples,
	nodeSamples: PhaseSamples,
	guestSamples: PhaseSamples,
) {
	const native = sampleStats(nativeSamples[phase]);
	const node = sampleStats(nodeSamples[phase]);
	const guest = sampleStats(guestSamples[phase]);
	return {
		row,
		phase,
		concurrency,
		command: "node -e process.exit(0)",
		layers: { native, node, guest },
		guest_vs_node_p50: round(guest.p50 / node.p50),
		guest_vs_native_p50: round(guest.p50 / native.p50),
		guest_minus_node_p50_ms: round(guest.p50 - node.p50),
		guest_minus_native_p50_ms: round(guest.p50 - native.p50),
	};
}

async function main(): Promise<void> {
	const hardware = getHardware();
	console.error(
		`process-spawn 3-layer bench (node_exit) — iters=${ITERATIONS} warmup=${WARMUP}`,
	);
	console.error(`hardware: ${hardware.cpu} (${hardware.cores} cores)`);

	const native = stats(runNativeLayer("node_exit", ITERATIONS, WARMUP));
	const node = stats(runNodeLayer());
	forceGC();
	let guest;
	let phaseRows;
	let processLifecycle:
		| {
				version: "BENCH-035" | "BENCH-069";
				enabled: true;
				rows: ReturnType<typeof makeLifecycleRow>[];
				sidecarExecutePhases?: SidecarExecutePhaseMetric[];
				sidecarJsStartPhases?: SidecarExecutePhaseMetric[];
				sidecarJsEventPhases?: SidecarExecutePhaseMetric[];
				hostWriteSyncRows?: HostWriteSyncRow[];
				fanoutLadderRows?: FanoutLadderRow[];
				nestedChildProcessRows?: NestedChildProcessRow[];
			}
		| null = null;
	const lifecycleTraceFile = join(
		tmpdir(),
		`agentos-process-lifecycle-${Date.now()}-${Math.random()
			.toString(16)
			.slice(2)}.jsonl`,
	);
	const executePhasesFile = join(
		tmpdir(),
		`agentos-execute-phases-${Date.now()}-${Math.random()
			.toString(16)
			.slice(2)}.txt`,
	);
	const jsStartPhasesFile = join(
		tmpdir(),
		`agentos-js-start-phases-${Date.now()}-${Math.random()
			.toString(16)
			.slice(2)}.txt`,
	);
	const jsEventPhasesFile = join(
		tmpdir(),
		`agentos-js-event-phases-${Date.now()}-${Math.random()
			.toString(16)
			.slice(2)}.txt`,
	);
	const previousLifecycleTrace = process.env.AGENTOS_PROCESS_LIFECYCLE_TRACE;
	const previousLifecycleTraceFile =
		process.env.AGENTOS_PROCESS_LIFECYCLE_TRACE_FILE;
	const previousExecutePhases = process.env.AGENTOS_EXECUTE_PHASES;
	const previousExecutePhasesFile = process.env.AGENTOS_EXECUTE_PHASES_FILE;
	const previousJsStartPhases = process.env.AGENTOS_JS_START_PHASES;
	const previousJsStartPhasesFile = process.env.AGENTOS_JS_START_PHASES_FILE;
	const previousJsEventPhases = process.env.AGENTOS_JS_EVENT_PHASES;
	const previousJsEventPhasesFile = process.env.AGENTOS_JS_EVENT_PHASES_FILE;
	if (PROCESS_LIFECYCLE_TRACE) {
		process.env.AGENTOS_PROCESS_LIFECYCLE_TRACE = "1";
		process.env.AGENTOS_PROCESS_LIFECYCLE_TRACE_FILE = lifecycleTraceFile;
		process.env.AGENTOS_EXECUTE_PHASES = "1";
		process.env.AGENTOS_EXECUTE_PHASES_FILE = executePhasesFile;
		process.env.AGENTOS_JS_START_PHASES = "1";
		process.env.AGENTOS_JS_START_PHASES_FILE = jsStartPhasesFile;
		process.env.AGENTOS_JS_EVENT_PHASES = "1";
		process.env.AGENTOS_JS_EVENT_PHASES_FILE = jsEventPhasesFile;
		resetLifecycleTraceFile(lifecycleTraceFile);
		writeFileSync(executePhasesFile, "");
		writeFileSync(jsStartPhasesFile, "");
		writeFileSync(jsEventPhasesFile, "");
	}
	const sidecar = await createBenchSidecar();
	try {
		guest = stats(await runGuestLayer(sidecar));
		const nativeNodeExitPhases = runNativePhaseLayer(
			"node_exit",
			ITERATIONS,
			WARMUP,
		);
		const nativeFanoutPhases = runNativePhaseLayer(
			"node_fanout",
			ITERATIONS,
			WARMUP,
		);
		const nodeExitPhases = await runNodeExitPhases();
		const nodeFanoutPhases = await runNodeFanoutPhases();
		if (PROCESS_LIFECYCLE_TRACE) resetLifecycleTraceFile(lifecycleTraceFile);
		const guestNodeExitPhases = await runGuestNodeExitPhases(sidecar);
		const nodeExitLifecycleTraces = PROCESS_LIFECYCLE_TRACE
			? readLifecycleTraces(lifecycleTraceFile, WARMUP)
			: [];
		if (PROCESS_LIFECYCLE_TRACE) {
			resetLifecycleTraceFile(lifecycleTraceFile);
		}
		const guestFanoutPhases = await runGuestFanoutPhases(sidecar);
		const fanoutLifecycleTraces = PROCESS_LIFECYCLE_TRACE
			? readLifecycleTraces(lifecycleTraceFile, WARMUP * FANOUT)
			: [];
		const mainSidecarExecutePhases = PROCESS_LIFECYCLE_TRACE
			? readSidecarExecutePhases(executePhasesFile)
			: [];
		const mainSidecarJsStartPhases = PROCESS_LIFECYCLE_TRACE
			? readSidecarExecutePhases(jsStartPhasesFile)
			: [];
			const mainSidecarJsEventPhases = PROCESS_LIFECYCLE_TRACE
				? readSidecarExecutePhases(jsEventPhasesFile)
				: [];
			const skipExtraLifecycleRows =
				process.env.BENCH_PROCESS_LIFECYCLE_SKIP_EXTRA_ROWS === "1";
			const hostWriteSyncRows = PROCESS_LIFECYCLE_TRACE && !skipExtraLifecycleRows
				? await runGuestHostWriteSyncRows(sidecar, executePhasesFile)
				: [];
			if (PROCESS_LIFECYCLE_TRACE && !skipExtraLifecycleRows) {
				assertHostWriteSyncRows(hostWriteSyncRows);
			}
			const fanoutLadderRows = PROCESS_LIFECYCLE_TRACE && !skipExtraLifecycleRows
				? await runGuestFanoutLadderRows(sidecar, executePhasesFile)
				: [];
			const nestedChildProcessRows = PROCESS_LIFECYCLE_TRACE && !skipExtraLifecycleRows
				? await runNestedChildProcessRows(sidecar, executePhasesFile)
				: [];
		phaseRows = [
			makePhaseRow(
				"node_exit_1",
				"total",
				1,
				nativeNodeExitPhases,
				nodeExitPhases,
				guestNodeExitPhases,
			),
			makePhaseRow(
				"node_exit_1",
				"spawn",
				1,
				nativeNodeExitPhases,
				nodeExitPhases,
				guestNodeExitPhases,
			),
			makePhaseRow(
				"node_exit_1",
				"wait_reap",
				1,
				nativeNodeExitPhases,
				nodeExitPhases,
				guestNodeExitPhases,
			),
			makePhaseRow(
				"fanout_8",
				"total",
				FANOUT,
				nativeFanoutPhases,
				nodeFanoutPhases,
				guestFanoutPhases,
			),
			makePhaseRow(
				"fanout_8",
				"spawn_batch",
				FANOUT,
				nativeFanoutPhases,
				nodeFanoutPhases,
				guestFanoutPhases,
			),
			makePhaseRow(
				"fanout_8",
				"wait_reap_batch",
				FANOUT,
				nativeFanoutPhases,
				nodeFanoutPhases,
				guestFanoutPhases,
			),
		];
		if (PROCESS_LIFECYCLE_TRACE) {
			processLifecycle = {
				version: "BENCH-069",
				enabled: true,
				rows: [
					makeLifecycleRow("node_exit_1", 1, nodeExitLifecycleTraces),
					makeLifecycleRow("fanout_8", FANOUT, fanoutLifecycleTraces),
				],
				sidecarExecutePhases: mainSidecarExecutePhases,
				sidecarJsStartPhases: mainSidecarJsStartPhases,
				sidecarJsEventPhases: mainSidecarJsEventPhases,
				hostWriteSyncRows,
				fanoutLadderRows,
				nestedChildProcessRows,
			};
		}
	} finally {
		await sidecar.dispose();
		if (previousLifecycleTrace === undefined) {
			delete process.env.AGENTOS_PROCESS_LIFECYCLE_TRACE;
		} else {
			process.env.AGENTOS_PROCESS_LIFECYCLE_TRACE = previousLifecycleTrace;
		}
		if (previousLifecycleTraceFile === undefined) {
			delete process.env.AGENTOS_PROCESS_LIFECYCLE_TRACE_FILE;
		} else {
			process.env.AGENTOS_PROCESS_LIFECYCLE_TRACE_FILE =
				previousLifecycleTraceFile;
		}
		rmSync(lifecycleTraceFile, { force: true });
		if (previousExecutePhases === undefined) {
			delete process.env.AGENTOS_EXECUTE_PHASES;
		} else {
			process.env.AGENTOS_EXECUTE_PHASES = previousExecutePhases;
		}
		if (previousExecutePhasesFile === undefined) {
			delete process.env.AGENTOS_EXECUTE_PHASES_FILE;
		} else {
			process.env.AGENTOS_EXECUTE_PHASES_FILE = previousExecutePhasesFile;
		}
		rmSync(executePhasesFile, { force: true });
		if (previousJsStartPhases === undefined) {
			delete process.env.AGENTOS_JS_START_PHASES;
		} else {
			process.env.AGENTOS_JS_START_PHASES = previousJsStartPhases;
		}
		if (previousJsStartPhasesFile === undefined) {
			delete process.env.AGENTOS_JS_START_PHASES_FILE;
		} else {
			process.env.AGENTOS_JS_START_PHASES_FILE = previousJsStartPhasesFile;
		}
		rmSync(jsStartPhasesFile, { force: true });
		if (previousJsEventPhases === undefined) {
			delete process.env.AGENTOS_JS_EVENT_PHASES;
		} else {
			process.env.AGENTOS_JS_EVENT_PHASES = previousJsEventPhases;
		}
		if (previousJsEventPhasesFile === undefined) {
			delete process.env.AGENTOS_JS_EVENT_PHASES_FILE;
		} else {
			process.env.AGENTOS_JS_EVENT_PHASES_FILE = previousJsEventPhasesFile;
		}
		rmSync(jsEventPhasesFile, { force: true });
	}

	// Absolute floor context: cheapest libc process (no node), native only.
	const shFloor = stats(runNativeLayer("spawn_exit", ITERATIONS, WARMUP));

	const tax = {
		emulation_p50: round(guest.p50 / node.p50),
		emulation_p95: round(guest.p95 / node.p95),
		total_p50: round(guest.p50 / native.p50),
		total_p95: round(guest.p95 / native.p95),
		vs_libc_floor_p50: round(guest.p50 / shFloor.p50),
	};

	printTable(
		["layer", "p50 ms", "p95 ms", "p99 ms", "mean ms"],
		[
			["native node", native.p50, native.p95, native.p99, native.mean],
			["node child_proc", node.p50, node.p95, node.p99, node.mean],
			["guest isolate", guest.p50, guest.p95, guest.p99, guest.mean],
			["(libc sh floor)", shFloor.p50, shFloor.p95, shFloor.p99, shFloor.mean],
		],
	);
	console.error(
		`  node_exit: emulation tax ${tax.emulation_p50}x (guest/node), total ${tax.total_p50}x (guest/native node), ${tax.vs_libc_floor_p50}x vs libc floor — all p50`,
	);
	printTable(
		["row", "phase", "native p50", "node p50", "guest p50", "guest-node"],
		phaseRows.map((row) => [
			row.row,
			row.phase,
			row.layers.native.p50,
			row.layers.node.p50,
			row.layers.guest.p50,
			row.guest_minus_node_p50_ms,
		]),
	);
	if (processLifecycle) {
		printTable(
			[
				"row",
					"samples",
					"route",
					"waiters",
					"wait p50",
					"blockedStart p50",
					"execute p50",
				"signal p50",
				"drain p50",
			],
			processLifecycle.rows.map((row) => [
				row.row,
				row.samples,
					Object.entries(row.resolutionCounts)
						.map(([route, count]) => `${route}:${count}`)
						.join(","),
					Object.entries(row.waiterSourceCounts)
						.map(([source, count]) => `${source}:${count}`)
						.join(","),
					row.subphases.ts_wait_total?.p50 ?? "",
				row.subphases.ts_wait_blocked_on_start?.p50 ?? "",
				row.subphases.ts_execute_rpc?.p50 ?? "",
				row.subphases.ts_signal_refresh?.p50 ?? "",
				row.subphases.ts_trailing_output_drain?.p50 ?? "",
			]),
		);
		printTable(
			["sidecar execute stage", "calls", "avg us", "max us"],
			(processLifecycle.sidecarExecutePhases ?? []).map((metric) => [
				metric.stage,
				metric.calls,
				metric.avgUs,
				metric.maxUs,
			]),
		);
		printTable(
			["sidecar JS start stage", "calls", "avg us", "max us"],
			(processLifecycle.sidecarJsStartPhases ?? []).map((metric) => [
				metric.stage,
				metric.calls,
				metric.avgUs,
				metric.maxUs,
			]),
		);
		printTable(
			["sidecar JS event stage", "calls", "avg us", "max us"],
			(processLifecycle.sidecarJsEventPhases ?? []).map((metric) => [
				metric.stage,
				metric.calls,
				metric.avgUs,
				metric.maxUs,
			]),
		);
		printTable(
			[
				"host write sync row",
				"samples",
				"writes",
				"wall p50",
				"sync avg us",
				"clean skips",
				"cleanup avg us",
				"wait_reap avg us",
				"write visible",
			],
			(processLifecycle.hostWriteSyncRows ?? []).map((row) => [
				row.row,
				row.samples,
				row.writesPerProcess,
				row.wallMs.p50,
				phaseAvgUs(row, "process_exit_cleanup_sync_host_writes"),
				row.cleanSkipCalls,
				phaseAvgUs(row, "process_exit_cleanup"),
				phaseAvgUs(row, "process_exit_cleanup_wait_and_reap"),
				row.hostWriteVisible === undefined ? "" : String(row.hostWriteVisible),
			]),
		);
		printTable(
			[
				"fanout",
				"samples",
				"total p50",
				"spawn p50",
				"wait p50",
				"wait/process p50",
				"exit queued avg us",
				"cleanup avg us",
				"queue wait avg us",
			],
			(processLifecycle.fanoutLadderRows ?? []).map((row) => [
				row.fanout,
				row.samples,
				row.totalMs.p50,
				row.spawnBatchMs.p50,
				row.waitReapBatchMs.p50,
				row.perProcessWaitReapP50Ms,
				phaseAvgUs(row, "execute_response_to_exit_event_queued"),
				phaseAvgUs(row, "process_exit_cleanup"),
				phaseAvgUs(row, "process_exit_event_queue_wait"),
			]),
		);
		printTable(
			[
				"nested child_process row",
				"fanout",
				"samples",
				"node parent p50",
				"guest parent p50",
				"guest/node",
				"guest-node",
				"expected processes",
				"exit callbacks",
				"close callbacks",
				"child spawns",
				"child cleanups",
				"child spawn avg us",
				"child cleanup avg us",
			],
			(processLifecycle.nestedChildProcessRows ?? []).map((row) => [
				row.row,
				row.fanout,
				row.samples,
				row.nodeParentMs.p50,
				row.guestParentMs.p50,
				row.guestVsNodeP50,
				row.guestMinusNodeP50Ms,
				row.expectedSidecarProcessExecutions,
				row.parentObserved.exitCallbacks,
				row.parentObserved.closeCallbacks,
				phaseCalls(row, "child_process_spawn_total"),
				phaseCalls(row, "child_process_exit_cleanup"),
				phaseAvgUs(row, "child_process_spawn_total"),
				phaseAvgUs(row, "child_process_exit_cleanup"),
			]),
		);
	}

	console.log(
		JSON.stringify(
			{
				hardware,
				iterations: ITERATIONS,
				op: "node_exit",
				layers: { native, node, guest, shFloor },
				tax,
				phaseRows,
				processLifecycle,
			},
			null,
			2,
			),
		);
		if (processLifecycle) printTable(
			[
				"row",
				"event->finish p50",
				"finish p50",
				"finish->wait p50",
				"wait->return p50",
				"wait gap p50",
			],
			processLifecycle.rows.map((row) => [
				row.row,
				row.subphases.ts_exit_event_to_finish?.p50 ?? "",
				row.subphases.ts_finish_total?.p50 ?? "",
				row.subphases.ts_finish_to_wait_observed?.p50 ?? "",
				row.subphases.ts_wait_observed_to_return?.p50 ?? "",
				row.subphases.ts_wait_unattributed_gap?.p50 ?? "",
			]),
		);
	}

main().catch((err) => {
	console.error(err);
	process.exit(1);
});
