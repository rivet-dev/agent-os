import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { allOps } from "./families/index.js";

const DEFAULT_NATIVE_BIN = join(
	fileURLToPath(new URL("../../..", import.meta.url)),
	"target/release/agentos-benchmark-baseline",
);

function nativeBaselineBin(): string {
	return process.env.NATIVE_BASELINE_BIN ?? DEFAULT_NATIVE_BIN;
}

function listNativeOps(bin: string): Set<string> {
	if (!existsSync(bin)) {
		throw new Error(`native-baseline binary not found at ${bin}; build with cargo build --release -p agentos-benchmark-baseline`);
	}
	const stdout = execFileSync(bin, ["--list-ops"], {
		encoding: "utf8",
		maxBuffer: 1024 * 1024,
	});
	const ops = stdout
		.split(/\r?\n/)
		.map((line) => line.trim())
		.filter(Boolean);
	if (ops.length === 0) {
		throw new Error("native-baseline --list-ops returned no ops");
	}
	return new Set(ops);
}

const nativeOps = listNativeOps(nativeBaselineBin());
const failures: string[] = [];

for (const op of allOps) {
	if ("runHostCmd" in op) continue;
	const key = `${op.family}/${op.name}`;
	if (op.nativeOp) {
		if (!nativeOps.has(op.nativeOp)) {
			failures.push(`${key}: nativeOp ${op.nativeOp} is not in native-baseline --list-ops`);
		}
		continue;
	}
	if (!op.nativeUnsupportedReason) {
		failures.push(`${key}: missing nativeOp and nativeUnsupportedReason`);
	}
}

if (failures.length > 0) {
	console.error("native op drift check failed:");
	for (const failure of failures) console.error(`- ${failure}`);
	process.exit(1);
}

console.log(`native op drift check passed: ${nativeOps.size} native-baseline ops advertised`);
