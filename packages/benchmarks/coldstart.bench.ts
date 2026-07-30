/** Cold and warm execution latency through the public AgentOS language execution API. */

import { AgentOs, type AgentOsSidecar } from "@rivet-dev/agentos";
import {
	EXEC_TIMEOUT_MS,
	getHardware,
	ITERATIONS,
	stats,
	TRIVIAL_CODE,
	WARMUP_ITERATIONS,
} from "./bench-utils.js";

type Scenario = "fresh-sidecar" | "shared-sidecar";

interface Measurement {
	runtimeCreateMs: number;
	firstExecuteMs: number;
	warmExecuteMs: number;
	coldTotalMs: number;
}

const scenarios: Scenario[] = (
	process.env.BENCH_SCENARIOS?.split(",").map((value) => value.trim()) ?? [
		"fresh-sidecar",
		"shared-sidecar",
	]
).filter(
	(value): value is Scenario =>
		value === "fresh-sidecar" || value === "shared-sidecar",
);

async function measure(scenario: Scenario): Promise<Measurement> {
	let sidecar: AgentOsSidecar | undefined;
	const startedAt = performance.now();
	if (scenario === "fresh-sidecar") sidecar = await AgentOs.createSidecar();
	const runtime = await AgentOs.create(
		sidecar
			? { sidecar: { kind: "explicit", handle: sidecar } }
			: { sidecar: { kind: "shared", pool: "exec-benchmark" } },
	);
	const runtimeCreateMs = performance.now() - startedAt;

	try {
		const firstStartedAt = performance.now();
		const first = await runtime.javascript.execute(TRIVIAL_CODE, {
			timeoutMs: EXEC_TIMEOUT_MS,
		});
		const firstExecuteMs = performance.now() - firstStartedAt;
		if (first.outcome !== "succeeded") {
			throw new Error(
				first.stderr || `first execution exited ${first.exitCode}`,
			);
		}

		const warmStartedAt = performance.now();
		const warm = await runtime.javascript.execute(TRIVIAL_CODE, {
			timeoutMs: EXEC_TIMEOUT_MS,
		});
		const warmExecuteMs = performance.now() - warmStartedAt;
		if (warm.outcome !== "succeeded") {
			throw new Error(warm.stderr || `warm execution exited ${warm.exitCode}`);
		}

		return {
			runtimeCreateMs,
			firstExecuteMs,
			warmExecuteMs,
			coldTotalMs: runtimeCreateMs + firstExecuteMs,
		};
	} finally {
		await runtime.dispose();
		await sidecar?.dispose();
	}
}

async function main(): Promise<void> {
	const hardware = getHardware();
	const results = [];

	console.error("=== AgentOS language execution Cold Start Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`Cores: ${hardware.cores} | RAM: ${hardware.ram}`);
	console.error(`Node: ${hardware.node}`);
	console.error(`Iterations: ${ITERATIONS} (+ ${WARMUP_ITERATIONS} warmup)`);
	console.error(`Scenarios: ${scenarios.join(", ")}`);

	for (const scenario of scenarios) {
		const measurements: Measurement[] = [];
		for (let index = 0; index < WARMUP_ITERATIONS + ITERATIONS; index++) {
			const measurement = await measure(scenario);
			if (index >= WARMUP_ITERATIONS) measurements.push(measurement);
		}
		const result = {
			scenario,
			runtimeCreate: stats(measurements.map((value) => value.runtimeCreateMs)),
			firstExecute: stats(measurements.map((value) => value.firstExecuteMs)),
			warmExecute: stats(measurements.map((value) => value.warmExecuteMs)),
			coldTotal: stats(measurements.map((value) => value.coldTotalMs)),
		};
		results.push(result);
		console.error(
			`${scenario}: cold p50=${result.coldTotal.p50}ms, warm p50=${result.warmExecute.p50}ms`,
		);
	}

	console.log(JSON.stringify({ hardware, results }, null, 2));
}

main().catch((error) => {
	console.error(error);
	process.exitCode = 1;
});
