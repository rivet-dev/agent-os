/**
 * Focused readdir scaling benchmark.
 *
 * Matches fs/readdir_small by preparing fixtures outside the timed loop so the
 * measurement isolates fs.readdirSync cost.
 */

import {
	existsSync,
	mkdirSync,
	readdirSync,
	rmSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createBenchSidecar, createBenchVm, type BenchVm } from "../lib/vm.js";
import type { HostDirectoryMount, SidecarProcess } from "@rivet-dev/agentos-core";
import { getHardware, printTable, round, stats } from "../lib/perf-utils.js";

type ReaddirMode = "plain" | "withFileTypes";
type ReaddirFixture = "vm-shadow" | "native-host-dir";
type ReaddirWorkload = "pure" | "matrix-guarded" | "probe";
type ReaddirPreflightOp = "none" | "existsSync" | "statSync";
type ReaddirProbeTargets = "children" | "dir-plus-children";

interface OperationCounts {
	existsSync: number;
	statSync: number;
	readdirSync: number;
	mkdirSync: number;
	writeFileSync: number;
}

interface ReaddirShape {
	workload: ReaddirWorkload;
	preflightOp: ReaddirPreflightOp;
	preflightCount: number;
	includeReaddir: boolean;
	probeTargets: ReaddirProbeTargets;
	operationCounts: OperationCounts;
}

interface ReaddirCaseResult {
	fixture: ReaddirFixture;
	workload: ReaddirWorkload;
	preflightOp: ReaddirPreflightOp;
	preflightCount: number;
	includeReaddir: boolean;
	probeTargets: ReaddirProbeTargets;
	operationCounts: OperationCounts;
	entryCount: number;
	mode: ReaddirMode;
	iterations: number;
	warmup: number;
	host: ReturnType<typeof stats>;
	guest: ReturnType<typeof stats>;
	guestVsHostRatio: number;
	returnedCount: {
		host: number;
		guest: number;
	};
	payloadBytes: {
		host: number;
		guest: number;
	};
	msPerEntry: {
		host: number;
		guest: number;
	};
	deltaFromEmptyMs?: {
		host: number;
		guest: number;
	};
	derived?: {
		guestP50MinusPureReaddirMs?: number;
		guestP50MinusPreflightOnlyMs?: number;
		guestMsPerPreflightCall?: number;
	};
	raw: {
		hostMs: number[];
		guestMs: number[];
	};
}

function parseArgs(): {
	iterations: number;
	warmup: number;
	entryCounts: number[];
	modes: ReaddirMode[];
	fixtures: ReaddirFixture[];
	workloads: ReaddirWorkload[];
	probeEnabled: boolean;
	preflightOps: ReaddirPreflightOp[];
	preflightCounts: number[];
	includeReaddirs: boolean[];
	probeTargets: ReaddirProbeTargets;
} {
	const value = (name: string) =>
		process.argv.find((arg) => arg.startsWith(`--${name}=`))?.split("=")[1];
	const iterations = Number(value("iterations") ?? 10);
	const warmup = Number(value("warmup") ?? 2);
	const entryCounts = (value("entry-counts") ?? "0,1,32,100,1000")
		.split(",")
		.map((n) => Number(n.trim()))
		.filter((n) => Number.isFinite(n) && n >= 0);
	const modes = (value("modes") ?? "plain,withFileTypes")
		.split(",")
		.map((mode) => mode.trim())
		.filter((mode): mode is ReaddirMode => mode === "plain" || mode === "withFileTypes");
	const fixtures = (value("fixtures") ?? "vm-shadow")
		.split(",")
		.map((fixture) => fixture.trim())
		.filter(
			(fixture): fixture is ReaddirFixture =>
				fixture === "vm-shadow" || fixture === "native-host-dir",
		);
	const workloads = (value("workloads") ?? "pure")
		.split(",")
		.map((workload) => workload.trim())
		.filter(
			(workload): workload is ReaddirWorkload =>
				workload === "pure" || workload === "matrix-guarded" || workload === "probe",
		);
	const preflightOpsArg = value("preflight-ops");
	const preflightCountsArg = value("preflight-counts");
	const includeReaddirArg = value("include-readdir");
	const probeEnabled =
		workloads.includes("probe") ||
		preflightOpsArg !== undefined ||
		preflightCountsArg !== undefined ||
		includeReaddirArg !== undefined;
	const preflightOps = (preflightOpsArg ?? "none,existsSync,statSync")
		.split(",")
		.map((op) => op.trim())
		.filter(
			(op): op is ReaddirPreflightOp =>
				op === "none" || op === "existsSync" || op === "statSync",
		);
	const preflightCounts = (preflightCountsArg ?? "0,1,32,33")
		.split(",")
		.map((n) => Number(n.trim()))
		.filter((n) => Number.isFinite(n) && n >= 0);
	const includeReaddirs =
		includeReaddirArg === "both"
			? [false, true]
			: includeReaddirArg === "false" || includeReaddirArg === "0"
				? [false]
				: [true];
	const probeTargets = value("probe-targets") ?? "children";
	if (
		iterations < 1 ||
		warmup < 0 ||
		entryCounts.length === 0 ||
		modes.length === 0 ||
		fixtures.length === 0 ||
		workloads.length === 0 ||
		preflightOps.length === 0 ||
		preflightCounts.length === 0 ||
		(probeTargets !== "children" && probeTargets !== "dir-plus-children")
	) {
		throw new Error(
			"invalid args; expected --iterations>=1 --warmup>=0 --entry-counts=0,1,32,100,1000 --modes=plain,withFileTypes --fixtures=vm-shadow,native-host-dir --workloads=pure,matrix-guarded --preflight-ops=none,existsSync,statSync --preflight-counts=0,1,32,33 --include-readdir=true,false,both --probe-targets=children,dir-plus-children",
		);
	}
	return {
		iterations,
		warmup,
		entryCounts,
		modes,
		fixtures,
		workloads,
		probeEnabled,
		preflightOps,
		preflightCounts,
		includeReaddirs,
		probeTargets,
	};
}

