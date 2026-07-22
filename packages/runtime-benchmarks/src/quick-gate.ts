import { existsSync } from "node:fs";
import { pathToFileURL } from "node:url";
import {
	baselinePathForEnvironment,
	baselineRowFromLatency,
	type GateLane,
	loadMatrixBaseline,
} from "./baseline.js";
import { compareMatrixBaseline, type GateRow } from "./compare-baseline.js";
import { printTable } from "./lib/perf-utils.js";
import {
	formatSidecarProvenance,
	resolveBenchSidecarProvenance,
} from "./lib/vm.js";

const GATE_DEFAULTS = {
	iterations: 9,
	warmup: 3,
	threshold: 2.0,
	tinyBaselineFloorMs: 0.5,
	tinyAllowedRegressionMs: 1.0,
	rows: [
		{
			key: "fs/fs_write_small",
			lane: "guest",
			reason:
				"tiny sync write hot path; protected by the 1ms absolute-regression allowance",
		},
		{
			key: "fs/fs_write_big",
			lane: "guest",
			reason: "large write payload catches whole-buffer copy regressions",
		},
		{
			key: "fs/fs_read_small",
			lane: "guest",
			reason: "small read bridge/VFS floor",
		},
		{
			key: "fs/stat_storm",
			lane: "guest",
			reason: "metadata syscall hot path",
		},
		{
			key: "fs/readdir_small",
			lane: "guest",
			reason:
				"directory enumeration without the high variance of large listings",
		},
		{
			key: "net/tcp_connect_close",
			lane: "guest",
			reason: "TCP socket lifecycle floor",
		},
		{
			key: "net/tcp_echo_small",
			lane: "guest",
			reason: "small TCP payload round trip",
		},
		{
			key: "net/http_loopback_get",
			lane: "guest",
			reason: "HTTP over kernel sockets with loopback server",
		},
		{
			key: "modules/import_fresh_file",
			lane: "guest",
			reason: "dynamic import and filesystem resolution",
		},
		{
			key: "modules/require_100_small",
			lane: "guest",
			reason: "CommonJS resolver/cache behavior",
		},
		{
			key: "control/cpu_loop",
			lane: "wasm",
			reason: "WASM runtime lane sanity check",
		},
		{
			key: "ecosystem/ls_100",
			lane: "vmCmd",
			reason: "end-to-end WASM command tier",
		},
	] satisfies Array<GateRow & { reason: string }>,
};

class GateExitError extends Error {
	constructor(
		readonly code: number,
		message: string,
	) {
		super(message);
	}
}

async function main(): Promise<void> {
	const baselinePath = baselinePathForEnvironment();
	if (process.env.GITHUB_ACTIONS === "true" && !existsSync(baselinePath)) {
		console.error(
			`BENCH GATE SKIPPED: ${baselinePath} is missing. Run the nightly benchmark, download its candidate baseline-ci.json artifact, commit it, and PR gates will enforce from then on.`,
		);
		return;
	}
	const baseline = loadMatrixBaseline(baselinePath);
	if (!baseline) {
		throw new GateExitError(
			2,
			`BENCH GATE REFUSED: missing baseline ${baselinePath}. Regenerate with pnpm --dir packages/benchmarks bench:baseline.`,
		);
	}

	const sidecar = resolveBenchSidecarProvenance();
	console.error(formatSidecarProvenance(sidecar));
	if (sidecar.profile !== "release") {
		throw new GateExitError(
			2,
			`BENCH GATE REFUSED: sidecar provenance profile is ${sidecar.profile}; set AGENTOS_SIDECAR_BIN to target/release/agentos-native-sidecar`,
		);
	}

	const gateRows = selectedGateRows();
	process.env.BENCH_OP_FILTER = gateRows.map((row) => row.key).join(",");
	process.env.BENCH_ITERATIONS ??= String(GATE_DEFAULTS.iterations);
	process.env.BENCH_WARMUP ??= String(GATE_DEFAULTS.warmup);
	process.env.BENCH_REQUIRED_WASM_COMMANDS ??=
		requiredWasmCommands(gateRows).join(",");

	const { runLatencyMatrix } = await import("./run-all.js");
	const matrix = await runLatencyMatrix();
	const currentRows = matrix.results.map(baselineRowFromLatency);
	const threshold = Number(
		process.env.BENCH_GATE_THRESHOLD ?? GATE_DEFAULTS.threshold,
	);
	const comparisons = compareMatrixBaseline(currentRows, baseline, gateRows, {
		threshold,
		tinyBaselineFloorMs: GATE_DEFAULTS.tinyBaselineFloorMs,
		tinyAllowedRegressionMs: GATE_DEFAULTS.tinyAllowedRegressionMs,
	});

	console.error(
		`Bench gate baseline: ${baselinePath}; threshold > ${threshold}x; tiny rows baseline < ${GATE_DEFAULTS.tinyBaselineFloorMs}ms ignore absolute regressions < ${GATE_DEFAULTS.tinyAllowedRegressionMs}ms`,
	);
	printTable(
		["row", "lane", "baseline p50", "current p50", "ratio", "status", "reason"],
		comparisons.map((row) => [
			row.key,
			row.lane,
			row.baselineP50Ms === undefined ? "-" : `${row.baselineP50Ms}ms`,
			row.currentP50Ms === undefined ? "-" : `${row.currentP50Ms}ms`,
			row.ratio === undefined ? "-" : `${row.ratio}x`,
			row.status.toUpperCase(),
			row.reason,
		]),
	);

	const failures = comparisons.filter((row) =>
		["fail", "missing-baseline", "missing-current"].includes(row.status),
	);
	if (failures.length > 0) {
		throw new GateExitError(
			1,
			`BENCH GATE FAILED: ${failures.length} row(s) exceeded threshold or were missing`,
		);
	}
	console.error("BENCH GATE PASSED");
}

function selectedGateRows(): GateRow[] {
	const override = process.env.BENCH_GATE_ROWS;
	if (!override) return GATE_DEFAULTS.rows;
	return override
		.split(",")
		.map((entry) => entry.trim())
		.filter(Boolean)
		.map((entry) => {
			const [key, lane] = entry.split(":");
			const configured = GATE_DEFAULTS.rows.find((row) => row.key === key);
			return {
				key,
				lane: (lane ?? configured?.lane ?? "guest") as GateLane,
			};
		});
}

function requiredWasmCommands(gateRows: GateRow[]): string[] {
	const commands = new Set<string>();
	for (const row of gateRows) {
		if (row.key === "ecosystem/ls_100") {
			commands.add("ls");
		}
	}
	return [...commands];
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
	main().then(
		() => process.exit(0),
		(error) => {
			const code = error instanceof GateExitError ? error.code : 1;
			console.error(error instanceof Error ? error.message : error);
			process.exit(code);
		},
	);
}
