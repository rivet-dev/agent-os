import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import type { NodeRuntimeResourceSnapshot } from "@rivet-dev/agentos-runtime-core";
import { round, stats, type Stats } from "../lib/perf-utils.js";
import {
	formatPacificIso,
	prewarmBenchVm,
	resolveBenchSidecarProvenance,
	type BenchVm,
	type SidecarBinaryProvenance,
} from "../lib/vm.js";

export const DEFAULT_CONCURRENCY_COUNTS = [1, 4, 8] as const;
export const DEFAULT_DURATION_MS = 5_000;
export const DEFAULT_TCP_WARMUP = 5;
export const DEFAULT_FS_WARMUP = 25;
export const FS_WRITE_SMALL_BYTES = 4 * 1024;

export interface GuestLoopResult {
	op: "tcp_echo_small" | "fs_write_small" | "busy_interference";
	durationMs: number;
	ops: number;
	verifiedOps: number;
	samplesMs?: number[];
	latencyMs?: Stats;
	rawSampleCount?: number;
	payloadBytes?: number;
	writeBytes?: number;
}

export interface ParticipantResult {
	index: number;
	ops: number;
	durationMs: number;
	opsPerSec: number;
	latencyMs: Stats;
	resourceSnapshot?: NodeRuntimeResourceSnapshot;
	sidecarVmHwmBytes?: number;
	rawSampleCount: number;
}

export interface ConcurrencyRow {
	n: number;
	durationMs: number;
	aggregateOps: number;
	aggregateOpsPerSec: number;
	meanParticipantOpsPerSec: number;
	minParticipantOpsPerSec: number;
	maxParticipantOpsPerSec: number;
	meanP50Ms: number;
	meanP95Ms: number;
	maxP95Ms: number;
	scaling: {
		idealScaling: 1;
		throughputVsN1: number;
		measuredOfIdeal: number;
	};
	participants: ParticipantResult[];
}

export interface RegressionRow {
	rowKey: string;
	metric: string;
	value: number;
	unit: string;
}

const REPO_ROOT = fileURLToPath(new URL("../../../..", import.meta.url));
const MATRIX_PATH = join(REPO_ROOT, "packages/benchmarks/results/latency-matrix.json");

const PREWARM_OP = {
	family: "focused",
	name: "concurrency-prewarm",
	fileLine: "packages/benchmarks/src/focused/concurrency-common.ts",
	reproducer: "guest node prewarm for focused concurrency lanes",
	program: "async () => {}",
};

export function generatedAtPacific(): string {
	return formatPacificIso(new Date());
}

export function benchmarkProvenance(): {
	generatedAt: string;
	sidecar: SidecarBinaryProvenance;
} {
	return {
		generatedAt: generatedAtPacific(),
		sidecar: resolveBenchSidecarProvenance(),
	};
}

export async function prewarmConcurrencyVm(vm: BenchVm): Promise<void> {
	await prewarmBenchVm(vm, PREWARM_OP);
}

export function parseCounts(value: string | undefined): number[] {
	if (!value) return [...DEFAULT_CONCURRENCY_COUNTS];
	const counts = value
		.split(",")
		.map((part) => Number(part.trim()))
		.filter((n) => Number.isInteger(n) && n > 0);
	if (counts.length === 0) {
		throw new Error(`invalid concurrency counts: ${JSON.stringify(value)}`);
	}
	if (Math.max(...counts) > 8) {
		throw new Error(`concurrency count exceeds bounded max 8: ${Math.max(...counts)}`);
	}
	return counts;
}

export function envNumber(name: string, fallback: number): number {
	const raw = process.env[name];
	if (raw === undefined || raw === "") return fallback;
	const value = Number(raw);
	if (!Number.isFinite(value) || value <= 0) {
		throw new Error(`${name} must be a positive number, got ${JSON.stringify(raw)}`);
	}
	return value;
}

export async function writeGuestProgram(
	vm: BenchVm,
	path: string,
	source: string,
): Promise<void> {
	await vm.writeFile(path, source);
}

