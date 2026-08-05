import { existsSync, readFileSync } from "node:fs";
import { basename } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { allOps } from "./families/index.js";
import type { LatencyResult, LayerStatsEntry } from "./lib/layers.js";
import { getHardware, round } from "./lib/perf-utils.js";
import { writeJson } from "./lib/report.js";
import {
	formatPacificIso,
	formatSidecarProvenance,
	resolveBenchSidecarProvenance,
	type SidecarBinaryProvenance,
} from "./lib/vm.js";

export const LOCAL_BASELINE_PATH = fileURLToPath(
	new URL("../results/baseline-local.json", import.meta.url),
);
export const CI_BASELINE_PATH = fileURLToPath(
	new URL("../results/baseline-ci.json", import.meta.url),
);

export type GateLane =
	| "native"
	| "node"
	| "guest"
	| "wasm"
	| "hostCmd"
	| "vmCmd";

export interface MatrixBaseline {
	schemaVersion: 1;
	kind: "local" | "ci";
	generatedAt: string;
	hardware: ReturnType<typeof getHardware>;
	sidecar: SidecarBinaryProvenance;
	engine: {
		iterations: number;
		warmup: number;
		cold: boolean;
		sharedVm: boolean;
		rowCount: number;
	};
	rows: MatrixBaselineRow[];
}

export interface MatrixBaselineRow {
	key: string;
	family: string;
	op: string;
	lanes: Partial<Record<GateLane, BaselineLane>>;
	tax: Record<string, number>;
	skipped?: true;
	skipReason?: string;
}

export interface BaselineLane {
	p50Ms: number;
	memBytes?: number;
	memProvenance?: string;
}

export interface LatencyMatrixLike {
	results?: LatencyResult[];
	latency?: LatencyResult[];
	sidecar: SidecarBinaryProvenance;
	mode?: {
		cold: boolean;
		sharedVm: boolean;
	};
	matrixMode?: {
		cold: boolean;
		sharedVm: boolean;
	};
}

export function baselinePathForEnvironment(): string {
	return process.env.GITHUB_ACTIONS === "true" ? CI_BASELINE_PATH : LOCAL_BASELINE_PATH;
}

export function loadMatrixBaseline(path: string): MatrixBaseline | null {
	if (!existsSync(path)) return null;
	return JSON.parse(readFileSync(path, "utf8")) as MatrixBaseline;
}

export function rowKey(row: Pick<LatencyResult, "family" | "op">): string {
	return `${row.family}/${row.op}`;
}

export function laneMetric(
	row: Pick<MatrixBaselineRow, "lanes">,
	lane: GateLane,
): BaselineLane | undefined {
	return row.lanes[lane];
}

export function createMatrixBaseline(
	matrix: LatencyMatrixLike,
	options: {
		kind: "local" | "ci";
		iterations: number;
		warmup: number;
	},
): MatrixBaseline {
	const latency = matrix.results ?? matrix.latency ?? [];
	const mode = matrix.mode ?? matrix.matrixMode ?? { cold: false, sharedVm: false };
	return {
		schemaVersion: 1,
		kind: options.kind,
		generatedAt: formatPacificIso(new Date()),
		hardware: getHardware(),
		sidecar: matrix.sidecar,
		engine: {
			iterations: options.iterations,
			warmup: options.warmup,
			cold: mode.cold,
			sharedVm: mode.sharedVm,
			rowCount: latency.length,
		},
		rows: latency.map(baselineRowFromLatency),
	};
}

export function baselineRowFromLatency(result: LatencyResult): MatrixBaselineRow {
	const lanes: MatrixBaselineRow["lanes"] = {};
	for (const lane of ["native", "node", "guest", "wasm", "hostCmd", "vmCmd"] as const) {
		const stats = (result.layers as Partial<Record<GateLane, LayerStatsEntry>>)[lane];
		if (stats) lanes[lane] = baselineLane(stats);
	}
	return {
		key: rowKey(result),
		family: result.family,
		op: result.op,
		lanes,
		tax: Object.fromEntries(
			Object.entries(result.tax).filter((entry): entry is [string, number] =>
				typeof entry[1] === "number",
			),
		),
		...("skipped" in result && result.skipped ? { skipped: true as const } : {}),
		...("skipReason" in result && result.skipReason
			? { skipReason: result.skipReason }
			: {}),
	};
}

