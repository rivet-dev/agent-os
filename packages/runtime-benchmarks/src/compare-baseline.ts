import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import {
	baselineRowFromLatency,
	type GateLane,
	laneMetric,
	loadMatrixBaseline,
	type MatrixBaseline,
	type MatrixBaselineRow,
} from "./baseline.js";
import type { LatencyResult } from "./lib/layers.js";
import { round } from "./lib/perf-utils.js";

export interface GateRow {
	key: string;
	lane: GateLane;
}

export interface MatrixComparison {
	key: string;
	lane: GateLane;
	baselineP50Ms?: number;
	currentP50Ms?: number;
	ratio?: number;
	deltaMs?: number;
	status: "pass" | "fail" | "ignored" | "missing-baseline" | "missing-current";
	reason: string;
}

export function compareMatrixBaseline(
	currentRows: MatrixBaselineRow[],
	baseline: MatrixBaseline,
	gateRows: GateRow[],
	options: {
		threshold: number;
		tinyBaselineFloorMs: number;
		tinyAllowedRegressionMs: number;
	},
): MatrixComparison[] {
	const currentByKey = new Map(currentRows.map((row) => [row.key, row]));
	const baselineByKey = new Map(baseline.rows.map((row) => [row.key, row]));
	return gateRows.map((gate) => {
		const current = currentByKey.get(gate.key);
		const base = baselineByKey.get(gate.key);
		const currentMetric = current ? laneMetric(current, gate.lane) : undefined;
		const baselineMetric = base ? laneMetric(base, gate.lane) : undefined;
		if (!base || !baselineMetric) {
			return {
				key: gate.key,
				lane: gate.lane,
				currentP50Ms: currentMetric?.p50Ms,
				status: "missing-baseline",
				reason: "baseline has no p50 for this row/lane",
			};
		}
		if (!current || !currentMetric) {
			return {
				key: gate.key,
				lane: gate.lane,
				baselineP50Ms: baselineMetric.p50Ms,
				status: "missing-current",
				reason: "current run has no p50 for this row/lane",
			};
		}
		const ratio = round(currentMetric.p50Ms / baselineMetric.p50Ms, 2);
		const deltaMs = round(currentMetric.p50Ms - baselineMetric.p50Ms, 3);
		const tinyBaseline = baselineMetric.p50Ms < options.tinyBaselineFloorMs;
		if (tinyBaseline && deltaMs < options.tinyAllowedRegressionMs) {
			return {
				key: gate.key,
				lane: gate.lane,
				baselineP50Ms: baselineMetric.p50Ms,
				currentP50Ms: currentMetric.p50Ms,
				ratio,
				deltaMs,
				status: "ignored",
				reason: `baseline < ${options.tinyBaselineFloorMs}ms and absolute regression < ${options.tinyAllowedRegressionMs}ms`,
			};
		}
		const failed = ratio > options.threshold;
		return {
			key: gate.key,
			lane: gate.lane,
			baselineP50Ms: baselineMetric.p50Ms,
			currentP50Ms: currentMetric.p50Ms,
			ratio,
			deltaMs,
			status: failed ? "fail" : "pass",
			reason: failed
				? `ratio ${ratio} > ${options.threshold}`
				: `ratio ${ratio} <= ${options.threshold}`,
		};
	});
}

export function latencyResultsToBaselineRows(
	results: LatencyResult[],
): MatrixBaselineRow[] {
	return results.map(baselineRowFromLatency);
}

export function compareBaselineFile(
	currentPath: string,
	baselinePath: string,
	gateRows: GateRow[] = [],
	options: {
		threshold: number;
		tinyBaselineFloorMs: number;
		tinyAllowedRegressionMs: number;
	} = {
		threshold: 2,
		tinyBaselineFloorMs: 0.5,
		tinyAllowedRegressionMs: 1,
	},
) {
	const baseline = loadMatrixBaseline(baselinePath);
	if (!baseline) {
		return {
			status: "missing-baseline",
			baselinePath,
			rows: [],
		};
	}
	const currentJson = JSON.parse(readFileSync(currentPath, "utf8")) as {
		latency?: LatencyResult[];
		rows?: MatrixBaselineRow[];
	};
	const currentRows =
		currentJson.rows ?? latencyResultsToBaselineRows(currentJson.latency ?? []);
	const rows = compareMatrixBaseline(currentRows, baseline, gateRows, options);
	return {
		status: rows.some((row) => row.status === "fail") ? "regressed" : "ok",
		rows,
	};
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
	const current =
		process.argv[2] ?? "packages/benchmarks/results/latency-matrix.json";
	const baseline =
		process.argv[3] ?? "packages/benchmarks/results/baseline-local.json";
	const rowsArg = process.argv[4] ?? "";
	const gateRows = rowsArg
		.split(",")
		.map((row) => row.trim())
		.filter(Boolean)
		.map((row) => {
			const [key, lane = "guest"] = row.split(":");
			return { key, lane: lane as GateLane };
		});
	console.log(
		JSON.stringify(
			compareBaselineFile(current, baseline, gateRows, {
				threshold: 2,
				tinyBaselineFloorMs: 0.5,
				tinyAllowedRegressionMs: 1,
			}),
			null,
			2,
		),
	);
}