export async function runGuestJsonProgram(
	vm: BenchVm,
	path: string,
	env: Record<string, string>,
): Promise<GuestLoopResult> {
	const result = await vm.spawnNodeCapture(path, env);
	if (result.exitCode !== 0) {
		throw new Error(
			`guest program ${path} exited ${result.exitCode}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
		);
	}
	const parsed = JSON.parse(result.stdout) as GuestLoopResult;
	if (parsed.ops !== parsed.verifiedOps) {
		throw new Error(
			`${path} verification mismatch: ops=${parsed.ops} verified=${parsed.verifiedOps}`,
		);
	}
	const sampleCount = parsed.rawSampleCount ?? parsed.samplesMs?.length ?? 0;
	if (sampleCount <= 0) {
		throw new Error(`${path} produced no latency samples`);
	}
	if (parsed.ops <= 0) {
		throw new Error(`${path} produced no operations`);
	}
	return parsed;
}

export async function participantFromLoop(
	index: number,
	vm: BenchVm,
	loop: GuestLoopResult,
): Promise<ParticipantResult> {
	const pid = vm.sidecarPid();
	const sampleCount = loop.rawSampleCount ?? loop.samplesMs?.length ?? 0;
	return {
		index,
		ops: loop.ops,
		durationMs: round(loop.durationMs),
		opsPerSec: round((loop.ops / loop.durationMs) * 1000, 2),
		latencyMs: loop.latencyMs ?? stats(loop.samplesMs ?? []),
		resourceSnapshot: await vm.getResourceSnapshot(),
		sidecarVmHwmBytes: pid === null ? undefined : readVmHwmBytes(pid),
		rawSampleCount: sampleCount,
	};
}

export function buildConcurrencyRow(
	n: number,
	durationMs: number,
	participants: ParticipantResult[],
	baselineOpsPerSec: number,
): ConcurrencyRow {
	const aggregateOps = participants.reduce((sum, row) => sum + row.ops, 0);
	const aggregateOpsPerSec = round(
		participants.reduce((sum, row) => sum + row.opsPerSec, 0),
		2,
	);
	const participantOps = participants.map((row) => row.opsPerSec);
	const p50s = participants.map((row) => row.latencyMs.p50);
	const p95s = participants.map((row) => row.latencyMs.p95);
	const throughputVsN1 = round(aggregateOpsPerSec / baselineOpsPerSec, 2);
	return {
		n,
		durationMs,
		aggregateOps,
		aggregateOpsPerSec,
		meanParticipantOpsPerSec: round(mean(participantOps), 2),
		minParticipantOpsPerSec: round(Math.min(...participantOps), 2),
		maxParticipantOpsPerSec: round(Math.max(...participantOps), 2),
		meanP50Ms: round(mean(p50s), 2),
		meanP95Ms: round(mean(p95s), 2),
		maxP95Ms: round(Math.max(...p95s), 2),
		scaling: {
			idealScaling: 1,
			throughputVsN1,
			measuredOfIdeal: round(aggregateOpsPerSec / (baselineOpsPerSec * n), 2),
		},
		participants,
	};
}

export function concurrencyRegressionRows(
	benchmark: "concurrency-vms" | "concurrent-processes",
	rows: ConcurrencyRow[],
): RegressionRow[] {
	return rows.flatMap((row) => [
		{
			rowKey: `${benchmark}.n${row.n}.aggregate_ops_per_sec`,
			metric: "aggregateOpsPerSec",
			value: row.aggregateOpsPerSec,
			unit: "ops/s",
		},
		{
			rowKey: `${benchmark}.n${row.n}.mean_p95_ms`,
			metric: "meanP95Ms",
			value: row.meanP95Ms,
			unit: "ms",
		},
		{
			rowKey: `${benchmark}.n${row.n}.scaling_of_ideal`,
			metric: "measuredOfIdeal",
			value: row.scaling.measuredOfIdeal,
			unit: "ratio",
		},
	]);
}

export function assertMatrixBallpark(
	op: "fs_write_small" | "tcp_echo_small",
	observedP50Ms: number,
	options: { multiplier: number; fallbackCeilingMs: number },
): void {
	const matrixP50 = readMatrixGuestP50(op);
	const threshold = matrixP50 === undefined
		? options.fallbackCeilingMs
		: matrixP50 * options.multiplier;
	if (observedP50Ms > threshold) {
		throw new Error(
			`${op} N=1 p50 ${observedP50Ms}ms exceeds sanity ceiling ${round(
				threshold,
				2,
			)}ms${matrixP50 === undefined ? " (fallback)" : ` (matrix ${matrixP50}ms x ${options.multiplier})`}`,
		);
	}
	console.error(
		`  sanity ${op}: N=1 p50=${observedP50Ms}ms <= ${round(threshold, 2)}ms${
			matrixP50 === undefined ? " fallback ceiling" : ` (${options.multiplier}x matrix ${matrixP50}ms)`
		}`,
	);
}

export function tcpEchoSmallLoopProgram(): string {
	return `
import net from "node:net";

const durationMs = Number(process.env.BENCH_DURATION_MS || ${DEFAULT_DURATION_MS});
const warmup = Number(process.env.BENCH_WARMUP || ${DEFAULT_TCP_WARMUP});
const payload = Buffer.from("agentos-tcp-echo");
const samplesMs = [];
const now = () => Number(process.hrtime.bigint()) / 1e6;

async function once() {
  const body = await new Promise((resolve, reject) => {
    const server = net.createServer((socket) => {
      const serverChunks = [];
      let serverReceived = 0;
      socket.on("data", (chunk) => {
        serverChunks.push(Buffer.from(chunk));
        serverReceived += chunk.length;
        if (serverReceived >= payload.length) {
          socket.end(Buffer.concat(serverChunks));
        }
      });
      socket.on("error", reject);
    });
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        reject(new Error("tcp echo server did not bind to a TCP port"));
        return;
      }
      const socket = net.connect(address.port, "127.0.0.1");
      const chunks = [];
      let received = 0;
      socket.on("connect", () => socket.write(payload));
      socket.on("data", (chunk) => {
        chunks.push(Buffer.from(chunk));
        received += chunk.length;
      });
      socket.on("error", reject);
      socket.on("close", () => {
        server.close((error) => {
          if (error) reject(error);
          else resolve(Buffer.concat(chunks));
        });
      });
    });
  });
  if (!Buffer.from(body).equals(payload)) {
    throw new Error("bad tcp echo payload: " + Buffer.from(body).toString("hex"));
  }
}