function nowMs(start: number): number {
	return performance.now() - start;
}

function createHostFixture(entryCount: number): string {
	const dir = mkdtempSync(join(tmpdir(), `agentos-readdir-bench-${entryCount}-`));
	for (let i = 0; i < entryCount; i++) {
		writeFileSync(join(dir, `file-${String(i).padStart(5, "0")}.txt`), "x");
	}
	return dir;
}

function readDirPayload(dir: string, mode: ReaddirMode): unknown[] {
	if (mode === "withFileTypes") {
		return readdirSync(dir, { withFileTypes: true }).map((entry) => ({
			name: entry.name,
			file: entry.isFile(),
			dir: entry.isDirectory(),
			symlink: entry.isSymbolicLink(),
		}));
	}
	return readdirSync(dir);
}

function ensureMatrixGuardedFixture(dir: string, entryCount: number): void {
	if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
	for (let i = 0; i < entryCount; i++) {
		const path = join(dir, `file-${String(i).padStart(5, "0")}.txt`);
		if (!existsSync(path)) writeFileSync(path, "hi");
	}
}

function createOperationCounts(
	shape: Pick<ReaddirShape, "preflightOp" | "preflightCount" | "includeReaddir">,
): OperationCounts {
	return {
		existsSync: shape.preflightOp === "existsSync" ? shape.preflightCount : 0,
		statSync: shape.preflightOp === "statSync" ? shape.preflightCount : 0,
		readdirSync: shape.includeReaddir ? 1 : 0,
		mkdirSync: 0,
		writeFileSync: 0,
	};
}

function shapeForWorkload(workload: ReaddirWorkload, entryCount: number): ReaddirShape {
	if (workload === "matrix-guarded") {
		const countsShape = {
			workload,
			preflightOp: "existsSync" as const,
			preflightCount: entryCount + 1,
			includeReaddir: true,
			probeTargets: "dir-plus-children" as const,
		};
		return {
			...countsShape,
			operationCounts: createOperationCounts(countsShape),
		};
	}
	const shape = {
		workload,
		preflightOp: "none" as const,
		preflightCount: 0,
		includeReaddir: true,
		probeTargets: "children" as const,
	};
	return { ...shape, operationCounts: createOperationCounts(shape) };
}

function preflightPath(
	dir: string,
	entryCount: number,
	index: number,
	probeTargets: ReaddirProbeTargets,
): string {
	if (probeTargets === "dir-plus-children" && index === 0) return dir;
	if (entryCount === 0) return dir;
	const childIndex = probeTargets === "dir-plus-children" ? index - 1 : index;
	return join(dir, `file-${String(childIndex % entryCount).padStart(5, "0")}.txt`);
}