export function printBaselineMetadata(baseline: MatrixBaseline, path: string): void {
	console.log(
		JSON.stringify(
			{
				path,
				kind: baseline.kind,
				generatedAt: baseline.generatedAt,
				hardware: baseline.hardware,
				sidecar: baseline.sidecar,
				engine: baseline.engine,
			},
			null,
			2,
		),
	);
}

function baselineLane(stats: LayerStatsEntry): BaselineLane {
	return {
		p50Ms: stats.p50,
		...(stats.memBytes !== undefined ? { memBytes: stats.memBytes } : {}),
		...(stats.memProvenance ? { memProvenance: stats.memProvenance } : {}),
	};
}

function parseArgs(argv: string[]): {
	kind: "local" | "ci";
	output: string;
	from?: string;
} {
	let kind: "local" | "ci" = "local";
	let output: string | undefined;
	let from: string | undefined;
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		if (arg === "--") {
			// pnpm run forwards the `--` separator itself; ignore it.
			continue;
		}
		if (arg === "--ci") {
			kind = "ci";
		} else if (arg === "--local") {
			kind = "local";
		} else if (arg === "--output") {
			output = argv[++i];
		} else if (arg === "--from") {
			from = argv[++i];
		} else {
			throw new Error(`unknown ${basename(import.meta.url)} argument: ${arg}`);
		}
	}
	return {
		kind,
		output: output ?? (kind === "ci" ? CI_BASELINE_PATH : LOCAL_BASELINE_PATH),
		from,
	};
}

async function loadOrRunMatrix(from: string | undefined): Promise<LatencyMatrixLike> {
	if (from) {
		return JSON.parse(readFileSync(from, "utf8")) as LatencyMatrixLike;
	}
	if (process.env.BENCH_FAMILIES || process.env.BENCH_OP_FILTER) {
		throw new Error("baseline regeneration requires the full matrix; unset BENCH_FAMILIES and BENCH_OP_FILTER");
	}
	const sidecar = resolveBenchSidecarProvenance();
	console.error(formatSidecarProvenance(sidecar));
	if (sidecar.profile !== "release") {
		throw new BaselineExitError(
			2,
			`refusing to regenerate baseline with ${sidecar.profile} sidecar; set AGENTOS_SIDECAR_BIN to target/release/agentos-sidecar`,
		);
	}
	const { runLatencyMatrix } = await import("./run-all.js");
	return runLatencyMatrix();
}

class BaselineExitError extends Error {
	constructor(
		readonly code: number,
		message: string,
	) {
		super(message);
	}
}

async function main(): Promise<void> {
	const args = parseArgs(process.argv.slice(2));
	const matrix = await loadOrRunMatrix(args.from);
	if (matrix.sidecar.profile !== "release") {
		throw new BaselineExitError(
			2,
			`refusing to write ${args.output} from ${matrix.sidecar.profile} sidecar provenance`,
		);
	}
	const baseline = createMatrixBaseline(matrix, {
		kind: args.kind,
		iterations: Number(process.env.BENCH_ITERATIONS ?? 20),
		warmup: Number(process.env.BENCH_WARMUP ?? 5),
	});
	// One matrix row per registered op; derive from the registry so the check
	// tracks new families instead of going stale on a hardcoded count.
	if (baseline.engine.rowCount !== allOps.length) {
		throw new BaselineExitError(
			2,
			`refusing to write incomplete baseline: expected ${allOps.length} matrix rows, got ${baseline.engine.rowCount}`,
		);
	}
	writeJson(args.output, baseline);
	printBaselineMetadata(baseline, args.output);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
	main().then(
		() => process.exit(0),
		(error) => {
			const code = error instanceof BaselineExitError ? error.code : 1;
			console.error(error instanceof Error ? error.message : error);
			process.exit(code);
		},
	);
}