for (let i = 0; i < warmup; i++) await once();
const start = now();
const deadline = start + durationMs;
while (now() < deadline) {
  const opStart = now();
  await once();
  samplesMs.push(now() - opStart);
}
const elapsed = now() - start;
process.stdout.write(JSON.stringify({
  op: "tcp_echo_small",
  durationMs: elapsed,
  ops: samplesMs.length,
  verifiedOps: samplesMs.length,
  payloadBytes: payload.length,
  samplesMs,
}));
`;
}

export function fsWriteSmallLoopProgram(): string {
	return `
import fs from "node:fs";

const durationMs = Number(process.env.BENCH_DURATION_MS || ${DEFAULT_DURATION_MS});
const warmup = Number(process.env.BENCH_WARMUP || ${DEFAULT_FS_WARMUP});
const processIndex = process.env.BENCH_PROCESS_INDEX || "0";
const payload = Buffer.alloc(${FS_WRITE_SMALL_BYTES}, 7);
const path = "/tmp/focused-fs-write-small-" + process.pid + "-" + processIndex + ".bin";
const samplesMs = [];
const now = () => Number(process.hrtime.bigint()) / 1e6;
const round = (n) => Math.round(n * 100) / 100;
function summarize(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const percentile = (p) => sorted[Math.max(0, Math.ceil((p / 100) * sorted.length) - 1)];
  return {
    mean: round(values.reduce((sum, value) => sum + value, 0) / values.length),
    p50: round(percentile(50)),
    p95: round(percentile(95)),
    p99: round(percentile(99)),
    min: round(sorted[0]),
    max: round(sorted[sorted.length - 1]),
  };
}