function runPreflight(
	dir: string,
	entryCount: number,
	preflightOp: ReaddirPreflightOp,
	preflightCount: number,
	probeTargets: ReaddirProbeTargets,
): void {
	for (let i = 0; i < preflightCount; i++) {
		const path = preflightPath(dir, entryCount, i, probeTargets);
		if (preflightOp === "existsSync") {
			existsSync(path);
		} else if (preflightOp === "statSync") {
			statSync(path);
		}
	}
}

function payloadBytes(payload: unknown): number {
	return Buffer.byteLength(JSON.stringify(payload));
}

function runHost(
	dir: string,
	entryCount: number,
	mode: ReaddirMode,
	shape: ReaddirShape,
	iterations: number,
	warmup: number,
) {
	const samples: number[] = [];
	let returnedCount = 0;
	let bytes = 0;
	for (let i = 0; i < warmup + iterations; i++) {
		const start = performance.now();
		if (shape.workload === "matrix-guarded") {
			ensureMatrixGuardedFixture(dir, entryCount);
		} else {
			runPreflight(
				dir,
				entryCount,
				shape.preflightOp,
				shape.preflightCount,
				shape.probeTargets,
			);
		}
		const payload = shape.includeReaddir ? readDirPayload(dir, mode) : [];
		const ms = nowMs(start);
		if (i >= warmup) samples.push(ms);
		returnedCount = payload.length;
		bytes = payloadBytes(payload);
	}
	return { samples, returnedCount, payloadBytes: bytes };
}

async function setupGuestFixture(vm: BenchVm, dir: string, entryCount: number): Promise<void> {
	await vm.exec(`rm -rf ${dir} && mkdir -p ${dir}`);
	for (let i = 0; i < entryCount; i++) {
		await vm.writeFile(`${dir}/file-${String(i).padStart(5, "0")}.txt`, "x");
	}
}

