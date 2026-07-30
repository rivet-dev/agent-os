import type { NodeRuntimeCreateOptions } from "@rivet-dev/agentos-core";
import { fsFamily } from "./fs.js";
import { netFamily } from "./net.js";
import type { BenchmarkOp } from "../lib/layers.js";

const PERMISSION_OPS = [
	...selectOps(fsFamily, ["fs_write_small", "stat_storm", "readdir_small"]),
	...selectOps(netFamily, ["tcp_echo_small", "http_loopback_get"]),
];

// Policy shape under test:
// - fs: default deny, then 15 allow glob rules. Common runtime paths appear
//   first; benchmark script and op working paths are last-matching for their
//   requests so the last-match evaluator walks the list.
// - network: default deny, then 5 allowlist rules. The real loopback rule is
//   last so TCP connect/listen checks walk the list before allowing.
// - childProcess/process/env: allow, keeping the benchmark focused on the
//   guest syscall permission matcher for fs and network operations.
export const restrictivePermissionsPolicy: NodeRuntimeCreateOptions["permissions"] = {
	fs: {
		default: "deny",
		rules: [
			{ mode: "allow", operations: ["*"], paths: ["/bin/**"] },
			{ mode: "allow", operations: ["*"], paths: ["/usr/**"] },
			{ mode: "allow", operations: ["*"], paths: ["/lib/**"] },
			{ mode: "allow", operations: ["*"], paths: ["/etc/**"] },
			{ mode: "allow", operations: ["*"], paths: ["/dev/**"] },
			{ mode: "allow", operations: ["*"], paths: ["/proc/**"] },
			{ mode: "allow", operations: ["*"], paths: ["/workspace/**"] },
			{ mode: "allow", operations: ["*"], paths: ["/mnt/**"] },
			{ mode: "allow", operations: ["*"], paths: ["/tmp"] },
			{ mode: "allow", operations: ["*"], paths: ["/tmp/*.mjs"] },
			{ mode: "allow", operations: ["*"], paths: ["/tmp/fuzz-perf-permissions-*.mjs"] },
			{ mode: "allow", operations: ["*"], paths: ["/tmp/fuzz-perf-write.txt"] },
			{ mode: "allow", operations: ["*"], paths: ["/tmp/fuzz-perf-stat.txt"] },
			{ mode: "allow", operations: ["*"], paths: ["/tmp/fuzz-perf-readdir-*"] },
			{ mode: "allow", operations: ["*"], paths: ["/tmp/fuzz-perf-readdir-*/**"] },
		],
	},
	network: {
		default: "deny",
		rules: [
			{ mode: "allow", operations: ["http", "listen"], patterns: ["tcp://127.0.0.2:*"] },
			{ mode: "allow", operations: ["http", "listen"], patterns: ["tcp://localhost:*"] },
			{ mode: "allow", operations: ["http", "listen"], patterns: ["tcp://[::1]:*"] },
			{ mode: "allow", operations: ["http", "listen"], patterns: ["tcp://0.0.0.0:*"] },
			{ mode: "allow", operations: ["http", "listen"], patterns: ["tcp://127.0.0.1:*"] },
		],
	},
	childProcess: "allow",
	process: "allow",
	env: "allow",
};

export const permissionsFamily: BenchmarkOp[] = PERMISSION_OPS.flatMap((op) => [
	permissionRow(op, "allow"),
	permissionRow(op, "policy"),
]);

function selectOps(family: BenchmarkOp[], names: string[]): BenchmarkOp[] {
	return names.map((name) => {
		const op = family.find((candidate) => candidate.name === name);
		if (!op) {
			throw new Error(`missing benchmark op ${name}`);
		}
		return op;
	});
}

function permissionRow(op: BenchmarkOp, variant: "allow" | "policy"): BenchmarkOp {
	return {
		...op,
		family: "permissions",
		name: `${op.name}_${variant}`,
		nativeOp: undefined,
		nativeUnsupportedReason: "permissions family measures guest VM policy overhead only",
		wasmUnsupportedReason: "permissions family measures guest VM policy overhead only",
		reproducer: `${op.reproducer}; permissions=${variant}`,
		prepareVm:
			variant === "policy"
				? async () => ({ options: { permissions: restrictivePermissionsPolicy } })
				: undefined,
	};
}
