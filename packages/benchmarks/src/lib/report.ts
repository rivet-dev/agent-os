import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { OpResult } from "./layers.js";
import { round } from "./perf-utils.js";

export interface Finding {
	op: string;
	family: string;
	emulation_ratio: number;
	total_ratio: number;
	confirmed: boolean;
	suspected_cause: string;
	file_line: string;
	reproducer: string;
	evidence: string;
}

export interface RefutedCandidate {
	op: string;
	family: string;
	reason: string;
	evidence: string;
}

export interface PermissionPolicyTax {
	op: string;
	allowP50Ms: number;
	policyP50Ms: number;
	policyTax: number;
}

export function ensureDir(path: string): void {
	mkdirSync(path, { recursive: true });
}

export function writeJson(path: string, value: unknown): void {
	ensureDir(dirname(path));
	writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

export function findingsFromLatency(results: OpResult[]): Finding[] {
	return [...results]
		.filter((result) => isLatencyFinding(result))
		.sort((a, b) => b.tax.emulation - a.tax.emulation)
		.map((result) => ({
			op: result.op,
			family: result.family,
			emulation_ratio: result.tax.emulation,
			total_ratio: result.tax.total ?? 0,
			confirmed: true,
			suspected_cause: causeFor(result.family, result.op),
			file_line: result.fileLine,
			reproducer: result.reproducer,
			evidence: `p50 native=${result.layers.native ? `${result.layers.native.p50}ms` : `unsupported (${result.unsupported?.native ?? "n/a"})`} node=${result.layers.node.p50}ms guest=${result.layers.guest.p50}ms`,
		}));
}

export function refutedFromLatency(results: OpResult[]): RefutedCandidate[] {
	return [...results]
		.filter((result) => !isLatencyFinding(result))
		.map((result) => ({
			op: result.op,
			family: result.family,
			reason:
				result.expectedRatio === "control"
					? "control workload stayed within the methodology guardrail"
					: "emulation tax stayed below the confirmed-offender threshold",
			evidence: `guest/node=${result.tax.emulation}; p50 native=${result.layers.native ? `${result.layers.native.p50}ms` : `unsupported (${result.unsupported?.native ?? "n/a"})`} node=${result.layers.node.p50}ms guest=${result.layers.guest.p50}ms`,
		}));
}

export function permissionPolicyTaxFromLatency(
	results: OpResult[],
): PermissionPolicyTax[] {
	const byOp = new Map<string, { allow?: OpResult; policy?: OpResult }>();
	for (const result of results) {
		if (result.family !== "permissions") continue;
		const match = /^(.*)_(allow|policy)$/.exec(result.op);
		if (!match) continue;
		const [, op, variant] = match;
		const pair = byOp.get(op) ?? {};
		pair[variant as "allow" | "policy"] = result;
		byOp.set(op, pair);
	}
	return [...byOp.entries()]
		.filter((entry): entry is [string, { allow: OpResult; policy: OpResult }] =>
			Boolean(entry[1].allow && entry[1].policy),
		)
		.map(([op, pair]) => ({
			op,
			allowP50Ms: pair.allow.layers.guest.p50,
			policyP50Ms: pair.policy.layers.guest.p50,
			policyTax: round(
				pair.policy.layers.guest.p50 / pair.allow.layers.guest.p50,
			),
		}));
}

export function permissionPolicyFindings(
	rows: PermissionPolicyTax[],
): Finding[] {
	return rows
		.filter((row) => row.policyTax > 1.2)
		.map((row) => ({
			op: row.op,
			family: "permissions",
			emulation_ratio: row.policyTax,
			total_ratio: row.policyTax,
			confirmed: true,
			suspected_cause:
				"permission matcher rule-walk overhead on a hot guest syscall path",
			file_line: "archive/browser/crates/sidecar-core/src/permissions.rs:80",
			reproducer: `BENCH_FAMILIES=permissions BENCH_OP_FILTER=${row.op}_allow,${row.op}_policy pnpm --dir packages/benchmarks bench:matrix`,
			evidence: `policyTax=${row.policyTax}; allow p50=${row.allowP50Ms}ms policy p50=${row.policyP50Ms}ms`,
		}));
}

export function compareBaseline(currentPath: string, baselinePath: string) {
	const current = JSON.parse(readFileSync(currentPath, "utf8")) as {
		findings: Finding[];
	};
	const baseline = JSON.parse(readFileSync(baselinePath, "utf8")) as {
		findings: Finding[];
	};
	const byKey = new Map(
		baseline.findings.map((finding) => [
			`${finding.family}/${finding.op}`,
			finding,
		]),
	);
	return current.findings.map((finding) => {
		const base = byKey.get(`${finding.family}/${finding.op}`);
		return {
			family: finding.family,
			op: finding.op,
			current_emulation_ratio: finding.emulation_ratio,
			baseline_emulation_ratio: base?.emulation_ratio ?? null,
			regressed:
				base !== undefined &&
				finding.emulation_ratio > base.emulation_ratio * 1.1,
		};
	});
}

function causeFor(family: string, op: string): string {
	if (family === "process") {
		return "fresh V8 isolate/thread per spawned process plus process-table polling";
	}
	if (family === "dns") {
		return "per-lookup resolver/runtime setup and missing DNS result cache";
	}
	if (family === "net") {
		return "global socket table mutex and byte-buffer cloning in virtual socket I/O";
	}
	if (family === "fs") {
		return "single sync filesystem bridge/VFS round trip floor on tiny filesystem operations";
	}
	if (family === "modules") {
		return "module resolution, source loading, and import cache behavior in the guest JavaScript runtime";
	}
	if (family === "pipes") {
		return "stdio pipe bytes cross synchronous bridge boundaries";
	}
	if (op.includes("alloc")) {
		return "control workload; should remain near host ratio";
	}
	return "control workload; expected to validate measurement overhead";
}

function isLatencyFinding(result: OpResult): boolean {
	if (result.expectedRatio === "control") {
		return result.tax.emulation > 1.5;
	}
	return result.tax.emulation > 2;
}