async function runGuest(
	vm: BenchVm,
	dir: string,
	entryCount: number,
	mode: ReaddirMode,
	shape: ReaddirShape,
	iterations: number,
	warmup: number,
) {
	const scriptPath = `/tmp/readdir-bench-runner-${mode}-${Date.now()}-${Math.random()
		.toString(16)
		.slice(2)}.mjs`;
	const source = `
import fs from "node:fs";
const dir = ${JSON.stringify(dir)};
const entryCount = ${JSON.stringify(entryCount)};
const mode = ${JSON.stringify(mode)};
const shape = ${JSON.stringify(shape)};
const iterations = Number(process.env.BENCH_ITERATIONS || 10);
const warmup = Number(process.env.BENCH_WARMUP || 2);
const samples = [];
let returnedCount = 0;
let payloadBytes = 0;
const now = () => Number(process.hrtime.bigint()) / 1e6;
function readPayload() {
  if (mode === "withFileTypes") {
    return fs.readdirSync(dir, { withFileTypes: true }).map((entry) => ({
      name: entry.name,
      file: entry.isFile(),
      dir: entry.isDirectory(),
      symlink: entry.isSymbolicLink(),
    }));
  }
  return fs.readdirSync(dir);
}
function ensureMatrixGuardedFixture() {
  if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
  for (let i = 0; i < entryCount; i++) {
    const path = dir + "/file-" + String(i).padStart(5, "0") + ".txt";
    if (!fs.existsSync(path)) fs.writeFileSync(path, "hi");
  }
}
function preflightPath(index) {
  if (shape.probeTargets === "dir-plus-children" && index === 0) return dir;
  if (entryCount === 0) return dir;
  const childIndex = shape.probeTargets === "dir-plus-children" ? index - 1 : index;
  return dir + "/file-" + String(childIndex % entryCount).padStart(5, "0") + ".txt";
}
function runPreflight() {
  for (let i = 0; i < shape.preflightCount; i++) {
    const path = preflightPath(i);
    if (shape.preflightOp === "existsSync") fs.existsSync(path);
    else if (shape.preflightOp === "statSync") fs.statSync(path);
  }
}
for (let i = 0; i < warmup + iterations; i++) {
  const start = now();
  if (shape.workload === "matrix-guarded") ensureMatrixGuardedFixture();
  else runPreflight();
  const payload = shape.includeReaddir ? readPayload() : [];
  const ms = now() - start;
  if (i >= warmup) samples.push(ms);
  returnedCount = payload.length;
  payloadBytes = Buffer.byteLength(JSON.stringify(payload));
}
process.stdout.write(JSON.stringify({ samples, returnedCount, payloadBytes }));
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
		throw new Error(`guest readdir ${mode} exited ${code}\n${stderr}`);
	}
	return JSON.parse(stdout) as {
		samples: number[];
		returnedCount: number;
		payloadBytes: number;
	};
}

async function createVm(sidecar: SidecarProcess): Promise<BenchVm> {
	return createBenchVm({
		sidecar,
	});
}

async function createVmWithMount(
	sidecar: SidecarProcess,
	path: string,
	hostPath: string,
): Promise<BenchVm> {
	const mounts: HostDirectoryMount[] = [{ guestPath: path, hostPath, readOnly: true }];
	return createBenchVm({
		sidecar,
		mounts,
	});
}

async function runCase(
	fixture: ReaddirFixture,
	shape: ReaddirShape,
	entryCount: number,
	mode: ReaddirMode,
	iterations: number,
	warmup: number,
	sidecar: SidecarProcess,
): Promise<ReaddirCaseResult> {
	const hostDir = createHostFixture(entryCount);
	const vmDir =
		fixture === "native-host-dir"
			? `/mnt/readdir-bench-${entryCount}-${mode}`
			: `/tmp/readdir-bench-${entryCount}-${mode}`;
	try {
		const host = runHost(hostDir, entryCount, mode, shape, iterations, warmup);
		const vm =
			fixture === "native-host-dir"
				? await createVmWithMount(sidecar, vmDir, hostDir)
				: await createVm(sidecar);
		try {
			if (fixture === "vm-shadow") {
				await setupGuestFixture(vm, vmDir, entryCount);
			}
			const guest = await runGuest(
				vm,
				vmDir,
				entryCount,
				mode,
				shape,
				iterations,
				warmup,
			);
			const hostStats = stats(host.samples);
			const guestStats = stats(guest.samples);
			const divisor = Math.max(entryCount, 1);
			return {
				fixture,
				workload: shape.workload,
				preflightOp: shape.preflightOp,
				preflightCount: shape.preflightCount,
				includeReaddir: shape.includeReaddir,
				probeTargets: shape.probeTargets,
				operationCounts: shape.operationCounts,
				entryCount,
				mode,
				iterations,
				warmup,
				host: hostStats,
				guest: guestStats,
				guestVsHostRatio: round(guestStats.p50 / hostStats.p50),
				returnedCount: {
					host: host.returnedCount,
					guest: guest.returnedCount,
				},
				payloadBytes: {
					host: host.payloadBytes,
					guest: guest.payloadBytes,
				},
				msPerEntry: {
					host: round(hostStats.p50 / divisor),
					guest: round(guestStats.p50 / divisor),
				},
				raw: {
					hostMs: host.samples,
					guestMs: guest.samples,
				},
			};
		} finally {
			await vm.dispose();
		}
	} finally {
		rmSync(hostDir, { recursive: true, force: true });
	}
}

async function main(): Promise<void> {
	const {
		iterations,
		warmup,
		entryCounts,
		modes,
		fixtures,
		workloads,
		probeEnabled,
		preflightOps,
		preflightCounts,
		includeReaddirs,
		probeTargets,
	} = parseArgs();
	const hardware = getHardware();
	console.error("=== Readdir Scaling Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(
		`Iterations: ${iterations} (+ ${warmup} warmup), entries: ${entryCounts.join(",")}, modes: ${modes.join(",")}, fixtures: ${fixtures.join(",")}, workloads: ${probeEnabled ? "probe" : workloads.join(",")}, preflightOps: ${probeEnabled ? preflightOps.join(",") : "n/a"}, preflightCounts: ${probeEnabled ? preflightCounts.join(",") : "n/a"}, includeReaddir: ${probeEnabled ? includeReaddirs.join(",") : "n/a"}`,
	);

	const sidecar = await createBenchSidecar();
	try {
		const cases: ReaddirCaseResult[] = [];
		for (const fixture of fixtures) {
			const shapesByEntryCount = (entryCount: number): ReaddirShape[] => {
				if (!probeEnabled) return workloads.map((workload) => shapeForWorkload(workload, entryCount));
				const shapes: ReaddirShape[] = [];
				for (const preflightOp of preflightOps) {
					for (const preflightCount of preflightCounts) {
						for (const includeReaddir of includeReaddirs) {
							const shape = {
								workload: "probe" as const,
								preflightOp,
								preflightCount,
								includeReaddir,
								probeTargets,
							};
							shapes.push({ ...shape, operationCounts: createOperationCounts(shape) });
						}
					}
				}
				return shapes;
			};
			for (const mode of modes) {
				for (const entryCount of entryCounts) {
					for (const shape of shapesByEntryCount(entryCount)) {
						if (shape.preflightOp === "none" && shape.preflightCount !== 0) continue;
						if (shape.preflightOp !== "none" && shape.preflightCount === 0) continue;
						if (!shape.includeReaddir && shape.preflightOp === "none") continue;
						const result = await runCase(
							fixture,
							shape,
							entryCount,
							mode,
							iterations,
							warmup,
							sidecar,
						);
						cases.push(result);
						console.error(
							`  fixture=${fixture} workload=${shape.workload} preflight=${shape.preflightOp}:${shape.preflightCount} readdir=${shape.includeReaddir} mode=${mode} entries=${entryCount}: host.p50=${result.host.p50}ms guest.p50=${result.guest.p50}ms ratio=${result.guestVsHostRatio}x guest.ms/entry=${result.msPerEntry.guest}`,
						);
					}
				}
			}
		}
		for (const fixture of fixtures) {
			for (const workload of probeEnabled ? (["probe"] as ReaddirWorkload[]) : workloads) {
				for (const mode of modes) {
					const empty = cases.find(
						(result) =>
							result.fixture === fixture &&
							result.workload === workload &&
							result.mode === mode &&
							result.preflightOp === "none" &&
							result.preflightCount === 0 &&
							result.includeReaddir &&
							result.entryCount === 0,
					);
					if (!empty) continue;
					for (const result of cases) {
						if (
							result.fixture !== fixture ||
							result.workload !== workload ||
							result.mode !== mode ||
							result.preflightOp !== empty.preflightOp ||
							result.preflightCount !== empty.preflightCount ||
							result.includeReaddir !== empty.includeReaddir
						) {
							continue;
						}
						result.deltaFromEmptyMs = {
							host: round(result.host.p50 - empty.host.p50),
							guest: round(result.guest.p50 - empty.guest.p50),
						};
					}
				}
			}
		}
		for (const result of cases) {
			const pureReaddir = cases.find(
				(candidate) =>
					candidate.fixture === result.fixture &&
					candidate.mode === result.mode &&
					candidate.entryCount === result.entryCount &&
					candidate.preflightOp === "none" &&
					candidate.preflightCount === 0 &&
					candidate.includeReaddir,
			);
			const preflightOnly = cases.find(
				(candidate) =>
					candidate.fixture === result.fixture &&
					candidate.mode === result.mode &&
					candidate.entryCount === result.entryCount &&
					candidate.preflightOp === result.preflightOp &&
					candidate.preflightCount === result.preflightCount &&
					!candidate.includeReaddir,
			);
			result.derived = {
				guestP50MinusPureReaddirMs: pureReaddir
					? round(result.guest.p50 - pureReaddir.guest.p50)
					: undefined,
				guestP50MinusPreflightOnlyMs: preflightOnly
					? round(result.guest.p50 - preflightOnly.guest.p50)
					: undefined,
				guestMsPerPreflightCall:
					result.preflightCount > 0
						? round(result.guest.p50 / result.preflightCount)
						: undefined,
			};
		}
		printTable(
			[
				"fixture",
				"workload",
				"preflight",
				"readdir",
				"mode",
				"entries",
				"host p50",
				"guest p50",
				"guest/host",
				"guest ms/entry",
				"guest delta empty",
				"returned",
			],
			cases.map((result) => [
				result.fixture,
				result.workload,
				`${result.preflightOp}:${result.preflightCount}`,
				result.includeReaddir ? "yes" : "no",
				result.mode,
				result.entryCount,
				`${result.host.p50}ms`,
				`${result.guest.p50}ms`,
				`${result.guestVsHostRatio}x`,
				`${result.msPerEntry.guest}ms`,
				result.deltaFromEmptyMs ? `${result.deltaFromEmptyMs.guest}ms` : "n/a",
				result.returnedCount.guest,
			]),
		);
		console.log(
			JSON.stringify(
				{
					benchmark: "readdir-scaling",
					hardware,
					iterations,
					warmup,
					fixtures,
					workloads: probeEnabled ? ["probe"] : workloads,
					preflightOps: probeEnabled ? preflightOps : undefined,
					preflightCounts: probeEnabled ? preflightCounts : undefined,
					includeReaddirs: probeEnabled ? includeReaddirs : undefined,
					cases,
				},
				null,
				2,
			),
		);
	} finally {
		await sidecar.dispose();
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