function once(iteration) {
  payload[0] = iteration & 255;
  fs.writeFileSync(path, payload);
}

for (let i = 0; i < warmup; i++) once(i);
const start = now();
const deadline = start + durationMs;
while (now() < deadline) {
  const opStart = now();
  once(samplesMs.length);
  samplesMs.push(now() - opStart);
}
const stat = fs.statSync(path);
fs.unlinkSync(path);
if (stat.size !== payload.length) {
  throw new Error("bad fs write size: " + stat.size);
}
process.stdout.write(JSON.stringify({
  op: "fs_write_small",
  durationMs: now() - start,
  ops: samplesMs.length,
  verifiedOps: samplesMs.length,
  writeBytes: payload.length,
  latencyMs: summarize(samplesMs),
  rawSampleCount: samplesMs.length,
}));
`;
}

export function busyInterferenceProgram(): string {
	return `
import fs from "node:fs";

const durationMs = Number(process.env.BENCH_DURATION_MS || ${DEFAULT_DURATION_MS});
const payload = Buffer.alloc(${FS_WRITE_SMALL_BYTES}, 3);
const path = "/tmp/focused-busy-interference-" + process.pid + ".bin";
const samplesMs = [];
const now = () => Number(process.hrtime.bigint()) / 1e6;
const round = (n) => Math.round(n * 100) / 100;
function summarize(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const percentile = (p) => sorted[Math.max(0, Math.ceil((p / 100) * sorted.length) - 1)];
  return {
    mean: round(values.reduce((sum, value) => sum + value, 0) / values.length),
    p50: round(percentile(50)),
    p95: round(percentile(95)),
    p99: round(percentile(99)),
    min: round(sorted[0]),
    max: round(sorted[sorted.length - 1]),
  };
}

function spin(ms) {
  const end = now() + ms;
  let x = 0;
  while (now() < end) x = (x + 1) % 1000003;
  return x;
}

const start = now();
const deadline = start + durationMs;
let writes = 0;
while (now() < deadline) {
  spin(5);
  const opStart = now();
  for (let i = 0; i < 8; i++) {
    payload[0] = (writes + i) & 255;
    fs.writeFileSync(path, payload);
  }
  writes += 8;
  samplesMs.push(now() - opStart);
}
const stat = fs.statSync(path);
fs.unlinkSync(path);
if (stat.size !== payload.length) {
  throw new Error("bad busy write size: " + stat.size);
}
process.stdout.write(JSON.stringify({
  op: "busy_interference",
  durationMs: now() - start,
  ops: writes,
  verifiedOps: writes,
  writeBytes: payload.length,
  latencyMs: summarize(samplesMs),
  rawSampleCount: samplesMs.length,
}));
`;
}

function readMatrixGuestP50(op: string): number | undefined {
	if (!existsSync(MATRIX_PATH)) return undefined;
	try {
		const parsed = JSON.parse(readFileSync(MATRIX_PATH, "utf8")) as {
			latency?: Array<{ op?: string; layers?: { guest?: { p50?: number } } }>;
		};
		const row = parsed.latency?.find((entry) => entry.op === op);
		const p50 = row?.layers?.guest?.p50;
		return typeof p50 === "number" && Number.isFinite(p50) ? p50 : undefined;
	} catch {
		return undefined;
	}
}

function readVmHwmBytes(pid: number): number | undefined {
	try {
		const status = readFileSync(`/proc/${pid}/status`, "utf8");
		const match = status.match(/^VmHWM:\s+(\d+)\s+kB/m);
		return match ? Number(match[1]) * 1024 : undefined;
	} catch {
		return undefined;
	}
}

function mean(values: number[]): number {
	return values.reduce((sum, value) => sum + value, 0) / values.length;
}
